//! Remote screen viewer: shows the decoded frames and forwards mouse/keyboard/files.

use remote_friend_common::{FileChunk, InputEvent, MouseButton, Packet, RemoteKey};
use std::io::Read as _;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{history, App};

impl App {
    pub(crate) fn viewer_ui(&mut self, ctx: &egui::Context) {
        let shared = self.shared.clone().unwrap();
        let tx = self.tx_out.clone().unwrap();
        let (new_frame, status, hw, hh, fps) = {
            let mut s = shared.lock().unwrap();
            // Take ownership of the frame instead of cloning it (a 1080p RGBA frame is ~8 MiB).
            (s.texture.take(), s.status.clone(), s.host_w, s.host_h, s.fps)
        };

        // Update the GPU texture only when a new frame arrived.
        if let Some((img, _w, _h)) = new_frame {
            if !self.thumb_saved {
                if let Some(fname) = history::save_thumb(&self.current_addr, &img) {
                    if let Some(e) = self.recents.iter_mut().find(|e| e.addr == self.current_addr) {
                        e.thumb = Some(fname);
                        history::save_recents(&self.recents);
                    }
                }
                self.thumb_saved = true;
            }

            let [iw, ih] = img.size;
            let same = self.screen_tex.as_ref().is_some_and(|t| t.size() == [iw, ih]);
            if same {
                if let Some(tex) = self.screen_tex.as_mut() {
                    tex.set(img, egui::TextureOptions::LINEAR);
                }
            } else {
                self.screen_tex = Some(ctx.load_texture(
                    "screen",
                    img,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("✖ Disconnect").clicked() {
                    self.disconnect();
                    return;
                }
                ui.label(format!("{status}  ·  {hw}×{hh}  ·  {fps:.0} fps"));
                if ui.button("📁 Send file").clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_file() {
                        self.send_file(path);
                    }
                }
            });
        });

