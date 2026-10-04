//! Home screen ("This computer" and "Connect to a computer") and the Settings window.
//!
//! Layout rules: few words on screen, explanations in tooltips, small grey section labels,
//! icon buttons for copy/show, settings split into tabs.

use egui::{Color32, RichText};
use std::time::{Duration, Instant};

use crate::{autostart, history, restart_app, App, ACCENT, ERR, ICON_PNG, OK, WARN};
use remote_friend_host::ScreenState;

/// Grey for section labels and secondary text.
pub(crate) const MUTED: Color32 = Color32::from_rgb(140, 146, 160);

/// A rounded panel. Long content scrolls inside it; `fill_height` makes side-by-side panels
/// equally tall.
fn card<R>(ui: &mut egui::Ui, title: &str, fill_height: bool, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let margin = 20.0;
    let width = (ui.available_width() - 2.0 * margin).max(120.0);
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(16.0)
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(width);
            if fill_height {
                ui.set_min_height(ui.available_height());
            }
            ui.label(RichText::new(title).size(18.0).strong());
            egui::ScrollArea::vertical()
                .id_salt(title)
                .auto_shrink([false, !fill_height])
                .show(ui, |ui| {
                    ui.set_width(width);
                    add(ui)
                })
                .inner
        })
        .inner
}

/// Small grey section label ("YOUR ID").
fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(12.0);
    ui.label(RichText::new(text).size(11.5).color(MUTED).strong());
}

/// Section label with a widget on the right (e.g. a small button).
fn section_row(ui: &mut egui::Ui, text: &str, right: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(text).size(11.5).color(MUTED).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
    });
}

/// Frameless icon button with a tooltip.
fn icon_btn(ui: &mut egui::Ui, icon: &str, tip: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(icon).size(16.0)).frame(false).min_size(egui::vec2(30.0, 30.0)))
        .on_hover_text(tip)
}

/// Coloured status dot (painted: the bullet glyph is missing from the default font).
pub(crate) fn dot(ui: &mut egui::Ui, color: Color32, text: impl Into<String>) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 18.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 5.0, color);
        ui.add(egui::Label::new(text.into()).wrap());
    });
}

/// Rounded pill with a dot (header status), as wide as its text.
fn pill(ui: &mut egui::Ui, color: Color32, text: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(13.0), color);
    let size = egui::vec2(galley.size().x + 34.0, 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, 13.0, color.gamma_multiply(0.16));
    painter.circle_filled(egui::pos2(rect.left() + 14.0, rect.center().y), 4.0, color);
    painter.galley(egui::pos2(rect.left() + 24.0, rect.center().y - galley.size().y / 2.0), galley, color);
    resp
}

/// Highlighted box for something that needs the user (e.g. the desktop permission).
/// The server owner's key (setup: 32 hex characters), as opposed to a personal "RF-…" key.
fn owner_key_like(key: &str) -> bool {
    let key = key.trim();
    key.len() >= 16 && key.chars().all(|c| c.is_ascii_hexdigit())
}

fn hint_box(ui: &mut egui::Ui, color: Color32, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.7)))
        .corner_radius(10.0)
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// Two-option switch drawn as connected buttons.
fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str, &str)]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (v, label, tip) in options {
            let on = *value == *v;
            let text = RichText::new(*label).size(14.0).color(if on { Color32::WHITE } else { MUTED });
            let btn = egui::Button::new(text).fill(if on { ACCENT } else { ui.visuals().extreme_bg_color }).corner_radius(8.0);
            if ui.add(btn).on_hover_text(*tip).clicked() && !on {
                *value = *v;
                changed = true;
            }
        }
    });
    changed
}

impl App {
    pub(crate) fn open_settings(&mut self, tab: u8) {
        let server = remote_friend_host::server_settings();
        self.settings_server = server.server;
        self.settings_web = server.web_url;
        self.settings_key = server.key;
        self.settings_use_server = server.use_server;
        self.autostart = autostart::is_enabled();
        self.settings_private_apps = remote_friend_host::privacy::private_apps().join(", ");
        self.settings_tab = tab;
        self.settings_open = true;
    }

