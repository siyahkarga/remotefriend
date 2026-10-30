//! Remote screen viewer: shows the decoded frames and forwards mouse/keyboard/files.

use remote_friend_common::{FileChunk, InputEvent, MouseButton, Packet, RemoteKey};
use std::io::Read as _;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ui_kit as kit;
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

        // Several screens: offer the choice once, when the picture first arrives.
        if monitors.len() > 1 && !self.screens_offered && self.screen_tex.is_some() {
            self.screens_offered = true;
            self.screens_open = true;
        }
        self.screen_answers(&shared, monitor);
        if self.screens_open {
            if let Some(i) = self.screen_picker(ctx, &monitors, monitor, perms.control) {
                self.send_control(serde_json::json!({"t": "monitor", "i": i}));
            }
        }

        let title = if self.current_name.is_empty() { remote_friend_common::format_id(&self.current_addr) } else { self.current_name.clone() };
        egui::TopBottomPanel::top("bar")
            .frame(egui::Frame::new().fill(kit::SIDEBAR).inner_margin(egui::Margin::symmetric(12, 8)))
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let disconnect = kit::danger_solid("✖  Disconnect");
                    if ui.add(disconnect).on_hover_text("End the session").clicked() {
                        self.send_control(serde_json::json!({"t": "bye"}));
                        self.disconnect();
                        return;
                    }
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new(&title).color(kit::TEXT).strong());
                        let rtt = if rtt > 0 { format!(" · {rtt} ms") } else { String::new() };
                        ui.label(RichText::new(format!("{status} · {hw}×{hh} · {fps:.0} fps{rtt}")).size(11.5).color(kit::MUTED));
                    });
                    if direct {
                        kit::badge(ui, "direct", kit::OK)
                            .on_hover_text("Peer-to-peer: the data goes straight between the devices, not through the server");
                    } else {
                        kit::badge(ui, "via server", kit::MUTED).on_hover_text(
                            "End-to-end encrypted through the server (a direct path was not possible or is still being tried)",
                        );
                    }
                    ui.add_space(10.0);
                    if monitors.len() > 1 {
                        let label = format!("🖥 Screen {} of {}", monitor + 1, monitors.len());
                        if ui.add(kit::secondary(&label)).on_hover_text("Choose which screen to see").clicked() {
                            self.screens_open = !self.screens_open;
                            self.screens_note = None;
                        }
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
                    let sound_label = if self.sound_on { "🔊" } else { "🔇" };
                    if ui
                        .add_enabled(perms.sound, kit::secondary(sound_label))
                        .on_hover_text(if self.sound_on { "Sound on (click to mute)" } else { "Sound off (click to play)" })
                        .clicked()
                    {
                        self.sound_on = !self.sound_on;
                        shared.lock().unwrap().sound = self.sound_on;
                        self.send_control(serde_json::json!({"t": "audio", "on": self.sound_on}));
                    }
                    if ui
                        .add_enabled(perms.files, kit::secondary("📁 Files"))
                        .on_hover_text("Send files to the remote computer, or browse and download its files")
                        .clicked()
                    {
                        if self.files_open {
                            self.files_open = false;
                        } else {
                            self.open_files();
                        }
                    }
                    let me = remote_friend_host::snapshot();
                    if me.online
                        && !self.switch_pending
                        && ui
                            .add(kit::secondary("⇄ Switch sides"))
                            .on_hover_text("Let the remote computer control this one instead (they are asked first)")
                            .clicked()
                    {
                        self.offer_switch(false);
                    }
                    let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                    if ui.add(kit::secondary("⛶")).on_hover_text(if full { "Exit full screen" } else { "Full screen" }).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
                    }
                    if let Some((name, got, total)) = &upload {
                        kit::transfer_inline(ui, "⬆", *got, *total, 90.0).on_hover_text(format!("Sending {name}"));
                        if ui.add(kit::quiet("Cancel")).clicked() {
                            shared.lock().unwrap().cancel_upload = true;
                        }
                    }
                });
            });

        // Short messages float over the picture instead of moving it.
        let notice = if !perms.control {
            Some(("View only: the computer's user turned off your mouse and keyboard.".to_string(), kit::WARN))
        } else {
            banner.map(|b| (b, kit::OK))
        };
        if let Some((text, color)) = notice {
            egui::Area::new(egui::Id::new("viewer_notice"))
                .anchor(egui::Align2::CENTER_TOP, [0.0, 64.0])
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(kit::tint(color, 0.18))
                        .stroke(egui::Stroke::new(1.0_f32, kit::tint(color, 0.6)))
                        .corner_radius(kit::R_ROW)
                        .inner_margin(egui::Margin::symmetric(14, 8))
                        .show(ui, |ui| {
                            ui.label(RichText::new(text).color(kit::TEXT));
                        });
                });
        }

        self.files_window(ctx, &shared);
        #[cfg(debug_assertions)]
        self.debug_autosend(&shared);

        // Trust on first use: an unknown server is confirmed by its fingerprint once.
        if let Some((srv, fp)) = shared.lock().unwrap().tofu_pending.clone() {
            crate::ui_dialogs::dialog("tofu").show(ctx, |ui| {
                ui.set_width(460.0);
                crate::ui_dialogs::heading(ui, "🔒", kit::WARN, "First connection to this server", |ui| {
                    kit::line(ui, RichText::new(format!("Server: {srv}")).color(kit::TEXT));
                    ui.add(egui::Label::new(RichText::new(&fp).monospace().size(12.0).color(kit::TEXT)).wrap());
                    kit::note(ui, "This is normal the first time. It must match the fingerprint shown at the end of the server setup; it will be remembered.");
                });
                ui.add_space(14.0);
                kit::split(ui, 32.0, |_| {}, |ui| {
                    if ui.add(kit::primary("Trust and connect")).clicked() {
                        shared.lock().unwrap().tofu_answer = Some(true);
                    }
                    if ui.add(kit::secondary("Cancel")).clicked() {
                        shared.lock().unwrap().tofu_answer = Some(false);
                    }
                });
            });
        }

        let (failed, accepted) = {
            let s = shared.lock().unwrap();
            (s.failed, s.accepted)
        };

        // Cloning a TextureHandle only clones a reference, not the pixels.
        let display_tex = self.screen_tex.clone();
        // A session that ran and ended (the computer closed it, RemoteFriends was closed there,
        // the network dropped): back to the home screen instead of a frozen picture.
        if failed && (accepted || display_tex.is_some()) {
            self.disconnect();
            self.flash(format!("Session ended: {status}"));
            ctx.request_repaint();
            return;
        }
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

                // The computer's pointer is part of the picture: hide ours so there is only one.
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
                                // egui: positive y = wheel up (content moves down); the protocol
                                // counts lines with positive = down/right, like the browser page.
                                self.scroll_acc.0 -= delta.x * mult;
                                self.scroll_acc.1 -= delta.y * mult;
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
    /// Development aid: RF_AUTOSEND=path opens the file window and sends that file once the
    /// computer said what it allows (debug builds).
    #[cfg(debug_assertions)]
    fn debug_autosend(&mut self, shared: &std::sync::Arc<std::sync::Mutex<crate::net::Shared>>) {
        use std::sync::atomic::{AtomicU8, Ordering};
        static STEP: AtomicU8 = AtomicU8::new(0);
        let Ok(path) = std::env::var("RF_AUTOSEND") else { return };
        let (accepted, level, dir) = {
            let s = shared.lock().unwrap();
            (s.accepted, s.files_level.clone(), s.remote_dir.as_ref().map(|d| d.path.clone()))
        };
        match STEP.load(Ordering::Relaxed) {
            0 if accepted => {
                self.open_files();
                STEP.store(1, Ordering::Relaxed);
            }
            1 if matches!(level.as_str(), "shared" | "disk" | "all") && dir.is_some() => {
                let dest = std::env::var("RF_AUTOSEND_DIR").ok();
                self.send_file(path.into(), dest);
                STEP.store(2, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// Ask the computer what we may reach among its files, and list where we start.
    pub(crate) fn open_files(&mut self) {
        self.files_open = true;
        self.send_control(serde_json::json!({"t": "files_hello"}));
        self.list_remote("");
    }

    fn list_remote(&self, path: &str) {
        if let Some(shared) = &self.shared {
            shared.lock().unwrap().ls_asked = Some(std::time::Instant::now());
        }
        self.send_control(serde_json::json!({"t": "ls", "path": path}));
    }

    /// File window: what this viewer may reach, sending files (and where they land), and the
    /// computer's folders to download from.
    fn files_window(&mut self, ctx: &egui::Context, shared: &std::sync::Arc<std::sync::Mutex<crate::net::Shared>>) {
        if !self.files_open {
            return;
        }
        let (dir, upload, download, downloaded, level, shared_dir, sent, asked, refresh) = {
            let mut s = shared.lock().unwrap();
            let refresh = std::mem::take(&mut s.files_refresh);
            (
                s.remote_dir.clone(),
                s.upload.clone(),
                s.download.clone(),
                s.downloaded.clone(),
                s.files_level.clone(),
                s.files_shared.clone(),
                s.sent.clone(),
                s.ls_asked,
                refresh,
            )
        };
        if refresh {
            self.list_remote("");
        }
        // Where a file we send lands: the folder shown, if we may write there.
        let browsing = dir.as_ref().map(|d| d.path.clone()).filter(|p| !p.is_empty());
        let dest = match (level.as_str(), &browsing) {
            ("disk" | "all", Some(p)) => Some(p.clone()),
            _ => None,
        };
        let shared_name = if shared_dir.is_empty() { "Downloads/RemoteFriend".to_string() } else { shared_dir.clone() };
        let mut open = true;
        egui::SidePanel::right("files")
            .default_width(380.0)
            .min_width(300.0)
            .max_width(480.0)
            .frame(egui::Frame::new().fill(kit::SURFACE).inner_margin(14))
            .show(ctx, |ui| {
                kit::split(
                    ui,
                    30.0,
                    |ui| {
                        ui.label(RichText::new("Files").size(16.0).color(kit::TEXT).strong());
                    },
                    |ui| {
                        if kit::icon(ui, kit::Icon::Close, "Close").clicked() {
                            open = false;
                        }
                    },
                );
                let (color, what, detail) = match level.as_str() {
                    "shared" => (kit::ACCENT, "Shared folder only", format!("That computer shares one folder with you: {shared_name}")),
                    "disk" => (kit::OK, "Its folders and disks", "System folders and hidden files stay hidden.".to_string()),
                    "all" => (kit::OK, "Everything (your device)", "All folders and disks of that computer.".to_string()),
                    "wait" => (kit::WARN, "Waiting for approval", "The computer's user is asked to let you use its files.".to_string()),
                    "off" => (kit::ERR, "File transfer is off", "The computer's user turned file transfer off.".to_string()),
                    _ => (kit::MUTED, "Files", String::new()),
                };
                kit::dot(ui, color, what);
                if !detail.is_empty() {
                    kit::note(ui, detail);
                }
                let direct = shared.lock().unwrap().direct;
                kit::note(
                    ui,
                    if direct {
                        "Files go straight between the two devices (direct), end-to-end encrypted."
                    } else {
                        "Files go through the server, end-to-end encrypted: the server can't read them."
                    },
                );

                kit::section_label(ui, "SEND TO THAT COMPUTER");
                let can_send = upload.is_none() && !matches!(level.as_str(), "wait" | "off");
                if kit::wide(ui, kit::primary("⬆  Send a file…"), 34.0).clicked() && can_send {
                    if let Some(path) = rfd::FileDialog::new().pick_file() {
                        self.send_file(path, dest.clone());
                    }
                }
                let goes_to = dest.clone().unwrap_or_else(|| shared_name.clone());
                kit::note(ui, format!("Goes to: {goes_to}"));
                if let Some((name, got, total)) = &upload {
                    if kit::transfer(ui, "Sending", name, *got, *total, true) {
                        shared.lock().unwrap().cancel_upload = true;
                    }
                }
                for (name, path) in sent.iter().take(3) {
                    kit::line(ui, RichText::new(format!("Sent {name} to {path}")).size(12.0).color(kit::OK)).on_hover_text(path);
                }

                kit::section_label(ui, "GET FROM THAT COMPUTER");
                let waiting = asked.is_some_and(|t| t.elapsed().as_secs() < 10);
                let Some(dir) = dir else {
                    if waiting {
                        ui.spinner();
                    } else {
                        ui.horizontal(|ui| {
                            kit::dot(ui, kit::WARN, "No answer from that computer");
                            if ui.add(kit::secondary("Retry")).clicked() {
                                self.send_control(serde_json::json!({"t": "files_hello"}));
                                self.list_remote("");
                            }
                        });
                    }
                    return;
                };
                ui.horizontal_wrapped(|ui| {
                    if let Some(parent) = &dir.parent {
                        if ui.button("⬆ Up").clicked() {
                            self.list_remote(parent);
                        }
                    }
                    if ui.button("Start").on_hover_text("Where you start on that computer").clicked() {
                        self.list_remote("");
                    }
                    let here = if dir.path.is_empty() { "That computer" } else { &dir.path };
                    ui.label(RichText::new(here).monospace());
                    if waiting {
                        ui.spinner();
                    }
                });
                if let Some(e) = &dir.error {
                    kit::dot(ui, kit::WARN, e.clone());
                }
                if let Some(d) = &download {
                    if kit::transfer(ui, "Getting", &d.name, d.got, d.total, true) {
                        self.send_control(serde_json::json!({"t": "get_cancel", "id": d.id}));
                        remote_friend_host::cancel_incoming_file(d.id);
                        shared.lock().unwrap().download = None;
                    }
                }
                let here = remote_friend_host::receive_dir();
                ui.horizontal_wrapped(|ui| {
                    kit::note(ui, format!("Saved on this computer in: {}", here.display()));
                    if ui.small_button("Open folder").clicked() {
                        let _ = std::fs::create_dir_all(&here);
                        crate::open_path(&here);
                    }
                });
                if let Some(path) = &downloaded {
                    kit::line(ui, RichText::new(format!("Got: {path}")).size(12.0).color(kit::OK)).on_hover_text(path);
                }
                ui.separator();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    if dir.entries.is_empty() && dir.error.is_none() {
                        kit::empty(ui, "This folder is empty.");
                    }
                    for (name, is_dir, size) in &dir.entries {
                        ui.horizontal(|ui| {
                            let full = if dir.path.is_empty() {
                                name.clone()
                            } else {
                                std::path::Path::new(&dir.path).join(name).display().to_string()
                            };
                            if *is_dir {
                                let link = egui::Button::new(RichText::new(format!("📁 {name}")).color(kit::ACCENT)).frame(false).truncate();
                                if ui.add(link).on_hover_text(&full).clicked() {
                                    self.list_remote(&full);
                                }
                            } else {
                                ui.add(egui::Label::new(format!("📄 {name}")).truncate()).on_hover_text(&full);
                                ui.small(human_size(*size));
                                if ui.add_enabled(download.is_none(), egui::Button::new("⬇ Get").small()).on_hover_text("Download to this computer").clicked() {
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
        if login_ok {
            // The typed password is used up: next time a trusted device logs in with its key.
            self.pass_field.clear();
            if !self.pending_password.is_empty() {
                let pw = std::mem::take(&mut self.pending_password);
                history::set_password(&mut self.recents, &self.current_addr, &pw);
            }
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
            // The computer forgot this device: try the saved password right away, if any.
            let addr = self.current_addr.clone();
            if let Some(pw) = history::password_for(&self.recents, &addr) {
                self.disconnect();
                self.pending_password = pw.clone();
                self.connect_to(addr, pw, None);
                return;
            }
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
            crate::ui_dialogs::dialog("switch_question").show(ctx, |ui| {
                ui.set_width(440.0);
                crate::ui_dialogs::heading(ui, "⇄", kit::ACCENT, "Switch sides?", |ui| {
                    kit::note(ui, format!("{name} asks to control this computer instead. Your view of {name} closes."));
                });
                ui.add_space(14.0);
                kit::split(ui, 32.0, |_| {}, |ui| {
                    if ui.add(kit::primary("Allow")).clicked() {
                        self.switch_question = false;
                        self.offer_switch(true);
                    }
                    if ui.add(kit::secondary("No")).clicked() {
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

    /// Control messages must not get lost while a file fills the queue: if it is full,
    /// a short-lived thread waits for room.
    pub(crate) fn send_control(&self, v: serde_json::Value) {
        if let Some(tx) = self.tx() {
            if let Err(tokio::sync::mpsc::error::TrySendError::Full(p)) = tx.try_send(Packet::Control(v.to_string())) {
                std::thread::spawn(move || {
                    let _ = tx.blocking_send(p);
                });
            }
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

    /// Send a file in 64 KiB pieces from a thread. `dest`: the folder on the computer (only
    /// where the computer lets us choose; otherwise it lands in its shared folder).
    fn send_file(&self, path: std::path::PathBuf, dest: Option<String>) {
        let Some(tx) = self.tx() else { return };
        let Some(shared) = self.shared.clone() else { return };
        std::thread::spawn(move || {
            const CHUNK: usize = 64 * 1024;
            // Pieces waiting to go out at most: clicks and keys never wait behind a whole file.
            const IN_FLIGHT: usize = 8;
            let fail = |msg: String| {
                let mut sh = shared.lock().unwrap();
                sh.upload = None;
                sh.banner(msg);
            };
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file").to_string();
            let max = match shared.lock().unwrap().files_max {
                0 => std::env::var("RF_MAX_FILE_BYTES").ok().and_then(|v| v.parse::<u64>().ok()).filter(|v| *v > 0).unwrap_or(512 << 20),
                n => n,
            };
            let total = match std::fs::metadata(&path) {
                Ok(m) if m.is_file() => m.len(),
                Ok(_) => return fail(format!("{name} is not a file")),
                Err(e) => return fail(format!("Cannot read {name}: {e}")),
            };
            if total > max {
                return fail(format!("{name} is too big ({}; that computer takes up to {})", human_size(total), human_size(max)));
            }
            let mut file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(e) => return fail(format!("Cannot open {name}: {e}")),
            };
            shared.lock().unwrap().upload = Some((name.clone(), 0, total));
            let id = rand_id();
            if let Some(dir) = dest {
                if tx.blocking_send(Packet::Control(serde_json::json!({"t": "put_dir", "id": id, "dir": dir}).to_string())).is_err() {
                    return fail("Not sent: the connection is closed".into());
                }
            }
            let mut offset = 0u64;
            let mut buf = vec![0u8; CHUNK];
            shared.lock().unwrap().cancel_upload = false;
            loop {
                if std::mem::take(&mut shared.lock().unwrap().cancel_upload) {
                    let _ = tx.blocking_send(Packet::Control(serde_json::json!({"t": "file_cancel", "id": id}).to_string()));
                    return fail(format!("Sending {name} canceled"));
                }
                if shared.lock().unwrap().upload.is_none() {
                    // The computer refused it (file_err) or the session ended.
                    return;
                }
                if tx.max_capacity() - tx.capacity() >= IN_FLIGHT {
                    std::thread::sleep(std::time::Duration::from_millis(4));
                    continue;
                }
                let want = ((total - offset) as usize).min(CHUNK);
                let n = match file.read(&mut buf[..want]) {
                    Ok(0) if want > 0 => return fail(format!("{name} got shorter while sending")),
                    Ok(n) => n,
                    Err(e) => return fail(format!("Cannot read {name}: {e}")),
                };
                let end = offset + n as u64;
                let fc = FileChunk { transfer_id: id, name: name.clone(), offset, total, data: buf[..n].to_vec(), last: end == total };
                if tx.blocking_send(Packet::File(fc)).is_err() {
                    return fail("Not sent: the connection is closed".into());
                }
                offset = end;
                if offset >= total {
                    break;
                }
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

use crate::ui_kit::human_size;

/// Transfer id for a download.
fn rand_id() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1).max(1)
}