        // Trust on first use: an unknown server is confirmed by its fingerprint once.
        if let Some((srv, fp)) = shared.lock().unwrap().tofu_pending.clone() {
            egui::Window::new("First connection to this server")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Server: {srv}"));
                    ui.add_space(4.0);
                    ui.monospace(&fp);
                    ui.add_space(4.0);
                    ui.small("This is normal the first time. It must match the fingerprint shown at the end of the server setup; it will be remembered.");
                    ui.horizontal(|ui| {
                        if ui.button("Trust and connect").clicked() {
                            shared.lock().unwrap().tofu_answer = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            shared.lock().unwrap().tofu_answer = Some(false);
                        }
                    });
                });
        }

        let failed = shared.lock().unwrap().failed;

        // Cloning a TextureHandle only clones a reference, not the pixels.
        let display_tex = self.screen_tex.clone();
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(tex) = display_tex.as_ref() {
                let avail = ui.available_size();
                let img_size = tex.size_vec2();
                let scale = (avail.x / img_size.x).min(avail.y / img_size.y).max(0.1);
                let disp = img_size * scale;
                let (rect, resp) = ui.allocate_exact_size(disp, egui::Sense::click_and_drag());

                ui.painter().image(
                    tex.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );

                let to_host = |pos: egui::Pos2| -> Option<(u32, u32)> {
                    if hw == 0 || hh == 0 { return None; }
                    let p = pos - rect.min;
                    if p.x < 0.0 || p.y < 0.0 || p.x > rect.width() || p.y > rect.height() {
                        return None;
                    }
                    Some((
                        (p.x / rect.width() * hw as f32) as u32,
                        (p.y / rect.height() * hh as f32) as u32,
                    ))
                };

                if let Some(pos) = resp.hover_pos() {
                    if let Some((x, y)) = to_host(pos) {
                        if self.last_mouse != Some((x, y)) {
                            self.last_mouse = Some((x, y));
                            let _ = tx.try_send(Packet::Input(InputEvent::MouseMove { x, y }));
                        }
                    }
                }
                let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
                for ev in events {
                    match &ev {
                        // Real press/release: drag & drop, selection and double click work.
                        egui::Event::PointerButton { pos, button, pressed, .. } => {
                            let btn = match button {
                                egui::PointerButton::Primary => MouseButton::Left,
                                egui::PointerButton::Secondary => MouseButton::Right,
                                egui::PointerButton::Middle => MouseButton::Middle,
                                _ => continue,
                            };
                            if *pressed {
                                let Some((x, y)) = to_host(*pos) else { continue };
                                let _ = tx.try_send(Packet::Input(InputEvent::MouseMove { x, y }));
                                let _ = tx.try_send(Packet::Input(InputEvent::MouseDown { button: btn }));
                                self.buttons_down[btn_index(btn)] = true;
                            } else if self.buttons_down[btn_index(btn)] {
                                if let Some((x, y)) = to_host(*pos) {
                                    let _ = tx.try_send(Packet::Input(InputEvent::MouseMove { x, y }));
                                }
                                let _ = tx.try_send(Packet::Input(InputEvent::MouseUp { button: btn }));
                                self.buttons_down[btn_index(btn)] = false;
                            }
                        }
                        egui::Event::Text(t) => {
                            for c in t.chars() {
                                if c.is_control() { continue; }
                                self.send_key(RemoteKey::Char(c), true);
                                self.send_key(RemoteKey::Char(c), false);
                            }
                        }
                        egui::Event::Key { key, pressed, repeat, modifiers, .. } => {
                            if *repeat { continue; }
                            if let Some(rk) = egui_key_to_remote(*key) {
                                self.send_key(rk, *pressed);
                            } else {
                                // egui emits no text events while Ctrl/Alt is held: send Ctrl+C etc. here.
                                // Releases are always sent, even if the modifier is already up (no stuck keys).
                                let name = key.symbol_or_name();
                                let mut chars = name.chars();
                                if let (Some(c), None) = (chars.next(), chars.next()) {
                                    let c = c.to_ascii_lowercase();
                                    if *pressed && (modifiers.ctrl || modifiers.alt || modifiers.mac_cmd) {
                                        self.chars_down.insert(c);
                                        self.send_key(RemoteKey::Char(c), true);
                                    } else if !*pressed && self.chars_down.remove(&c) {
                                        self.send_key(RemoteKey::Char(c), false);
                                    }
                                }
                            }
                        }
                        egui::Event::MouseWheel { unit, delta, .. } => {
                            if resp.hover_pos().is_some() {
                                let mult = match unit {
                                    egui::MouseWheelUnit::Point => 1.0 / 50.0,
                                    egui::MouseWheelUnit::Line => 1.0,
                                    egui::MouseWheelUnit::Page => 10.0,
                                };
                                self.scroll_acc.0 += delta.x * mult;
                                self.scroll_acc.1 += delta.y * mult;
                                let dx = self.scroll_acc.0.trunc() as i32;
                                let dy = self.scroll_acc.1.trunc() as i32;
                                if dx != 0 || dy != 0 {
                                    self.scroll_acc.0 -= dx as f32;
                                    self.scroll_acc.1 -= dy as f32;
                                    let _ = tx.try_send(Packet::Input(InputEvent::Scroll { dx, dy }));
                                }
                            }
                        }
                        _ => {}
                    }
                }

                let (mshift, mctrl, malt, mmeta) = ctx.input(|i| {
                    let m = &i.modifiers;
                    (m.shift, m.ctrl, m.alt, m.mac_cmd || (m.command && !m.ctrl))
                });
                self.sync_mod(RemoteKey::Shift, mshift);
                self.sync_mod(RemoteKey::Ctrl, mctrl);
                self.sync_mod(RemoteKey::Alt, malt);
                self.sync_mod(RemoteKey::Meta, mmeta);
            } else if failed {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading("Could not connect");
                        ui.label(&status);
                        ui.add_space(8.0);
                        if ui.button("⟵ Back").clicked() {
                            self.disconnect();
                        }
                    });
                });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label(format!("{status}\n\nWaiting for the screen... the remote computer may need to accept the connection."));
                });
            }
        });
        // Keeps input latency low; the texture is only updated on new frames.
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

impl App {
    fn send_key(&self, key: RemoteKey, down: bool) {
        if let Some(tx) = self.tx() {
            let _ = tx.try_send(Packet::Input(InputEvent::Key { key, down }));
        }
    }

