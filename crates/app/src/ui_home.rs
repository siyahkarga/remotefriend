//! The main window: the menu on the left, the update strip, the status line, and the Home page
//! ("This computer" next to "Connect to a computer"). Other pages are in ui_pages.rs.
//!
//! Layout rules: few words on screen, explanations in tooltips, the same building blocks
//! (ui_kit) everywhere, long text shortened instead of wrapped letter by letter.

use egui::{Align, Layout, RichText};
use std::time::{Duration, Instant};

use crate::ui_kit::{self as kit, ACCENT, ERR, FAINT, MUTED, OK, TEXT, WARN};
use crate::{history, restart_app, App, Page};
use remote_friend_host::ScreenState;

/// Below this window width the menu shows icons only.
const COMPACT_WIDTH: f32 = 860.0;

/// The server owner's key (setup: 32 hex characters), as opposed to a personal "RF-…" access key.
pub(crate) fn owner_key_like(key: &str) -> bool {
    let key = key.trim();
    key.len() >= 16 && key.chars().all(|c| c.is_ascii_hexdigit())
}

/// A link that opens the browser viewer with this computer's ID and password filled in. They
/// travel after "#", which browsers never send to the server.
pub(crate) fn invite_link(web_url: &str, id: &str, password: &str) -> String {
    let enc: String = password
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b == b'-' { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    format!("{}/#id={id}&pw={enc}", web_url.trim_end_matches('/'))
}

/// ID and password from a pasted invite link (or None if it isn't one).
pub(crate) fn parse_invite(text: &str) -> Option<(String, String)> {
    let (_, fragment) = text.trim().split_once('#')?;
    let mut id = None;
    let mut pw = String::new();
    for pair in fragment.split('&') {
        match pair.split_once('=') {
            Some(("id", v)) => id = Some(v.chars().filter(|c| c.is_ascii_digit()).collect::<String>()),
            Some(("pw", v)) => {
                let bytes = v.as_bytes();
                let mut out = Vec::new();
                let mut i = 0;
                while i < bytes.len() {
                    match (bytes[i], bytes.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())) {
                        (b'%', Some(b)) => {
                            out.push(b);
                            i += 3;
                        }
                        (b, _) => {
                            out.push(b);
                            i += 1;
                        }
                    }
                }
                pw = String::from_utf8_lossy(&out).into_owned();
            }
            _ => {}
        }
    }
    id.filter(|i| i.len() == 9).map(|i| (i, pw))
}

/// Online / Local network / No access key / Offline, with a tooltip.
fn server_status(snap: &remote_friend_host::Snapshot) -> (egui::Color32, &'static str, String) {
    let refused = snap.server_error.as_deref().and_then(|e| e.strip_prefix("registration rejected: "));
    let (color, text) = match (&snap.server, snap.online, &snap.server_error) {
        (_, true, _) => (OK, "Online"),
        (None, _, _) => (WARN, "Local network"),
        (Some(_), false, Some(_)) if refused.is_some() => (WARN, "No access key"),
        (Some(_), false, Some(_)) => (ERR, "Offline"),
        (Some(_), false, None) => (WARN, "Connecting"),
    };
    let tip = match (&snap.server, &snap.server_error, refused) {
        (_, _, _) if snap.online => "Reachable from anywhere".to_string(),
        (None, _, _) => "Reachable on the local network only (Settings, Server)".to_string(),
        (Some(_), _, Some(reason)) => format!("The server says: {reason}. Other computers can still be reached from here."),
        (Some(_), Some(e), None) => format!("Cannot reach the server: {e}"),
        (Some(_), None, None) => "Connecting to the server".to_string(),
    };
    (color, text, tip)
}

impl App {
    pub(crate) fn open_settings(&mut self, tab: u8) {
        let server = remote_friend_host::server_settings();
        self.settings_server = server.server;
        self.settings_web = server.web_url;
        self.settings_key = server.key;
        self.settings_use_server = server.use_server;
        self.autostart = crate::autostart::is_enabled();
        self.settings_private_apps = remote_friend_host::privacy::private_apps().join(", ");
        self.settings_tab = tab;
        self.page = Page::Settings;
    }