    pub(crate) fn home_ui(&mut self, ctx: &egui::Context) {
        self.lan.lock().unwrap().retain(|e| e.last_seen.elapsed().as_secs() < 8);
        let snap = remote_friend_host::snapshot();
        let host_error = self.host_error.lock().unwrap().clone();
        if self.logo.is_none() {
            let img = image::load_from_memory(ICON_PNG).expect("embedded icon").to_rgba8();
            let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], &img);
            self.logo = Some(ctx.load_texture("logo", ci, egui::TextureOptions::LINEAR));
        }

        self.update_banner(ctx);
        egui::TopBottomPanel::top("top").frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 10)).fill(ctx.style().visuals.panel_fill)).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(logo) = &self.logo {
                    ui.image((logo.id(), egui::vec2(28.0, 28.0)));
                }
                ui.label(RichText::new("RemoteFriend").size(19.0).strong());
                ui.label(RichText::new(format!("{}", env!("CARGO_PKG_VERSION"))).size(12.0).color(MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙ Settings").clicked() {
                        self.open_settings(self.settings_tab);
                    }
                    // The server answered but does not let this computer in (no or wrong access key).
                    let refused = snap.server_error.as_deref().and_then(|e| e.strip_prefix("registration rejected: "));
                    let (color, text) = match (&snap.server, snap.online, &snap.server_error) {
                        (_, true, _) => (OK, "Online"),
                        (None, _, _) => (WARN, "Local network"),
                        (Some(_), false, Some(_)) if refused.is_some() => (WARN, "No access key"),
                        (Some(_), false, Some(_)) => (ERR, "Offline"),
                        (Some(_), false, None) => (WARN, "Connecting"),
                    };
                    let tip = match (&snap.server, &snap.server_error, refused) {
                        (None, _, _) => "Reachable on the local network only (Settings → Server)".to_string(),
                        (Some(_), _, Some(reason)) => format!("The server says: {reason}. Other computers can still be reached from here."),
                        (Some(s), Some(e), None) => format!("Cannot reach {s}: {e}"),
                        (Some(s), None, None) => format!("Server {s}"),
                    };
                    if pill(ui, color, text).on_hover_text(tip).clicked() {
                        self.open_settings(0);
                    }
                    if let Some((text, t)) = &self.flash {
                        if t.elapsed() < Duration::from_secs(3) {
                            ui.label(RichText::new(text).size(13.0).color(OK));
                        }
                    }
                });
            });
        });

        egui::TopBottomPanel::bottom("log").frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 6)).fill(ctx.style().visuals.panel_fill)).show(ctx, |ui| {
            ui.horizontal(|ui| {
                match snap.notices.last() {
                    Some(n) => {
                        ui.label(RichText::new(format!("{}  {}", n.time, n.text)).size(12.0).color(MUTED));
                    }
                    None => {
                        ui.label(RichText::new("Keep RemoteFriend open to allow remote access.").size(12.0).color(MUTED));
                    }
                }
            });
        });

        egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).fill(ctx.style().visuals.panel_fill)).show(ctx, |ui| {
            if ui.available_width() >= 760.0 {
                ui.columns(2, |cols| {
                    card(&mut cols[0], "This computer", true, |ui| self.this_computer_ui(ui, &snap, host_error.as_deref()));
                    card(&mut cols[1], "Connect to a computer", true, |ui| self.connect_ui(ui));
                });
            } else {
                // Narrow window: one panel at a time.
                ui.horizontal(|ui| {
                    segmented(ui, &mut self.narrow_tab, &[(0u8, "This computer", ""), (1u8, "Connect to a computer", "")]);
                });
                ui.add_space(8.0);
                if self.narrow_tab == 0 {
                    card(ui, "This computer", true, |ui| self.this_computer_ui(ui, &snap, host_error.as_deref()));
                } else {
                    card(ui, "Connect to a computer", true, |ui| self.connect_ui(ui));
                }
            }
        });

        if self.settings_open {
            self.settings_ui(ctx, &snap);
        }
        if self.keys_ui.open {
            self.keys_ui.window(ctx);
        }
        if self.history_open {
            self.history_window(ctx);
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn this_computer_ui(&mut self, ui: &mut egui::Ui, snap: &remote_friend_host::Snapshot, host_error: Option<&str>) {
        if let Some(err) = host_error {
            hint_box(ui, ERR, |ui| {
                ui.label(RichText::new("Sharing this computer is not available").strong());
                ui.label(err);
                if err.contains("already running") || err.contains("33200") {
                    ui.small("RemoteFriend seems to be running already (maybe in a terminal). Close it and restart this app.");
                }
            });
            return;
        }
        if snap.id.is_empty() {
            ui.spinner();
            return;
        }

        section(ui, "YOUR ID");
        ui.horizontal(|ui| {
            ui.label(RichText::new(remote_friend_common::format_id(&snap.id)).size(30.0).strong().monospace());
            if icon_btn(ui, "📋", "Copy ID").clicked() {
                self.copy(ui.ctx(), &snap.id, "ID");
            }
        });

        section(ui, "PASSWORD");
        ui.horizontal(|ui| {
            let shown = if snap.password_from_env {
                "set by REMOTE_FRIEND_PASS".to_string()
            } else if self.show_password {
                snap.password.clone()
            } else {
                "•••••-•••••".to_string()
            };
            ui.label(RichText::new(shown).size(22.0).monospace());
            if !snap.password_from_env {
                if icon_btn(ui, if self.show_password { "🙈" } else { "👁" }, if self.show_password { "Hide" } else { "Show" }).clicked() {
                    self.show_password = !self.show_password;
                }
                if icon_btn(ui, "📋", "Copy password").clicked() {
                    self.copy(ui.ctx(), &snap.password, "Password");
                }
                if icon_btn(ui, "🔄", "New password now (the old one stops working)").clicked()
                    && remote_friend_host::renew_password().is_some()
                {
                    self.show_password = true;
                    self.flash("New password created");
                }
            }
        });
        if !snap.password_from_env {
            let policy = match remote_friend_host::password_policy() {
                remote_friend_host::PasswordPolicy::OnStart => "Changes when RemoteFriend restarts",
                remote_friend_host::PasswordPolicy::AfterSession => "Changes after every session",
                remote_friend_host::PasswordPolicy::Never => "Stays the same until you change it",
            };
            ui.label(RichText::new(policy).size(12.0).color(MUTED))
                .on_hover_text("Settings → Security. Trusted devices (“Always allow”) don't need the password.");
        }

        ui.add_space(10.0);
        let refused = snap.server_error.as_deref().is_some_and(|e| e.starts_with("registration rejected"));
        match (&snap.server, snap.online) {
            (_, true) => dot(ui, OK, "Reachable from anywhere"),
            (None, _) => {
                ui.horizontal(|ui| {
                    dot(ui, WARN, "Local network only");
                    if ui.small_button("Settings…").clicked() {
                        self.open_settings(0);
                    }
                });
            }
            (Some(_), false) if refused => {
                ui.horizontal(|ui| {
                    dot(ui, WARN, "Reachable from anywhere with an access key");
                    if ui.small_button("Enter key…").clicked() {
                        self.open_settings(0);
                    }
                });
            }
            (Some(_), false) => dot(ui, WARN, "Connecting to the server…"),
        }
        match snap.screen {
            ScreenState::Ready => {}
            ScreenState::WaitingPermission => {
                hint_box(ui, WARN, |ui| {
                    ui.label(RichText::new("Allow screen sharing once").strong());
                    ui.label("In the desktop's window: turn on “Allow Remote Interaction”, pick the screen(s), click Share.")
                        .on_hover_text("Wayland asks every app for this; it is remembered.");
                });
            }
            ScreenState::Denied | ScreenState::NoInput => {
                ui.horizontal(|ui| {
                    let text = if snap.screen == ScreenState::Denied {
                        "Screen sharing is not allowed"
                    } else {
                        "Mouse and keyboard are not allowed"
                    };
                    dot(ui, ERR, text);
                    if ui.small_button("Ask again").clicked() && !remote_friend_host::retry_screen_permission() {
                        restart_app();
                    }
                });
            }
        }
        if let Some(url) = &snap.web_url {
            ui.horizontal(|ui| {
                ui.label(RichText::new("📱").size(15.0)).on_hover_text("Open this address on a phone or in any browser");
                ui.hyperlink_to(RichText::new(url.trim_start_matches("https://")).size(15.0), url);
                if icon_btn(ui, "📋", "Copy address").clicked() {
                    self.copy(ui.ctx(), url, "Address");
                }
            });
        }
        if snap.auto_accept {
            ui.colored_label(WARN, "Approval is off (REMOTE_FRIEND_AUTO_ACCEPT=1)");
        }

        section(ui, "VIEWERS SEE");
        let mut full = remote_friend_host::privacy::full_view();
        let wayland = cfg!(target_os = "linux") && std::env::var("WAYLAND_DISPLAY").is_ok_and(|v| !v.is_empty());
        let safe_tip = if wayland {
            "RemoteFriend is hidden (minimized). On Wayland other windows can't be hidden."
        } else {
            "RemoteFriend and private apps (Settings → Privacy) are hidden"
        };
        if segmented(
            ui,
            &mut full,
            &[(false, "🛡 Safe view", safe_tip), (true, "🖥 Everything", "Exactly what you see, as if they sat here")],
        ) {
            remote_friend_host::privacy::set_full_view(full);
        }

        self.sessions_ui(ui);
        self.shared_screen_ui(ui, snap);

        ui.add_space(14.0);
        ui.horizontal_wrapped(|ui| {
            let recent = remote_friend_host::sessions::recent().len();
            if ui.link(RichText::new(format!("History ({recent})")).size(13.0)).clicked() {
                self.history_open = true;
            }
            ui.label(RichText::new("·").color(MUTED));
            if ui
                .link(RichText::new(format!("Trusted devices ({})", snap.trusted_devices)).size(13.0))
                .on_hover_text("Devices you chose “Always allow” for: they connect without password and without asking")
                .clicked()
            {
                self.open_settings(1);
            }
        });
    }

    /// Who is connected right now, with their permissions.
    fn sessions_ui(&mut self, ui: &mut egui::Ui) {
        use remote_friend_host::sessions;
        let live = sessions::list();
        if live.is_empty() {
            return;
        }
        section_row(ui, &format!("CONNECTED NOW ({})", live.len()), |ui| {
            let blocked = sessions::input_blocked();
            let (text, fill) = if blocked { ("▶ Give control back", ACCENT) } else { ("✋ Take back control", WARN.gamma_multiply(0.85)) };
            if ui
                .add(egui::Button::new(RichText::new(text).size(13.0).color(Color32::WHITE)).fill(fill))
                .on_hover_text("Stops everyone's mouse and keyboard until you click again; they can still watch")
                .clicked()
            {
                sessions::set_input_blocked(!blocked);
            }
        });
        for s in live {
            egui::Frame::new()
                .fill(ui.visuals().extreme_bg_color)
                .corner_radius(10.0)
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&s.label).strong());
                        let via = if s.via == sessions::Via::App { "app" } else { "browser" };
                        let path = if s.direct { "direct" } else { "via server" };
                        let trusted = if s.trusted { " · trusted" } else { "" };
                        ui.label(
                            RichText::new(format!("{via} · {} · {path}{trusted}", remote_friend_host::local_time(s.since)))
                                .size(12.0)
                                .color(MUTED),
                        )
                        .on_hover_text(&s.peer);
                    });
                    let mut p = s.perms;
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let toggle = |ui: &mut egui::Ui, on: &mut bool, icon: &str, tip: &str| {
                            let text = RichText::new(icon).size(16.0).color(if *on { Color32::WHITE } else { MUTED });
                            let fill = if *on { ACCENT.gamma_multiply(0.7) } else { ui.visuals().faint_bg_color };
                            if ui
                                .add(egui::Button::new(text).fill(fill).min_size(egui::vec2(36.0, 30.0)))
                                .on_hover_text(format!("{tip}: {}", if *on { "allowed (click to turn off)" } else { "off (click to allow)" }))
                                .clicked()
                            {
                                *on = !*on;
                            }
                        };
                        toggle(ui, &mut p.control, "🖱", "Mouse & keyboard");
                        toggle(ui, &mut p.sound, "🔊", "Sound");
                        toggle(ui, &mut p.files, "📁", "Files");
                        toggle(ui, &mut p.clipboard, "📋", "Clipboard");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let stop = egui::Button::new(RichText::new("Disconnect").color(Color32::WHITE)).fill(ERR);
                            if ui.add(stop).clicked() {
                                sessions::disconnect(s.id);
                            }
                            if s.can_switch
                                && ui.button("Switch sides").on_hover_text("Control their computer instead; they are asked first").clicked()
                            {
                                sessions::request_switch(s.id);
                                self.flash("Asked the other side to switch");
                            }
                        });
                    });
                    if p != s.perms {
                        sessions::set_perms(s.id, p);
                    }
                    if let Some(t) = &s.transfer {
                        let frac = if t.total > 0 { t.got as f32 / t.total as f32 } else { 0.0 };
                        ui.add(egui::ProgressBar::new(frac).show_percentage().text(format!("Receiving {}", t.name)));
                    }
                });
        }
    }

    /// Which screen viewers see (switchable) and a small live preview while someone watches.
    fn shared_screen_ui(&mut self, ui: &mut egui::Ui, snap: &remote_friend_host::Snapshot) {
        let watching = snap.sessions > 0;
        if self.monitors.1.is_none_or(|t| t.elapsed() > Duration::from_secs(3)) {
            self.monitors = (remote_friend_host::monitors(), Some(Instant::now()));
        }
        let monitors = self.monitors.0.clone();
        if !watching && monitors.len() < 2 {
            return;
        }
        section(ui, if watching { "SHARED SCREEN" } else { "SCREEN TO SHARE" });
        if monitors.len() > 1 {
            let current = remote_friend_host::current_monitor();
            ui.horizontal_wrapped(|ui| {
                for m in &monitors {
                    let label = format!("🖥 {}", m.index + 1);
                    let tip = format!("{} · {}×{}{}", m.name, m.width, m.height, if m.primary { " · primary" } else { "" });
                    if ui.selectable_label(m.index == current, label).on_hover_text(tip).clicked() && m.index != current {
                        remote_friend_host::select_monitor(m.index);
                        self.monitors.1 = None;
                    }
                }
                #[cfg(target_os = "linux")]
                if std::env::var("WAYLAND_DISPLAY").is_ok_and(|v| !v.is_empty())
                    && ui
                        .small_button("Choose…")
                        .on_hover_text("Open the desktop's sharing dialog again to pick screens (RemoteFriend restarts)")
                        .clicked()
                    && !remote_friend_host::retry_screen_permission()
                {
                    restart_app();
                }
            });
        }
        if watching {
            if self.preview_at.is_none_or(|t| t.elapsed() > Duration::from_millis(900)) {
                self.preview_at = Some(Instant::now());
                if let Some((w, h, rgba)) = remote_friend_host::preview(280) {
                    let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    match self.preview_tex.as_mut() {
                        Some(t) if t.size() == [w as usize, h as usize] => t.set(img, egui::TextureOptions::LINEAR),
                        _ => self.preview_tex = Some(ui.ctx().load_texture("preview", img, egui::TextureOptions::LINEAR)),
                    }
                }
            }
            if let Some(t) = &self.preview_tex {
                let max_w = ui.available_width().min(280.0);
                let size = t.size_vec2() * (max_w / t.size_vec2().x);
                ui.add(egui::Image::new((t.id(), size)).corner_radius(8.0)).on_hover_text("What viewers see right now");
            }
        } else {
            self.preview_tex = None;
        }
    }

    fn connect_ui(&mut self, ui: &mut egui::Ui) {
        section(ui, "COMPUTER ID OR ADDRESS");
        let id_resp = ui.add(
            egui::TextEdit::singleline(&mut self.host_field)
                .hint_text("123 456 789")
                .font(egui::TextStyle::Heading)
                .margin(egui::Margin::symmetric(10, 8))
                .desired_width(f32::INFINITY),
        );
        section(ui, "PASSWORD");
        let trusted = history::device_for(&self.recents, &self.host_field).is_some();
        let saved = history::password_for(&self.recents, &self.host_field).is_some();
        let hint = if trusted {
            "not needed — this device is trusted"
        } else if saved {
            "saved — leave empty to use it"
        } else {
            "shown on that computer"
        };
        let pw_resp = ui.add(
            egui::TextEdit::singleline(&mut self.pass_field)
                .password(true)
                .hint_text(hint)
                .margin(egui::Margin::symmetric(10, 8))
                .desired_width(f32::INFINITY),
        );
        let enter = (id_resp.lost_focus() || pw_resp.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.add_space(8.0);
        let btn = egui::Button::new(RichText::new("Connect").size(16.0).strong().color(Color32::WHITE))
            .fill(ACCENT)
            .corner_radius(10.0);
        if ui.add_sized([ui.available_width(), 42.0], btn).clicked() || enter {
            self.connect();
        }
        if !self.login_msg.is_empty() {
            ui.colored_label(WARN, &self.login_msg);
        }

        let my_name = remote_friend_host::snapshot().name;
        let lan: Vec<crate::net::LanEntry> =
            self.lan.lock().unwrap().iter().filter(|e| e.name != my_name).cloned().collect();
        if !lan.is_empty() {
            section(ui, "ON THIS NETWORK");
            for e in lan {
                ui.horizontal(|ui| {
                    ui.label(format!("🖥 {}", e.name)).on_hover_text(&e.ip);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Connect").clicked() {
                            self.host_field = format!("{}:{}", e.ip, e.port);
                            self.connect();
                        }
                    });
                });
            }
        }

        if self.recents.is_empty() {
            return;
        }
        let mut clear = false;
        section_row(ui, "RECENT", |ui| {
            clear = ui.small_button("Clear").on_hover_text("Forget recent computers (favorites stay)").clicked();
        });
        if clear {
            self.recents.retain(|r| r.fav);
            history::save_recents(&self.recents);
            return;
        }
        let n = self.recents.len();
        for i in 0..n {
            let (addr, name, fav, thumb, trusted, saved) = {
                let r = &self.recents[i];
                (r.addr.clone(), r.name.clone(), r.fav, r.thumb.clone(), r.device.len() == 64, !r.password.is_empty())
            };
            let tex = thumb.and_then(|f| self.thumbs.entry(addr.clone()).or_insert_with(|| crate::load_thumb_texture(ui.ctx(), &f)).clone());
            let mut go = false;
            egui::Frame::new()
                .fill(ui.visuals().extreme_bg_color)
                .corner_radius(10.0)
                .inner_margin(8.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        match &tex {
                            Some(t) => {
                                ui.add(egui::Image::new((t.id(), egui::vec2(88.0, 50.0))).corner_radius(6.0));
                            }
                            None => {
                                ui.add_sized([88.0, 50.0], egui::Label::new(RichText::new("🖥").size(22.0)));
                            }
                        }
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&name).strong());
                            let mut info = remote_friend_common::format_id(&addr);
                            if trusted {
                                info.push_str(" · trusted");
                            } else if saved {
                                info.push_str(" · password saved");
                            }
                            ui.label(RichText::new(info).size(12.0).color(MUTED));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Connect").clicked() {
                                go = true;
                            }
                            if ui
                                .add(egui::Button::new(if fav { "★" } else { "☆" }).frame(false))
                                .on_hover_text(if fav { "Remove from favorites" } else { "Keep as favorite" })
                                .clicked()
                            {
                                self.recents[i].fav = !fav;
                                history::save_recents(&self.recents);
                            }
                        });
                    });
                });
            if go {
                self.host_field = addr.clone();
                self.pass_field.clear();
                self.connect();
                return;
            }
        }
    }

    pub(crate) fn settings_ui(&mut self, ctx: &egui::Context, snap: &remote_friend_host::Snapshot) {
        let mut open = true;
        egui::Window::new("Settings")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_width(500.0);
                ui.horizontal(|ui| {
                    for (i, name) in ["Server", "Security", "Privacy", "Connection", "General"].iter().enumerate() {
                        ui.selectable_value(&mut self.settings_tab, i as u8, RichText::new(*name).size(14.0));
                    }
                });
                ui.separator();
                ui.add_space(4.0);
                match self.settings_tab {
                    0 => self.settings_server(ui, snap),
                    1 => self.settings_security(ui, snap),
                    2 => self.settings_privacy(ui),
                    3 => self.settings_connection(ui),
                    _ => self.settings_general(ui),
                }
            });
        if !open {
            self.settings_open = false;
        }
    }

    fn settings_server(&mut self, ui: &mut egui::Ui, snap: &remote_friend_host::Snapshot) {
        use remote_friend_common::identity::{DEFAULT_SERVER, DEFAULT_WEB_URL};
        ui.checkbox(&mut self.settings_use_server, "Reachable over the internet (through a server)")
            .on_hover_text("Off: this computer can only be reached from your local network");
        ui.add_space(6.0);
        ui.add_enabled_ui(self.settings_use_server, |ui| {
            egui::Grid::new("server_grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                ui.label("Access key").on_hover_text("Lets this computer use the server; ask the server owner for one");
                ui.add(egui::TextEdit::singleline(&mut self.settings_key).password(true).hint_text("RF-XXXXX-…").desired_width(340.0));
                ui.end_row();
                ui.label("Server").on_hover_text("Leave empty for the blobidea server; your own relay: host:33202");
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings_server)
                        .hint_text(format!("{DEFAULT_SERVER} (default)"))
                        .desired_width(340.0),
                );
                ui.end_row();
                let web_hint = if self.settings_server.trim().is_empty() { DEFAULT_WEB_URL } else { "https://…" };
                ui.label("Phone address").on_hover_text("Web address phones open, e.g. https://remote.example.com");
                ui.add(egui::TextEdit::singleline(&mut self.settings_web).hint_text(web_hint).desired_width(340.0));
                ui.end_row();
            });
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new("Connecting to other computers needs no key. The access key only makes this computer reachable from anywhere.")
                .size(12.0)
                .color(MUTED),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Save and reconnect").clicked() {
                remote_friend_host::set_server(&self.settings_server, &self.settings_web, &self.settings_key, self.settings_use_server);
                self.flash("Server settings saved");
            }
            // Only the server's owner has a key that is not a personal "RF-…" access key.
            if owner_key_like(&self.settings_key)
                && ui.button("Manage access keys…").on_hover_text("Create, share and revoke keys for other people").clicked()
            {
                self.keys_ui.show();
            }
            match (&snap.server, snap.online, &snap.server_error) {
                (_, true, _) => dot(ui, OK, "Connected"),
                (Some(_), false, Some(e)) if e.starts_with("registration rejected: ") => {
                    let reason = e.trim_start_matches("registration rejected: ");
                    let reason = reason.split(" (RemoteFriend:").next().unwrap_or(reason);
                    dot(ui, WARN, format!("The server says: {reason}"))
                }
                (Some(_), false, Some(e)) => dot(ui, ERR, e.clone()),
                (Some(_), false, None) => dot(ui, WARN, "Connecting…"),
                (None, _, _) => {}
            }
        });
    }

    fn settings_security(&mut self, ui: &mut egui::Ui, snap: &remote_friend_host::Snapshot) {
        use remote_friend_host::PasswordPolicy as P;
        if !snap.password_from_env {
            ui.label(RichText::new("Password changes").strong());
            let mut policy = remote_friend_host::password_policy();
            let mut changed = false;
            changed |= ui.radio_value(&mut policy, P::OnStart, "When RemoteFriend starts").changed();
            changed |= ui.radio_value(&mut policy, P::AfterSession, "After every session").changed();
            changed |= ui.radio_value(&mut policy, P::Never, "Never (only with “New password”)").changed();
            if changed {
                remote_friend_host::set_password_policy(policy);
            }
            if ui.button("New password now").clicked() && remote_friend_host::renew_password().is_some() {
                self.show_password = true;
                self.flash("New password created");
            }
            ui.add_space(10.0);
        }
        ui.label(RichText::new("Trusted devices").strong())
            .on_hover_text("Chosen with “Always allow”: they connect without the password and without asking");
        let devices = remote_friend_host::trusted_devices();
        if devices.is_empty() {
            ui.label(RichText::new("None yet").color(MUTED));
        }
        egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
            for d in &devices {
                ui.horizontal(|ui| {
                    ui.label(&d.label);
                    ui.label(RichText::new(format!("last used {}", remote_friend_host::local_time(d.last_used))).size(12.0).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Remove").clicked() && remote_friend_host::remove_device(&d.handle) {
                            self.flash(format!("{} needs the password again", d.label));
                        }
                    });
                });
            }
        });
        if !devices.is_empty() && ui.button("Remove all").clicked() {
            let n = remote_friend_host::forget_devices();
            self.flash(format!("{n} device(s) need the password again"));
        }
    }

    fn settings_privacy(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Viewers see").strong());
        let mut full = remote_friend_host::privacy::full_view();
        let a = ui.radio_value(&mut full, false, "Safe view — RemoteFriend and private apps hidden");
        let b = ui.radio_value(&mut full, true, "Everything — as if they sat at this computer");
        if a.changed() || b.changed() {
            remote_friend_host::privacy::set_full_view(full);
        }
        ui.add_space(10.0);
        ui.label(RichText::new("Private apps").strong())
            .on_hover_text("Windows whose app or title contains one of these words are blacked out in the safe view (Windows, macOS, X11)");
        ui.add(
            egui::TextEdit::multiline(&mut self.settings_private_apps)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("keepass, bitwarden, banking"),
        );
        ui.horizontal(|ui| {
            if ui.button("Save list").clicked() {
                let list = self.settings_private_apps.split([',', '\n']).map(|s| s.trim().to_string()).collect();
                remote_friend_host::privacy::set_private_apps(list);
                self.flash("Private apps saved");
            }
            ui.label(RichText::new("ⓘ").color(MUTED)).on_hover_text(
                "Some apps hide themselves from every screen capture (e.g. Signal's “Screen security”); turn that off in the app if viewers should see it.",
            );
        });
    }

    fn settings_connection(&mut self, ui: &mut egui::Ui) {
        let mut direct = remote_friend_host::p2p::enabled();
        if ui
            .checkbox(&mut direct, "Direct connections (peer-to-peer) when possible")
            .on_hover_text("Picture, sound and input go straight between the devices instead of through the server — still end-to-end encrypted. The devices learn each other's internet address.")
            .changed()
        {
            remote_friend_host::set_direct_enabled(direct);
        }
        ui.add_space(6.0);
        let mut lan = self.lan_enabled;
        if ui
            .checkbox(&mut lan, "Allow connections from the local network")
            .on_hover_text("Off: this computer is reachable only through the server. The plain-http browser page on the local network is not encrypted.")
            .changed()
        {
            remote_friend_host::set_lan_enabled(lan);
            self.lan_enabled = lan;
            self.lan_changed = true;
        }
        if self.lan_changed {
            ui.horizontal(|ui| {
                ui.colored_label(WARN, "Restart RemoteFriend to apply.");
                if ui.button("Restart now").clicked() {
                    restart_app();
                }
            });
        }
    }

    fn settings_general(&mut self, ui: &mut egui::Ui) {
        let mut auto = self.autostart;
        if ui.checkbox(&mut auto, "Start RemoteFriend when I log in").changed() {
            match autostart::set(auto) {
                Ok(()) => self.autostart = auto,
                Err(e) => self.flash(format!("Could not change: {e}")),
            }
        }
        ui.add_space(6.0);
        let dir = remote_friend_host::receive_dir();
        ui.horizontal(|ui| {
            ui.label("Received files");
            ui.label(RichText::new(dir.display().to_string()).size(12.0).color(MUTED));
            if ui.small_button("Open").clicked() {
                let _ = std::fs::create_dir_all(&dir);
                crate::open_path(&dir);
            }
        });
        ui.add_space(6.0);
        let mut check = crate::update::enabled();
        if ui
            .checkbox(&mut check, "Check for new versions")
            .on_hover_text("Asks blobidea.com twice a day which version is current; nothing else is sent")
            .changed()
        {
            crate::update::set_enabled(check);
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("RemoteFriend {}", env!("CARGO_PKG_VERSION"))).color(MUTED));
            ui.hyperlink_to("blobidea.com/products/remotefriend", crate::update::DOWNLOAD_PAGE);
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Free software (GPL-3.0):").size(12.0).color(MUTED));
            ui.hyperlink_to(RichText::new("source code").size(12.0), "https://github.com/siyahkarga/remotefriend");
            ui.label(RichText::new("· Security problems: support@blobidea.com").size(12.0).color(MUTED));
        });
    }

    /// "A new version is available" strip above the home screen; red for security fixes.
    fn update_banner(&mut self, ctx: &egui::Context) {
        let Some(update) = self.update.lock().unwrap().clone() else {
            return;
        };
        if self.update_dismissed.as_deref() == Some(update.version.as_str()) {
            return;
        }
        let color = if update.security { ERR } else { ACCENT };
        egui::TopBottomPanel::top("update")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 8)).fill(color.gamma_multiply(0.18)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let text = if update.security {
                        format!("Security update: RemoteFriend {} fixes a security problem in this version. Please install it soon.", update.version)
                    } else {
                        format!("RemoteFriend {} is available.", update.version)
                    };
                    ui.label(RichText::new(text).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Later").clicked() {
                            self.update_dismissed = Some(update.version.clone());
                        }
                        if ui.button("Download").clicked() {
                            ctx.open_url(egui::OpenUrl::new_tab(crate::update::DOWNLOAD_PAGE));
                        }
                    });
                });
            });
    }

    fn history_window(&mut self, ctx: &egui::Context) {
        use remote_friend_host::sessions;
        let mut open = true;
        egui::Window::new("History").collapsible(false).resizable(true).default_size([420.0, 320.0]).open(&mut open).show(ctx, |ui| {
            let recent = sessions::recent();
            if recent.is_empty() {
                ui.label(RichText::new("No connections yet").color(MUTED));
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                for r in &recent {
                    ui.horizontal(|ui| {
                        ui.label(&r.label).on_hover_text(&r.peer);
                        ui.label(
                            RichText::new(format!(
                                "{} · {} · {}{}",
                                remote_friend_host::local_time(r.start),
                                remote_friend_host::duration_text(r.secs),
                                if r.app { "app" } else { "browser" },
                                if r.trusted { " · trusted" } else { "" },
                            ))
                            .size(12.0)
                            .color(MUTED),
                        );
                    });
                }
            });
            if !recent.is_empty() && ui.small_button("Clear").clicked() {
                sessions::clear_recent();
            }
        });
        if !open {
            self.history_open = false;
        }
    }
}