    fn sync_mod(&mut self, key: RemoteKey, now: bool) {
        let idx = match key {
            RemoteKey::Shift => 0,
            RemoteKey::Ctrl => 1,
            RemoteKey::Alt => 2,
            RemoteKey::Meta => 3,
            _ => return,
        };
        if self.mods[idx] != now {
            self.mods[idx] = now;
            self.send_key(key, now);
        }
    }

    fn send_file(&self, path: std::path::PathBuf) {
        let Some(tx) = self.tx() else { return };
        std::thread::spawn(move || {
            const CHUNK: usize = 64 * 1024;
            const DEFAULT_MAX: u64 = 512 * 1024 * 1024;
            let max = std::env::var("RF_MAX_FILE_BYTES")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(DEFAULT_MAX);
            let total = match std::fs::metadata(&path) {
                Ok(m) if m.is_file() => m.len(),
                Ok(_) => {
                    tracing::warn!("only regular files can be sent");
                    return;
                }
                Err(e) => {
                    tracing::warn!("cannot read file info: {e}");
                    return;
                }
            };
            if total == 0 || total > max {
                tracing::warn!("file size out of range: {total} (limit {max})");
                return;
            }
            let mut file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!("cannot open file: {e}");
                    return;
                }
            };
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file").to_string();
            let id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| (d.as_nanos() as u64).max(1))
                .unwrap_or(1);
            let mut offset = 0u64;
            let mut buf = vec![0u8; CHUNK];
            while offset < total {
                let want = ((total - offset) as usize).min(CHUNK);
                let n = match file.read(&mut buf[..want]) {
                    Ok(0) => {
                        tracing::warn!("file ended earlier than expected: {name}");
                        return;
                    }
                    Ok(n) => n,
                    Err(e) => {
                        tracing::warn!("cannot read file: {e}");
                        return;
                    }
                };
                let end = offset + n as u64;
                let fc = FileChunk {
                    transfer_id: id,
                    name: name.clone(),
                    offset,
                    total,
                    data: buf[..n].to_vec(),
                    last: end == total,
                };
                if tx.blocking_send(Packet::File(fc)).is_err() { return; }
                offset = end;
            }
            tracing::info!("file sent: {name} ({total} bytes)");
        });
    }
}

fn btn_index(b: MouseButton) -> usize {
    match b {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
    }
}

/// Printable characters come from Text events; only special keys are mapped here.
fn egui_key_to_remote(k: egui::Key) -> Option<RemoteKey> {
    Some(match k {
        egui::Key::Enter => RemoteKey::Enter,
        egui::Key::Tab => RemoteKey::Tab,
        egui::Key::Backspace => RemoteKey::Backspace,
        egui::Key::Escape => RemoteKey::Escape,
        egui::Key::Delete => RemoteKey::Delete,
        egui::Key::Insert => RemoteKey::Insert,
        egui::Key::Home => RemoteKey::Home,
        egui::Key::End => RemoteKey::End,
        egui::Key::PageUp => RemoteKey::PageUp,
        egui::Key::PageDown => RemoteKey::PageDown,
        egui::Key::ArrowUp => RemoteKey::Up,
        egui::Key::ArrowDown => RemoteKey::Down,
        egui::Key::ArrowLeft => RemoteKey::Left,
        egui::Key::ArrowRight => RemoteKey::Right,
        egui::Key::F1 => RemoteKey::F1,
        egui::Key::F2 => RemoteKey::F2,
        egui::Key::F3 => RemoteKey::F3,
        egui::Key::F4 => RemoteKey::F4,
        egui::Key::F5 => RemoteKey::F5,
        egui::Key::F6 => RemoteKey::F6,
        egui::Key::F7 => RemoteKey::F7,
        egui::Key::F8 => RemoteKey::F8,
        egui::Key::F9 => RemoteKey::F9,
        egui::Key::F10 => RemoteKey::F10,
        egui::Key::F11 => RemoteKey::F11,
        egui::Key::F12 => RemoteKey::F12,
        _ => return None,
    })
}

