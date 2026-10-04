//! Remote screen viewer: shows the decoded frames and forwards mouse/keyboard/files.

use remote_friend_common::{FileChunk, InputEvent, MouseButton, Packet, RemoteKey};
use std::io::Read as _;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{history, App};
use egui::RichText;

impl App {
    pub(crate) fn viewer_ui(&mut self, ctx: &egui::Context) {
        let shared = self.shared.clone().unwrap();
        let tx = self.tx_out.clone().unwrap();
        let (new_frame, status, hw, hh, fps) = {
            let mut s = shared.lock().unwrap();
            // Take ownership of the frame instead of cloning it (a 1080p RGBA frame is ~8 MiB).
            (s.texture.take(), s.status.clone(), s.host_w, s.host_h, s.fps)
        };
        self.viewer_events(ctx, &shared);
        self.files_window(ctx, &shared);
        let (perms, banner, upload, quality, rtt, monitors, monitor, direct, cursor_in_video) = {
            let s = shared.lock().unwrap();
            let banner = s.banner.as_ref().filter(|(_, t)| t.elapsed().as_secs() < 6).map(|(b, _)| b.clone());
            (s.perms, banner, s.upload.clone(), s.quality.clone(), s.rtt, s.monitors.clone(), s.monitor, s.direct, s.cursor_in_video)
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
            ui.horizontal_wrapped(|ui| {
                if ui.button("✖ Disconnect").clicked() {
                    self.send_control(serde_json::json!({"t": "bye"}));
                    self.disconnect();
                    return;
                }
                let rtt = if rtt > 0 { format!("  ·  {rtt} ms") } else { String::new() };
                ui.label(format!("{status}  ·  {hw}×{hh}  ·  {fps:.0} fps{rtt}"));
                if direct {
                    ui.label(RichText::new("Direct").color(crate::OK))
                        .on_hover_text("Peer-to-peer: the data goes straight between the devices, not through the server");
                } else {
                    ui.label(RichText::new("Via server").weak())
                        .on_hover_text("End-to-end encrypted through the server (a direct path was not possible or is still being tried)");
                }
                ui.separator();
                let sound_label = if self.sound_on { "🔊 Sound on" } else { "🔇 Sound off" };
                if ui
                    .add_enabled(perms.sound, egui::Button::new(sound_label))
                    .on_hover_text("Play the remote computer's sound here")
                    .clicked()
                {
                    self.sound_on = !self.sound_on;
                    shared.lock().unwrap().sound = self.sound_on;
                    self.send_control(serde_json::json!({"t": "audio", "on": self.sound_on}));
                }
                ui.add_enabled_ui(perms.control, |ui| {
                    ui.menu_button("⌨ Keys", |ui| self.keys_menu(ui));
                });
                ui.menu_button(format!("Quality: {}", quality_label(&quality)), |ui| {
                    for (p, label) in [("fast", "Fast (smooth on slow networks)"), ("balanced", "Balanced"), ("sharp", "Sharp (more data)")] {
                        if ui.selectable_label(quality == p, label).clicked() {
                            self.send_control(serde_json::json!({"t": "quality", "p": p}));
                            ui.close();
                        }
                    }
                });
                if monitors.len() > 1 {
                    ui.add_enabled_ui(perms.control, |ui| {
                        ui.menu_button(format!("🖥 Screen {}", monitor + 1), |ui| {
                            for (i, label) in &monitors {
                                if ui.selectable_label(*i == monitor, label).clicked() {
                                    self.send_control(serde_json::json!({"t": "monitor", "i": i}));
                                    ui.close();
                                }
                            }
                        });
                    });
                }
                let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                if ui.button(if full { "⛶ Exit full screen" } else { "⛶ Full screen" }).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
                }
                if ui
                    .add_enabled(perms.files, egui::Button::new("📁 Files"))
                    .on_hover_text("Send files to the remote computer, or browse and download its files")
                    .clicked()
                {
                    self.files_open = !self.files_open;
                    if self.files_open && shared.lock().unwrap().remote_dir.is_none() {
                        self.send_control(serde_json::json!({"t": "ls", "path": ""}));
                    }
                }
                let me = remote_friend_host::snapshot();
                if me.online
                    && !self.switch_pending
                    && ui
                        .button("Switch sides")
                        .on_hover_text("Let the remote computer control this one instead (they are asked first)")
                        .clicked()
                {
                    self.offer_switch(false);
                }
                if let Some((name, got, total)) = &upload {
                    let frac = if *total > 0 { *got as f32 / *total as f32 } else { 0.0 };
                    ui.add(egui::ProgressBar::new(frac).desired_width(180.0).show_percentage().text(format!("Sending {name}")));
                    if ui.small_button("Cancel").clicked() {
                        shared.lock().unwrap().cancel_upload = true;
                    }
                }
            });
            if !perms.control {
                ui.colored_label(crate::WARN, "View only: the computer's user turned off your mouse and keyboard.");
            }
            if let Some(b) = &banner {
                ui.label(RichText::new(b).color(crate::OK));
            }
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

                // The remote pointer is part of the picture: hide ours so there is only one.
                if cursor_in_video && resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::None);
                }
                if !perms.control {
                    // View only: nothing is sent.
                } else if let Some(pos) = resp.hover_pos() {
                    if let Some((x, y)) = to_host(pos) {
                        if self.last_mouse != Some((x, y)) {
                            self.last_mouse = Some((x, y));
                            let _ = tx.try_send(Packet::Input(InputEvent::MouseMove { x, y }));
                        }
                    }
                }
                let events: Vec<egui::Event> = if perms.control { ctx.input(|i| i.events.clone()) } else { Vec::new() };
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
                        // Clipboard shortcuts arrive as Copy/Cut/Paste instead of key events.
                        egui::Event::Copy => self.shortcut('c'),
                        egui::Event::Cut => self.shortcut('x'),
                        egui::Event::Paste(text) => {
                            // Put this computer's clipboard on the remote one first, then paste there.
                            if perms.clipboard {
                                self.send_control(serde_json::json!({"t": "clip", "text": text}));
                            }
                            self.shortcut('v');
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

                let (mshift, mctrl, malt, mmeta) = if !perms.control { (false, false, false, false) } else { ctx.input(|i| {
                    let m = &i.modifiers;
                    (m.shift, m.ctrl, m.alt, m.mac_cmd || (m.command && !m.ctrl))
                }) };
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
    /// File window: send files, browse the remote computer's folders, download files.
    fn files_window(&mut self, ctx: &egui::Context, shared: &std::sync::Arc<std::sync::Mutex<crate::net::Shared>>) {
        if !self.files_open {
            return;
        }
        let (dir, upload, download, downloaded) = {
            let s = shared.lock().unwrap();
            (s.remote_dir.clone(), s.upload.clone(), s.download.clone(), s.downloaded.clone())
        };
        let mut open = true;
        egui::Window::new("Files")
            .open(&mut open)
            .default_size([520.0, 440.0])
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(upload.is_none(), egui::Button::new("⬆ Send a file to the remote computer…")).clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            self.send_file(path);
                        }
                    }
                });
                if let Some((name, got, total)) = &upload {
                    ui.horizontal(|ui| {
                        let frac = if *total > 0 { *got as f32 / *total as f32 } else { 0.0 };
                        ui.add(egui::ProgressBar::new(frac).show_percentage().text(format!("Sending {name}")).desired_width(320.0));
                        if ui.button("Cancel").clicked() {
                            shared.lock().unwrap().cancel_upload = true;
                        }
                    });
                }
                ui.small(format!("Sent files are saved in the remote computer's Downloads/RemoteFriend folder."));
                ui.separator();
                ui.label(RichText::new("On the remote computer").strong());
                let Some(dir) = dir else {
                    ui.spinner();
                    return;
                };
                ui.horizontal_wrapped(|ui| {
                    if let Some(parent) = &dir.parent {
                        if ui.button("⬆ Up").clicked() {
                            self.send_control(serde_json::json!({"t": "ls", "path": parent}));
                        }
                    }
                    if ui.button("🏠 Home").clicked() {
                        self.send_control(serde_json::json!({"t": "ls", "path": ""}));
                    }
                    ui.label(RichText::new(if dir.path.is_empty() { "This PC" } else { &dir.path }).monospace());
                });
                if let Some(e) = &dir.error {
                    ui.colored_label(crate::WARN, e);
                }
                if let Some(d) = &download {
                    ui.horizontal(|ui| {
                        let frac = if d.total > 0 { d.got as f32 / d.total as f32 } else { 1.0 };
                        ui.add(egui::ProgressBar::new(frac).show_percentage().text(format!("Downloading {}", d.name)).desired_width(320.0));
                        if ui.button("Cancel").clicked() {
                            self.send_control(serde_json::json!({"t": "get_cancel", "id": d.id}));
                            remote_friend_host::cancel_incoming_file(d.id);
                            shared.lock().unwrap().download = None;
                        }
                    });
                }
                if let Some(path) = &downloaded {
                    ui.horizontal_wrapped(|ui| {
                        ui.small(format!("Last download: {path}"));
                        if ui.small_button("Open folder").clicked() {
                            if let Some(folder) = std::path::Path::new(path).parent() {
                                crate::open_path(folder);
                            }
                        }
                    });
                }
                ui.separator();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for (name, is_dir, size) in &dir.entries {
                        ui.horizontal(|ui| {
                            let full = if dir.path.is_empty() {
                                name.clone()
                            } else {
                                std::path::Path::new(&dir.path).join(name).display().to_string()
                            };
                            if *is_dir {
                                if ui.link(format!("📁 {name}")).clicked() {
                                    self.send_control(serde_json::json!({"t": "ls", "path": full}));
                                }
                            } else {
                                ui.label(format!("📄 {name}"));
                                ui.small(human_size(*size));
                                if ui.add_enabled(download.is_none(), egui::Button::new("⬇ Download").small()).clicked() {
                                    let id = rand_id();
                                    shared.lock().unwrap().download =
                                        Some(crate::net::Transfer { id, name: name.clone(), got: 0, total: *size });
                                    self.send_control(serde_json::json!({"t": "get", "path": full, "id": id}));
                                }
                            }
                        });
                    }
                });
            });
        if !open {
            self.files_open = false;
        }
    }

    /// Things the network thread reported: saved device token, clipboard, switch requests.
    fn viewer_events(&mut self, ctx: &egui::Context, shared: &std::sync::Arc<std::sync::Mutex<crate::net::Shared>>) {
        let (issued, rejected, clip, switch_req, switch_ok, clip_ok, login_ok, wrong_pw) = {
            let mut s = shared.lock().unwrap();
            let rejected = std::mem::take(&mut s.device_rejected);
            (
                s.issued_device.take(),
                rejected,
                s.clip_in.take(),
                std::mem::take(&mut s.switch_req),
                std::mem::take(&mut s.switch_ok),
                s.perms.clipboard,
                std::mem::take(&mut s.login_ok),
                std::mem::take(&mut s.wrong_password),
            )
        };
        // Remember a password that worked; forget one that no longer does.
        if login_ok && !self.pending_password.is_empty() {
            let pw = std::mem::take(&mut self.pending_password);
            history::set_password(&mut self.recents, &self.current_addr, &pw);
        }
        if wrong_pw {
            history::set_password(&mut self.recents, &self.current_addr, "");
            self.login_msg = "Wrong password (the computer may have a new one).".into();
        }
        if let Some(token) = issued {
            history::set_device(&mut self.recents, &self.current_addr, &token);
        }
        if rejected {
            history::set_device(&mut self.recents, &self.current_addr, "");
            self.login_msg = "That computer no longer trusts this device: enter its password.".into();
        }
        if let Some(text) = clip {
            if clip_ok && !text.is_empty() {
                ctx.copy_text(text);
            }
        }
        if switch_req {
            self.switch_question = true;
        }
        if switch_ok {
            // The other side now controls this computer; our viewer is no longer needed.
            self.switch_pending = false;
            self.disconnect();
            self.flash("Switched sides: the other computer now controls this one");
            return;
        }
        if self.switch_question {
            let name = if self.current_name.is_empty() { self.current_addr.clone() } else { self.current_name.clone() };
            egui::Modal::new(egui::Id::new("switch_question")).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.label(RichText::new("Switch sides?").size(19.0).strong());
                ui.label(format!("{name} asks to control this computer instead. Your view of {name} closes."));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("Allow").strong().color(egui::Color32::WHITE)).fill(crate::OK)).clicked() {
                        self.switch_question = false;
                        self.offer_switch(true);
                    }
                    if ui.button("No").clicked() {
                        self.switch_question = false;
                        self.send_control(serde_json::json!({"t": "switch_no"}));
                    }
                });
            });
        }
    }

    /// Offer this computer to the one we control: it may connect back once, without our
    /// password, and we stop viewing it. `reply`: answering its request (no second question).
    fn offer_switch(&mut self, reply: bool) {
        let me = remote_friend_host::snapshot();
        if me.id.is_empty() || !me.online {
            self.flash("This computer is not online, so the other side can't connect to it");
            return;
        }
        let token = remote_friend_host::grant_temporary(&format!("{} (switched sides)", self.current_name));
        self.switch_pending = true;
        self.send_control(serde_json::json!({"t": "switch", "id": me.id, "token": token, "name": me.name, "reply": reply}));
    }

    pub(crate) fn send_control(&self, v: serde_json::Value) {
        if let Some(tx) = self.tx() {
            let _ = tx.try_send(Packet::Control(v.to_string()));
        }
    }

    /// Copy/cut/paste with the remote computer's command key (Cmd on a Mac, Ctrl elsewhere),
    /// whatever this computer uses.
    fn shortcut(&mut self, letter: char) {
        let mac_host = self.shared.as_ref().is_some_and(|s| s.lock().unwrap().host_os == "macos");
        let (cmd, other, cmd_idx, other_idx) =
            if mac_host { (RemoteKey::Meta, RemoteKey::Ctrl, 3, 1) } else { (RemoteKey::Ctrl, RemoteKey::Meta, 1, 3) };
        let other_held = self.mods[other_idx];
        let cmd_held = self.mods[cmd_idx];
        if other_held {
            self.send_key(other, false);
        }
        if !cmd_held {
            self.send_key(cmd, true);
        }
        self.send_key(RemoteKey::Char(letter), true);
        self.send_key(RemoteKey::Char(letter), false);
        if !cmd_held {
            self.send_key(cmd, false);
        }
        if other_held {
            self.send_key(other, true);
        }
    }

    /// Key combinations that can't be typed into the viewer (the local system takes them).
    fn keys_menu(&mut self, ui: &mut egui::Ui) {
        use RemoteKey::*;
        let combos: [(&str, &[RemoteKey]); 9] = [
            ("Ctrl+Alt+Del", &[Ctrl, Alt, Delete]),
            ("Ctrl+Shift+Esc (Task Manager)", &[Ctrl, Shift, Escape]),
            ("Win / ⌘", &[Meta]),
            ("Alt+Tab", &[Alt, Tab]),
            ("Alt+F4", &[Alt, F4]),
            ("Win+D (show desktop)", &[Meta, Char('d')]),
            ("Win+L (lock)", &[Meta, Char('l')]),
            ("Print Screen", &[PrintScreen]),
            ("Esc", &[Escape]),
        ];
        for (label, keys) in combos {
            if ui.button(label).clicked() {
                for k in keys {
                    self.send_key(*k, true);
                }
                for k in keys.iter().rev() {
                    self.send_key(*k, false);
                }
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Paste this computer's clipboard as typing").clicked() {
            if let Some(text) = read_local_clipboard() {
                if let Some(tx) = self.tx() {
                    let _ = tx.try_send(Packet::Input(InputEvent::Text(text.chars().take(4096).collect())));
                }
            }
            ui.close();
        }
    }

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
        let Some(shared) = self.shared.clone() else { return };
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
            shared.lock().unwrap().upload = Some((name.clone(), 0, total));
            let id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| (d.as_nanos() as u64).max(1))
                .unwrap_or(1);
            let mut offset = 0u64;
            let mut buf = vec![0u8; CHUNK];
            shared.lock().unwrap().cancel_upload = false;
            while offset < total {
                if std::mem::take(&mut shared.lock().unwrap().cancel_upload) {
                    let _ = tx.blocking_send(Packet::Control(serde_json::json!({"t": "file_cancel", "id": id}).to_string()));
                    let mut sh = shared.lock().unwrap();
                    sh.upload = None;
                    sh.banner(format!("Sending {name} canceled"));
                    return;
                }
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
                if tx.blocking_send(Packet::File(fc)).is_err() {
                    shared.lock().unwrap().upload = None;
                    return;
                }
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


fn quality_label(p: &str) -> &'static str {
    match p {
        "fast" => "Fast",
        "sharp" => "Sharp",
        _ => "Balanced",
    }
}

/// Text on this computer's clipboard (for "paste as typing").
fn read_local_clipboard() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok().filter(|t| !t.is_empty())
}

fn human_size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} B"),
    }
}

/// Transfer id for a download.
fn rand_id() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1).max(1)
}