    fn go(&mut self, page: Page, ctx: &egui::Context) {
        match page {
            Page::Settings => self.open_settings(self.settings_tab),
            Page::Keys => {
                self.keys_ui.show();
                crate::notices::refresh(self.notices.clone(), ctx.clone());
                self.page = Page::Keys;
            }
            _ => self.page = page,
        }
    }

    pub(crate) fn home_ui(&mut self, ctx: &egui::Context) {
        self.lan.lock().unwrap().retain(|e| e.last_seen.elapsed().as_secs() < 8);
        let snap = remote_friend_host::snapshot();
        self.load_logo(ctx);
        ctx.request_repaint_after(Duration::from_millis(500));
        // The access-key page belongs to the server owner only.
        if self.page == Page::Keys && !owner_key_like(&remote_friend_host::server_settings().key) {
            self.page = Page::Home;
        }

        self.sidebar(ctx, &snap);
        self.update_banner(ctx);
        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(20, 7)).fill(kit::BG))
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let text = match snap.notices.last() {
                        Some(n) => format!("{}  {}", n.time, n.text),
                        None => "Keep RemoteFriends open to allow remote access.".to_string(),
                    };
                    kit::line(ui, RichText::new(text).size(12.0).color(FAINT));
                });
            });
        egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(egui::Margin::same(20)).fill(kit::BG)).show(ctx, |ui| {
            match self.page {
                Page::Home => self.home_page(ui, &snap),
                Page::Devices => self.devices_page(ui),
                Page::History => self.history_page(ui),
                Page::Keys => self.keys_page(ui),
                Page::Settings => self.settings_page(ui, &snap),
            }
        });
        self.confirm_mine_ui(ctx);
        self.notices_ui(ctx);
    }

    /// The menu on the left: logo, pages, and the connection status at the bottom.
    fn sidebar(&mut self, ctx: &egui::Context, snap: &remote_friend_host::Snapshot) {
        let compact = ctx.screen_rect().width() < COMPACT_WIDTH;
        let owner = owner_key_like(&self.settings_key) || owner_key_like(&remote_friend_host::server_settings().key);
        let devices = snap.trusted_devices;
        egui::SidePanel::left("nav")
            .exact_width(if compact { 64.0 } else { 212.0 })
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(kit::SIDEBAR).inner_margin(egui::Margin::symmetric(if compact { 10 } else { 14 }, 16)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(logo) = &self.logo {
                        ui.image((logo.id(), egui::vec2(30.0, 30.0)));
                    }
                    if !compact {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label(RichText::new("RemoteFriends").size(16.0).color(TEXT).strong());
                            ui.label(RichText::new(format!("version {}", env!("CARGO_PKG_VERSION"))).size(11.0).color(FAINT));
                        });
                    }
                });
                ui.add_space(22.0);
                let mut items: Vec<(Page, &str, &str, Option<usize>)> = vec![
                    (Page::Home, "🏠", "Home", None),
                    (Page::Devices, "🛡", "Devices", Some(devices)),
                    (Page::History, "🕘", "History", None),
                ];
                if owner {
                    let unseen = self.notices.lock().unwrap().unseen;
                    items.push((Page::Keys, "🔑", "Access keys", Some(unseen)));
                }
                items.push((Page::Settings, "⚙", "Settings", None));
                for (page, icon, label, count) in items {
                    if self.nav_item(ui, page, icon, label, count, compact).clicked() && self.page != page {
                        self.go(page, ctx);
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    let (color, text, tip) = server_status(snap);
                    let resp = if compact {
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(44.0, 26.0), egui::Sense::click());
                        ui.painter().circle_filled(rect.center(), 6.0, color);
                        resp
                    } else {
                        kit::status_pill(ui, color, text)
                    };
                    if resp.on_hover_text(tip).clicked() {
                        self.open_settings(0);
                    }
                    if let Some((text, t)) = &self.flash {
                        if t.elapsed() < Duration::from_secs(4) && !compact {
                            ui.add_space(4.0);
                            ui.add(egui::Label::new(RichText::new(text).size(12.5).color(OK)).wrap());
                        }
                    }
                });
            });
    }

    fn nav_item(&self, ui: &mut egui::Ui, page: Page, icon: &str, label: &str, count: Option<usize>, compact: bool) -> egui::Response {
        let selected = self.page == page;
        let size = egui::vec2(ui.available_width(), 38.0);
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, kit::R_ROW, kit::tint(ACCENT, 0.18));
        } else if resp.hovered() {
            painter.rect_filled(rect, kit::R_ROW, kit::SURFACE_HI);
        }
        let color = if selected { TEXT } else { MUTED };
        let icon_x = if compact { rect.center().x } else { rect.left() + 20.0 };
        painter.text(egui::pos2(icon_x, rect.center().y), egui::Align2::CENTER_CENTER, icon, egui::FontId::proportional(16.0), color);
        if !compact {
            painter.text(egui::pos2(rect.left() + 40.0, rect.center().y), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(14.5), color);
            if let Some(n) = count.filter(|n| *n > 0) {
                // Unread notices stand out; device counts are plain.
                let color = if page == Page::Keys { ACCENT } else { FAINT };
                painter.text(egui::pos2(rect.right() - 12.0, rect.center().y), egui::Align2::RIGHT_CENTER, n.to_string(), egui::FontId::proportional(12.5), color);
            }
        }
        ui.add_space(2.0);
        if compact { resp.on_hover_text(label) } else { resp }
    }

    /// "A new version is available" strip above the page (red for security fixes): Update now,
    /// progress while it installs, and the notice before an automatic install.
    fn update_banner(&mut self, ctx: &egui::Context) {
        let Some(update) = self.update.lock().unwrap().clone() else {
            return;
        };
        let progress = self.update_progress.lock().unwrap().clone();
        let busy = !matches!(progress, crate::update::Progress::Idle | crate::update::Progress::Failed(_));
        if self.update_dismissed.as_deref() == Some(update.version.as_str()) && !busy {
            return;
        }
        // Automatic: only when nobody is connected either way and no password is needed.
        const NOTICE: Duration = Duration::from_secs(30);
        let quiet = remote_friend_host::sessions::count() == 0 && remote_friend_host::pending_prompts().is_empty();
        let auto = update.install.as_ref().is_some_and(|i| !i.needs_password) && crate::update::automatic() && quiet;
        if auto && matches!(progress, crate::update::Progress::Idle) {
            let started = *self.update_countdown.get_or_insert_with(Instant::now);
            if started.elapsed() >= NOTICE {
                self.update_countdown = None;
                crate::update::install(&update, self.update_progress.clone(), ctx.clone());
            }
            ctx.request_repaint_after(Duration::from_millis(500));
        } else {
            self.update_countdown = None;
        }
        let color = if update.security { ERR } else { ACCENT };
        let countdown = self.update_countdown;
        egui::TopBottomPanel::top("update")
            .frame(egui::Frame::new().inner_margin(egui::Margin { left: 20, right: 20, top: 14, bottom: 0 }).fill(kit::BG))
            .show_separator_line(false)
            .show(ctx, |ui| {
                kit::callout(ui, color, |ui| {
                    kit::split(
                        ui,
                        32.0,
                        |ui| {
                            use crate::update::Progress;
                            let text = match &progress {
                                Progress::Downloading(got, total) if *total > 0 => {
                                    format!("Downloading RemoteFriends {}… {} %", update.version, got * 100 / total)
                                }
                                Progress::Downloading(..) => format!("Downloading RemoteFriends {}…", update.version),
                                Progress::Checking => "Checking the download…".to_string(),
                                Progress::Installing if update.install.as_ref().is_some_and(|i| i.needs_password) => {
                                    "Installing — enter your computer's password in the window that opens.".to_string()
                                }
                                Progress::Installing => "Installing… RemoteFriends restarts by itself.".to_string(),
                                Progress::Failed(e) => format!("The update failed: {e}"),
                                Progress::Idle => match countdown {
                                    Some(t) => format!(
                                        "RemoteFriends {} is installed automatically in {} s.",
                                        update.version,
                                        NOTICE.saturating_sub(t.elapsed()).as_secs()
                                    ),
                                    None if update.security => format!(
                                        "Security update: RemoteFriends {} fixes a security problem in this version. Please install it soon.",
                                        update.version
                                    ),
                                    None => format!("RemoteFriends {} is available.", update.version),
                                },
                            };
                            ui.add(egui::Label::new(RichText::new(text).color(TEXT).strong()).wrap());
                        },
                        |ui| {
                            use crate::update::Progress;
                            match &progress {
                                Progress::Idle | Progress::Failed(_) => {
                                    if ui.add(kit::quiet(if countdown.is_some() { "Not now" } else { "Later" })).clicked() {
                                        self.update_dismissed = Some(update.version.clone());
                                        self.update_countdown = None;
                                    }
                                    let failed = matches!(progress, Progress::Failed(_));
                                    if update.install.is_some() && !failed {
                                        if ui.add(kit::primary("Update now")).clicked() {
                                            self.update_countdown = None;
                                            crate::update::install(&update, self.update_progress.clone(), ctx.clone());
                                        }
                                    } else if ui.add(kit::primary("Download")).clicked() {
                                        crate::open_url(crate::update::DOWNLOAD_PAGE);
                                    }
                                }
                                Progress::Downloading(got, total) => {
                                    let frac = if *total > 0 { *got as f32 / *total as f32 } else { 0.0 };
                                    ui.add(egui::ProgressBar::new(frac).desired_width(160.0));
                                }
                                Progress::Checking | Progress::Installing => {
                                    ui.spinner();
                                }
                            }
                        },
                    );
                });
            });
    }

    // ---- Home page ----

    fn home_page(&mut self, ui: &mut egui::Ui, snap: &remote_friend_host::Snapshot) {
        let host_error = self.host_error.lock().unwrap().clone();
        if ui.available_width() >= 720.0 {
            ui.columns(2, |cols| {
                egui::ScrollArea::vertical().id_salt("this").auto_shrink([false, false]).show(&mut cols[0], |ui| {
                    kit::card(ui, |ui| self.this_computer_ui(ui, snap, host_error.as_deref()));
                });
                egui::ScrollArea::vertical().id_salt("connect").auto_shrink([false, false]).show(&mut cols[1], |ui| {
                    kit::card(ui, |ui| self.connect_ui(ui));
                });
            });
        } else {
            // Narrow window: one panel at a time.
            kit::segmented(ui, &mut self.narrow_tab, &[(0u8, "This computer", ""), (1u8, "Connect", "")]);
            ui.add_space(10.0);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if self.narrow_tab == 0 {
                    kit::card(ui, |ui| self.this_computer_ui(ui, snap, host_error.as_deref()));
                } else {
                    kit::card(ui, |ui| self.connect_ui(ui));
                }
            });
        }
    }

    fn card_title(ui: &mut egui::Ui, title: &str) {
        ui.label(RichText::new(title).size(17.0).color(TEXT).strong());
    }

    fn this_computer_ui(&mut self, ui: &mut egui::Ui, snap: &remote_friend_host::Snapshot, host_error: Option<&str>) {
        Self::card_title(ui, "This computer");
        if let Some(err) = host_error {
            ui.add_space(8.0);
            kit::callout(ui, ERR, |ui| {
                ui.label(RichText::new("Sharing this computer is not available").color(TEXT).strong());
                kit::note(ui, err);
                if err.contains("already running") || err.contains("33200") {
                    kit::note(ui, "Another RemoteFriends seems to be running (maybe in a terminal). Close it, then restart this app.");
                    if ui.add(kit::secondary("Restart RemoteFriends")).clicked() {
                        restart_app();
                    }
                }
            });
            return;
        }
        if snap.id.is_empty() {
            ui.spinner();
            return;
        }

        kit::section_label(ui, "YOUR ID");
        ui.horizontal(|ui| {
            let id = if self.hide_id { "••• ••• •••".to_string() } else { remote_friend_common::format_id(&snap.id) };
            ui.label(RichText::new(id).size(30.0).color(TEXT).monospace());
            let (icon, tip) = if self.hide_id { (kit::Icon::Eye, "Show the ID") } else { (kit::Icon::EyeOff, "Hide the ID") };
            if kit::icon(ui, icon, tip).clicked() {
                self.hide_id = !self.hide_id;
            }
            self.copy_icon(ui, &snap.id, "ID", "Copy the ID");
        });

        kit::section_label(ui, "PASSWORD");
        ui.horizontal(|ui| {
            let shown = if snap.password_from_env {
                "set by REMOTE_FRIEND_PASS".to_string()
            } else if self.show_password {
                snap.password.clone()
            } else {
                "•••••-•••••".to_string()
            };
            ui.label(RichText::new(shown).size(21.0).color(TEXT).monospace());
            if !snap.password_from_env {
                let (icon, tip) = if self.show_password { (kit::Icon::EyeOff, "Hide the password") } else { (kit::Icon::Eye, "Show the password") };
                if kit::icon(ui, icon, tip).clicked() {
                    self.show_password = !self.show_password;
                }
                self.copy_icon(ui, &snap.password, "Password", "Copy the password");
                if kit::icon(ui, kit::Icon::Refresh, "New password now (the old one stops working)").clicked()
                    && remote_friend_host::renew_password().is_some()
                {
                    self.show_password = true;
                    self.flash("New password created");
                }
            }
        });
        if !snap.password_from_env {
            let policy = match remote_friend_host::password_policy() {
                remote_friend_host::PasswordPolicy::OnStart => "Changes when RemoteFriends starts.",
                remote_friend_host::PasswordPolicy::AfterSession => "Changes after every session.",
                remote_friend_host::PasswordPolicy::Never => "Stays the same until you change it.",
            };
            kit::note(ui, format!("{policy} Trusted devices don't need it."))
                .on_hover_text("Settings, Security. Devices you chose “Always allow” for log in with their own key.");
        }

        ui.add_space(12.0);
        kit::divider(ui);
        ui.add_space(4.0);
        let refused = snap.server_error.as_deref().is_some_and(|e| e.starts_with("registration rejected"));
        match (&snap.server, snap.online) {
            (_, true) => {
                kit::dot(ui, OK, "Reachable from anywhere");
            }
            (None, _) => {
                ui.horizontal(|ui| {
                    kit::dot(ui, WARN, "Local network only");
                    if ui.add(kit::quiet("Settings…")).clicked() {
                        self.open_settings(0);
                    }
                });
            }
            (Some(_), false) if refused => {
                ui.horizontal(|ui| {
                    kit::dot(ui, WARN, "Not reachable from anywhere yet");
                    if ui.add(kit::primary("Sign in with email")).on_hover_text("Free: your email address and a 6-digit code").clicked() {
                        self.welcome = Some(crate::welcome::Welcome::new());
                    }
                });
            }
            (Some(_), false) => {
                kit::dot(ui, WARN, "Connecting to the server…");
            }
        }
        match snap.screen {
            ScreenState::Ready => {}
            ScreenState::WaitingPermission => {
                kit::callout(ui, WARN, |ui| {
                    ui.label(RichText::new("Allow screen sharing once").color(TEXT).strong());
                    kit::note(ui, "In the desktop's window: turn on “Allow Remote Interaction”, pick the screen(s), click Share.");
                });
            }
            ScreenState::Denied | ScreenState::NoInput => {
                ui.horizontal(|ui| {
                    let text = if snap.screen == ScreenState::Denied {
                        "Screen sharing is not allowed"
                    } else {
                        "Mouse and keyboard are not allowed"
                    };
                    kit::dot(ui, ERR, text);
                    if ui.add(kit::quiet("Ask again")).clicked() && !remote_friend_host::retry_screen_permission() {
                        restart_app();
                    }
                });
            }
        }
        // The invite link only works once this computer is reachable (it has a valid key).
        if let Some(url) = snap.web_url.as_ref().filter(|_| snap.online) {
            let link = invite_link(url, &snap.id, &snap.password);
            kit::section_label(ui, "INVITE LINK");
            ui.horizontal(|ui| {
                let shown = format!("{}/#id={}&pw=•••", url.trim_start_matches("https://").trim_end_matches('/'), snap.id);
                kit::line(ui, RichText::new(shown).size(14.0).color(ACCENT)).on_hover_text(
                    "Opens RemoteFriends in any browser (phone or computer) with this computer's ID and password filled in",
                );
                self.copy_icon(ui, &link, "Invite link", "Copy the invite link");
            });
            kit::note(ui, "Send it to whoever should connect: no ID or password to type. You still approve them; it stops working when the password changes.");
        }
        if snap.auto_accept {
            kit::dot(ui, WARN, "Approval is off (REMOTE_FRIEND_AUTO_ACCEPT=1)");
        }

        kit::section_label(ui, "VIEWERS SEE");
        let mut full = remote_friend_host::privacy::full_view();
        let wayland = crate::privacy::wayland();
        let safe_tip = if wayland {
            "This Linux desktop (Wayland) doesn't let apps hide windows: viewers see RemoteFriends and every app (your password stays hidden as dots)"
        } else {
            "RemoteFriends and private apps (Settings, Privacy) are blacked out; viewers can't use RemoteFriends"
        };
        if kit::segmented(
            ui,
            &mut full,
            &[(false, "🛡 Safe view", safe_tip), (true, "🖥 Everything", "Exactly what you see, as if they sat here")],
        ) {
            remote_friend_host::privacy::set_full_view(full);
        }
        if full {
            kit::note(ui, "Nothing is hidden from viewers now: hidden apps (Settings, Privacy) only work in Safe view.");
        }
        if wayland && !full {
            kit::note(ui, "On this Linux desktop (Wayland) private apps can't be hidden: close them while someone watches, or log in with an X11 (Xorg) session.");
        }

        self.sessions_ui(ui);
        self.shared_screen_ui(ui, snap);
    }

    /// Who is connected right now, with their permissions.
    fn sessions_ui(&mut self, ui: &mut egui::Ui) {
        use remote_friend_host::sessions;
        let live = sessions::list();
        if live.is_empty() {
            return;
        }
        ui.add_space(6.0);
        kit::section_header(ui, &format!("CONNECTED NOW ({})", live.len()), |ui| {
            let blocked = sessions::input_blocked();
            let button = if blocked { kit::primary("▶ Give control back") } else { kit::secondary("✋ Take back control") };
            if ui
                .add(button)
                .on_hover_text("Stops everyone's mouse and keyboard until you click again; they can still watch")
                .clicked()
            {
                sessions::set_input_blocked(!blocked);
            }
        });
        for s in live {
            egui::Frame::new().fill(kit::SURFACE_HI).corner_radius(kit::R_ROW).inner_margin(12).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let via = if s.via == sessions::Via::App { "app" } else { "browser" };
                let path = if s.direct { "direct" } else { "via server" };
                let trusted = format!(" · {}", s.access.word());
                let mut switch = false;
                kit::split(
                    ui,
                    kit::ROW_H,
                    |ui| kit::row_text(ui, &s.label, &format!("{via} · since {} · {path}{trusted}", remote_friend_host::local_time(s.since))),
                    |ui| {
                        if ui.add(kit::danger("Disconnect")).clicked() {
                            sessions::disconnect(s.id);
                        }
                        switch = s.can_switch
                            && ui.add(kit::secondary("Switch sides")).on_hover_text("Control their computer instead; they are asked first").clicked();
                    },
                );
                if switch {
                    sessions::request_switch(s.id);
                    self.flash("Asked the other side to switch");
                }
                let mut p = s.perms;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let toggle = |ui: &mut egui::Ui, on: &mut bool, word: &str, tip: &str| {
                        let text = RichText::new(word).size(13.5).color(if *on { egui::Color32::WHITE } else { FAINT });
                        let fill = if *on { kit::tint(ACCENT, 0.55) } else { kit::SURFACE };
                        if ui
                            .add(egui::Button::new(text).fill(fill).corner_radius(kit::R_BTN).min_size(egui::vec2(38.0, 30.0)))
                            .on_hover_text(format!("{tip}: {}", if *on { "allowed (click to turn off)" } else { "off (click to allow)" }))
                            .clicked()
                        {
                            *on = !*on;
                        }
                    };
                    toggle(ui, &mut p.control, "Control", "Mouse & keyboard");
                    toggle(ui, &mut p.sound, "Sound", "Sound");
                    toggle(ui, &mut p.files, "Files", "Sending and taking files");
                    toggle(ui, &mut p.clipboard, "Clipboard", "Shared clipboard");
                });
                if p != s.perms {
                    sessions::set_perms(s.id, p);
                }
                if let Some(t) = &s.transfer {
                    let verb = if t.incoming { "Receiving" } else { "Sending" };
                    kit::transfer(ui, verb, &t.name, t.got, t.total, false);
                }
                // Every file that came or went in this session, with where it is.
                for f in &s.file_log {
                    ui.horizontal(|ui| {
                        let (arrow, verb, color) = if f.incoming { ("⬇", "received", OK) } else { ("⬆", "took", WARN) };
                        kit::line(ui, RichText::new(format!("{arrow} {verb} {}", f.name)).size(12.5).color(color)).on_hover_text(&f.path);
                        if f.incoming && ui.add(kit::quiet("Show")).on_hover_text(&f.path).clicked() {
                            if let Some(folder) = std::path::Path::new(&f.path).parent() {
                                crate::open_path(folder);
                            }
                        }
                    });
                }
            });
        }
    }

    /// The screens of this computer: which one viewers see now, which ones they may switch to
    /// by themselves, and a small live preview while someone watches.
    fn shared_screen_ui(&mut self, ui: &mut egui::Ui, snap: &remote_friend_host::Snapshot) {
        let watching = snap.sessions > 0;
        if self.monitors.1.is_none_or(|t| t.elapsed() > Duration::from_secs(3)) {
            self.monitors = (remote_friend_host::monitors(), Some(Instant::now()));
        }
        let monitors = self.monitors.0.clone();
        if !watching && monitors.len() < 2 {
            return;
        }
        if monitors.len() > 1 {
            let current = remote_friend_host::current_monitor();
            kit::section_header(ui, "SCREENS", |ui| {
                #[cfg(target_os = "linux")]
                if crate::privacy::wayland()
                    && ui
                        .add(kit::quiet("Choose…"))
                        .on_hover_text("Open the desktop's sharing dialog again to pick screens (RemoteFriends restarts)")
                        .clicked()
                    && !remote_friend_host::retry_screen_permission()
                {
                    restart_app();
                }
            });
            for m in &monitors {
                let shown = m.index == current;
                let mut open = remote_friend_host::viewer_may_switch(m.index);
                let detail = format!(
                    "{}{}×{}{}",
                    if m.name.starts_with("Screen ") { String::new() } else { format!("{} · ", m.name) },
                    m.width,
                    m.height,
                    if m.primary { " · primary" } else { "" }
                );
                let mut show = false;
                let mut toggled = false;
                kit::row(
                    ui,
                    |ui| kit::row_text(ui, &format!("🖥  Screen {}", m.index + 1), &detail),
                    |ui| {
                        toggled = kit::switch(ui, &mut open)
                            .on_hover_text(if open {
                                "Viewers may switch to this screen by themselves (click: only with your OK)"
                            } else {
                                "Viewers have to ask you before they see this screen (click: let them switch freely)"
                            })
                            .changed();
                        ui.add_space(6.0);
                        if shown {
                            kit::badge(ui, if watching { "shown now" } else { "will be shown" }, OK);
                        } else {
                            show = ui.add(kit::secondary("Show")).on_hover_text("Viewers see this screen from now on").clicked();
                        }
                    },
                );
                if toggled {
                    remote_friend_host::set_viewer_may_switch(m.index, open);
                    self.monitors.1 = None;
                }
                if show {
                    remote_friend_host::select_monitor(m.index);
                    self.monitors.1 = None;
                }
            }
            kit::note(ui, "Switch on: viewers may change to that screen by themselves. Off: they have to ask you.");
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
                kit::section_label(ui, "WHAT VIEWERS SEE");
                let max_w = ui.available_width().min(300.0);
                let size = t.size_vec2() * (max_w / t.size_vec2().x);
                ui.add(egui::Image::new((t.id(), size)).corner_radius(8.0)).on_hover_text("What viewers see right now");
            }
        } else {
            self.preview_tex = None;
        }
    }

    fn connect_ui(&mut self, ui: &mut egui::Ui) {
        Self::card_title(ui, "Connect to a computer");
        kit::section_label(ui, "COMPUTER ID OR ADDRESS");
        let id_resp = ui.add(kit::input(&mut self.host_field, "123 456 789 or an invite link").font(egui::TextStyle::Heading).desired_width(f32::INFINITY));
        // A pasted invite link fills in both fields.
        if let Some((id, pw)) = parse_invite(&self.host_field) {
            self.host_field = id;
            self.pass_field = pw;
            self.flash("Invite link: ID and password filled in");
        }
        kit::section_label(ui, "PASSWORD");
        let trusted = history::device_for(&self.recents, &self.host_field).is_some();
        let saved = history::password_for(&self.recents, &self.host_field).is_some();
        let hint = if trusted {
            "not needed — this device is trusted"
        } else if saved {
            "saved — leave empty to use it"
        } else {
            "shown on that computer"
        };
        let pw_resp = ui
            .horizontal(|ui| {
                let width = (ui.available_width() - 36.0).max(80.0);
                let resp = ui.add(kit::input(&mut self.pass_field, hint).password(!self.show_pass_field).desired_width(width));
                let (icon, tip) = if self.show_pass_field { (kit::Icon::EyeOff, "Hide the password") } else { (kit::Icon::Eye, "Show the password") };
                if kit::icon(ui, icon, tip).clicked() {
                    self.show_pass_field = !self.show_pass_field;
                }
                resp
            })
            .inner;
        let enter = (id_resp.lost_focus() || pw_resp.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.add_space(10.0);
        if kit::wide(ui, kit::primary("Connect"), 40.0).clicked() || enter {
            self.connect();
        }
        if !self.login_msg.is_empty() {
            ui.add_space(4.0);
            kit::dot(ui, WARN, self.login_msg.clone());
        }

        let my_name = remote_friend_host::snapshot().name;
        let lan: Vec<crate::net::LanEntry> =
            self.lan.lock().unwrap().iter().filter(|e| e.name != my_name).cloned().collect();
        if !lan.is_empty() {
            kit::section_label(ui, "ON THIS NETWORK");
            for e in lan {
                let mut go = false;
                kit::row(
                    ui,
                    |ui| {
                        kit::row_text(ui, &format!("🖥  {}", e.name), &e.ip);
                    },
                    |ui| go = ui.add(kit::secondary("Connect")).clicked(),
                );
                if go {
                    self.host_field = format!("{}:{}", e.ip, e.port);
                    self.connect();
                }
            }
        }

        if self.recents.is_empty() {
            return;
        }
        let mut clear = false;
        kit::section_header(ui, "RECENT", |ui| {
            clear = ui.add(kit::quiet("Clear")).on_hover_text("Forget recent computers (favorites stay)").clicked();
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
            let mut toggle_fav = false;
            let id = remote_friend_common::format_id(&addr);
            let title = if name.is_empty() { id.clone() } else { name.clone() };
            let mut info = if name.is_empty() { String::new() } else { id };
            let tag = if trusted { "trusted" } else if saved { "password saved" } else { "" };
            if !tag.is_empty() {
                if !info.is_empty() {
                    info.push_str(" · ");
                }
                info.push_str(tag);
            }
            kit::row(
                ui,
                |ui| {
                    match &tex {
                        Some(t) => {
                            ui.add(egui::Image::new((t.id(), egui::vec2(80.0, 46.0))).corner_radius(6.0));
                        }
                        None => {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(80.0, 46.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 6.0, kit::SURFACE);
                            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "🖥", egui::FontId::proportional(20.0), FAINT);
                        }
                    }
                    ui.add_space(4.0);
                    kit::row_text(ui, &title, &info);
                },
                |ui| {
                    go = ui.add(kit::secondary("Connect")).clicked();
                    toggle_fav = kit::icon_button(ui, if fav { "★" } else { "☆" }, if fav { "Remove from favorites" } else { "Keep as favorite" })
                        .clicked();
                },
            );
            if toggle_fav {
                self.recents[i].fav = !fav;
                history::save_recents(&self.recents);
            }
            if go {
                self.host_field = addr.clone();
                self.pass_field.clear();
                self.connect();
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invite_link_carries_id_and_password() {
        let link = invite_link("https://remote.example.com/", "411747276", "ab3d9-x7Kq2");
        assert_eq!(link, "https://remote.example.com/#id=411747276&pw=ab3d9-x7Kq2");
        assert_eq!(parse_invite(&link), Some(("411747276".into(), "ab3d9-x7Kq2".into())));
        assert_eq!(parse_invite(&invite_link("https://x", "123456789", "a b&c")), Some(("123456789".into(), "a b&c".into())));
        assert_eq!(parse_invite("411 747 276"), None);
        assert_eq!(parse_invite("https://x/#id=12"), None);
    }
}
