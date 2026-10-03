//! RemoteFriend desktop app: share this computer and control other computers.
//!
//! One window, like AnyDesk/TeamViewer: the left side shows this computer's ID and
//! password (others connect to it), the right side connects to another computer.
//! The host engine runs in the background while the app is open.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod autostart;
mod history;
mod net;
mod viewer;

use anyhow::Result;
use egui::{Color32, RichText};
use history::RecentEntry;
use net::{LanEntry, Shared};
use remote_friend_common::Packet;
use remote_friend_host::{Answer, Decision, PromptKind, ScreenState};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{channel, Sender};

const ICON_PNG: &[u8] = include_bytes!("../../../assets/icon-256.png");
const ACCENT: Color32 = Color32::from_rgb(91, 132, 255);
const OK: Color32 = Color32::from_rgb(47, 191, 113);
const WARN: Color32 = Color32::from_rgb(245, 165, 36);
const ERR: Color32 = Color32::from_rgb(229, 72, 77);

enum Screen {
    Home,
    Viewer,
}

pub(crate) struct App {
    screen: Screen,
    // "Control another computer"
    host_field: String,
    pass_field: String,
    login_msg: String,
    // viewer (after connect)
    shared: Option<Arc<Mutex<Shared>>>,
    tx_out: Option<Sender<Packet>>,
    disconnect_tx: Option<tokio::sync::watch::Sender<bool>>,
    last_mouse: Option<(u32, u32)>,
    mods: [bool; 4], // shift, ctrl, alt, meta (currently pressed)
    buttons_down: [bool; 3],
    /// Letters pressed together with a modifier (their release is always sent).
    chars_down: std::collections::HashSet<char>,
    scroll_acc: (f32, f32),
    lan: Arc<Mutex<Vec<LanEntry>>>,
    recents: Vec<RecentEntry>,
    current_addr: String,
    current_name: String,
    thumb_saved: bool,
    screen_tex: Option<egui::TextureHandle>,
    thumbs: HashMap<String, Option<egui::TextureHandle>>,
    // "This computer"
    host_error: Arc<Mutex<Option<String>>>,
    show_password: bool,
    settings_open: bool,
    settings_server: String,
    settings_web: String,
    autostart: bool,
    last_prompt_seq: u64,
    flash: Option<(String, Instant)>,
    logo: Option<egui::TextureHandle>,
}

fn load_icon() -> egui::IconData {
    let img = image::load_from_memory(ICON_PNG).expect("embedded icon").to_rgba8();
    let (w, h) = img.dimensions();
    egui::IconData { rgba: img.into_raw(), width: w, height: h }
}

fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.visuals.selection.bg_fill = ACCENT;
        s.visuals.hyperlink_color = ACCENT;
        s.visuals.widgets.hovered.corner_radius = 8.into();
        s.visuals.widgets.inactive.corner_radius = 8.into();
        s.visuals.widgets.active.corner_radius = 8.into();
        for (_, f) in s.text_styles.iter_mut() {
            f.size *= 1.08;
        }
    });
}

fn main() -> Result<()> {
    remote_friend_common::tls::init_crypto();
    remote_friend_host::init_logging();

    // Host engine (share this computer) runs in the background while the app is open.
    remote_friend_host::enable_ui_prompts();
    let host_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    {
        let he = host_error.clone();
        std::thread::Builder::new().name("rf-host".into()).spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    *he.lock().unwrap() = Some(format!("cannot start: {e}"));
                    return;
                }
            };
            let opts = remote_friend_host::RunOptions { new_password: false, terminal: false };
            if let Err(e) = rt.block_on(remote_friend_host::run(opts)) {
                *he.lock().unwrap() = Some(format!("{e:#}"));
            }
        })?;
    }

    // LAN discovery: computers on this network show up in the list.
    let lan: Arc<Mutex<Vec<LanEntry>>> = Arc::new(Mutex::new(vec![]));
    let (disc_tx, disc_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || remote_friend_common::discovery::listen_loop(disc_tx));
    let lan_pump = lan.clone();
    std::thread::spawn(move || {
        while let Ok((ip, b)) = disc_rx.recv() {
            let mut v = lan_pump.lock().unwrap();
            if let Some(e) = v.iter_mut().find(|e| e.ip == ip && e.port == b.port) {
                e.name = b.name.clone();
                e.last_seen = Instant::now();
            } else {
                v.push(LanEntry { ip, name: b.name.clone(), port: b.port, last_seen: Instant::now() });
            }
        }
    });

    let (server, web) = remote_friend_host::server_settings();
    let app = App {
        screen: Screen::Home,
        host_field: std::env::args().nth(1).unwrap_or_default(),
        pass_field: std::env::args().nth(2).unwrap_or_default(),
        login_msg: String::new(),
        shared: None,
        tx_out: None,
        disconnect_tx: None,
        last_mouse: None,
        mods: [false; 4],
        buttons_down: [false; 3],
        chars_down: Default::default(),
        scroll_acc: (0.0, 0.0),
        lan,
        recents: history::load_recents(),
        current_addr: String::new(),
        current_name: String::new(),
        thumb_saved: false,
        screen_tex: None,
        thumbs: HashMap::new(),
        host_error,
        show_password: false,
        settings_open: false,
        settings_server: server,
        settings_web: web,
        autostart: autostart::is_enabled(),
        last_prompt_seq: 0,
        flash: None,
        logo: None,
    };

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RemoteFriend")
            .with_app_id("remotefriend")
            .with_inner_size([1060.0, 680.0])
            .with_min_inner_size([780.0, 540.0])
            .with_icon(load_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "RemoteFriend",
        opts,
        Box::new(|cc| {
            setup_style(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("window: {e}"))?;
    Ok(())
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Incoming connection requests must be answerable from any screen.
        self.prompts_ui(ctx);
        match self.screen {
            Screen::Home => self.home_ui(ctx),
            Screen::Viewer => self.viewer_ui(ctx),
        }
    }
}

impl App {
    /// Connect button: start the network thread and switch to the viewer.
    /// 9 digits = computer ID (via the relay server), otherwise a LAN address.
    fn connect(&mut self) {
        let host = self.host_field.trim().to_string();
        let password = self.pass_field.clone();
        if host.is_empty() {
            self.login_msg = "Enter a computer ID or address.".into();
            return;
        }
        let digits: String = host.chars().filter(|c| c.is_ascii_digit()).collect();
        let via_rv = digits.len() == 9 && !host.contains(':') && !host.contains('.');
        let server = if via_rv {
            std::env::var("RF_RV_SERVER").ok().filter(|s| !s.is_empty()).or_else(|| {
                let c = remote_friend_common::identity::load_rv_config();
                if c.server.is_empty() { None } else { Some(c.server) }
            })
        } else {
            None
        };
        if via_rv && server.is_none() {
            self.login_msg = "To connect by ID, set the server first (Settings).".into();
            return;
        }
        let host = if via_rv || host.contains(':') {
            host
        } else {
            format!("{host}:{}", remote_friend_common::DEFAULT_PORT)
        };
        tracing::info!("connecting to {host}");
        let shared = Arc::new(Mutex::new(Shared {
            texture: None,
            status: "Connecting...".into(),
            host_w: 0,
            host_h: 0,
            fps: 0.0,
            tofu_pending: None,
            tofu_answer: None,
            failed: false,
        }));
        let (tx_out, rx_out) = channel::<Packet>(256);
        let (dc_tx, dc_rx) = tokio::sync::watch::channel(false);
        let sh = shared.clone();
        let host_c = host.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
            rt.block_on(async move {
                let r = if via_rv {
                    let fp = std::env::var("RF_RV_FP").ok().filter(|s| !s.is_empty()).or_else(|| {
                        let c = remote_friend_common::identity::load_rv_config();
                        if c.fp.is_empty() { None } else { Some(c.fp) }
                    });
                    net::net_loop_rv(&server.unwrap(), fp, &digits, &password, sh.clone(), rx_out, dc_rx).await
                } else {
                    net::net_loop(&host_c, &password, sh.clone(), rx_out, dc_rx).await
                };
                if let Err(e) = r {
                    let mut s = sh.lock().unwrap();
                    if !s.failed {
                        s.status = format!("Error: {e:#}");
                        s.failed = true;
                    }
                    tracing::warn!("connection closed: {e:#}");
                }
            });
        });
        let known_name = self
            .lan
            .lock()
            .unwrap()
            .iter()
            .find(|e| format!("{}:{}", e.ip, e.port) == host)
            .map(|e| e.name.clone())
            .unwrap_or_default();
        history::touch_recent(&mut self.recents, &host, &known_name);
        self.current_addr = host;
        self.current_name = known_name;
        self.thumb_saved = false;
        self.screen_tex = None;
        self.shared = Some(shared);
        self.tx_out = Some(tx_out);
        self.disconnect_tx = Some(dc_tx);
        self.last_mouse = None;
        self.mods = [false; 4];
        self.buttons_down = [false; 3];
        self.chars_down.clear();
        self.scroll_acc = (0.0, 0.0);
        self.login_msg.clear();
        self.screen = Screen::Viewer;
    }

    pub(crate) fn tx(&self) -> Option<Sender<Packet>> {
        self.tx_out.clone()
    }

    /// Disconnect and return to the home screen.
    pub(crate) fn disconnect(&mut self) {
        if let Some(dc) = self.disconnect_tx.take() {
            let _ = dc.send(true);
        }
        self.tx_out = None;
        self.shared = None;
        self.screen_tex = None;
        self.thumbs.remove(&self.current_addr);
        self.screen = Screen::Home;
    }

    fn flash(&mut self, text: impl Into<String>) {
        self.flash = Some((text.into(), Instant::now()));
    }

    fn copy(&mut self, ctx: &egui::Context, text: &str, what: &str) {
        ctx.copy_text(text.to_string());
        self.flash(format!("{what} copied"));
    }
}

fn card<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(14.0)
        .inner_margin(18.0)
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            ui.label(RichText::new(title).size(17.0).strong());
            ui.add_space(6.0);
            add(ui)
        })
        .inner
}

fn dot(ui: &mut egui::Ui, color: Color32, text: impl Into<String>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("●").color(color));
        ui.label(text.into());
    });
}

impl App {
    fn home_ui(&mut self, ctx: &egui::Context) {
        self.lan.lock().unwrap().retain(|e| e.last_seen.elapsed().as_secs() < 8);
        let snap = remote_friend_host::snapshot();
        let host_error = self.host_error.lock().unwrap().clone();
        if self.logo.is_none() {
            let img = image::load_from_memory(ICON_PNG).expect("embedded icon").to_rgba8();
            let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], &img);
            self.logo = Some(ctx.load_texture("logo", ci, egui::TextureOptions::LINEAR));
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if let Some(logo) = &self.logo {
                    ui.image((logo.id(), egui::vec2(30.0, 30.0)));
                }
                ui.label(RichText::new("RemoteFriend").size(21.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙ Settings").clicked() {
                        let (server, web) = remote_friend_host::server_settings();
                        self.settings_server = server;
                        self.settings_web = web;
                        self.autostart = autostart::is_enabled();
                        self.settings_open = true;
                    }
                    if let Some((text, t)) = &self.flash {
                        if t.elapsed() < Duration::from_secs(3) {
                            ui.label(RichText::new(text).color(OK));
                        }
                    }
                });
            });
            ui.add_space(4.0);
        });

        egui::TopBottomPanel::bottom("log").show(ctx, |ui| {
            ui.add_space(4.0);
            let recent: Vec<_> = snap.notices.iter().rev().take(3).collect();
            if recent.is_empty() {
                ui.small("Keep RemoteFriend open to allow remote access to this computer.");
            }
            for n in recent {
                ui.small(format!("{}  {}", n.time, n.text));
            }
            ui.add_space(2.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(6.0);
            ui.columns(2, |cols| {
                card(&mut cols[0], "This computer", |ui| self.this_computer_ui(ui, &snap, host_error.as_deref()));
                card(&mut cols[1], "Control another computer", |ui| self.connect_ui(ui));
            });
        });

        if self.settings_open {
            self.settings_ui(ctx, &snap);
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn this_computer_ui(&mut self, ui: &mut egui::Ui, snap: &remote_friend_host::Snapshot, host_error: Option<&str>) {
        if let Some(err) = host_error {
            ui.colored_label(ERR, "Sharing this computer is not available:");
            ui.label(err);
            if err.contains("already running") || err.contains("33200") {
                ui.small("RemoteFriend seems to be running already (maybe in a terminal). Close the other one and restart this app.");
            }
            return;
        }
        if snap.id.is_empty() {
            ui.spinner();
            return;
        }
        ui.label(RichText::new("Others can control this computer with:").weak());
        ui.add_space(4.0);
        egui::Grid::new("ids").num_columns(3).spacing([12.0, 10.0]).show(ui, |ui| {
            ui.label("Your ID");
            ui.label(RichText::new(remote_friend_common::format_id(&snap.id)).size(28.0).strong().monospace());
            if ui.button("Copy").clicked() {
                self.copy(ui.ctx(), &snap.id, "ID");
            }
            ui.end_row();

            ui.label("Password");
            let shown = if snap.password_from_env {
                "(set by REMOTE_FRIEND_PASS)".to_string()
            } else if self.show_password {
                snap.password.clone()
            } else {
                "•••••-•••••".to_string()
            };
            ui.label(RichText::new(shown).size(22.0).monospace());
            ui.horizontal(|ui| {
                if !snap.password_from_env {
                    if ui.button(if self.show_password { "Hide" } else { "Show" }).clicked() {
                        self.show_password = !self.show_password;
                    }
                    if ui.button("Copy").clicked() {
                        self.copy(ui.ctx(), &snap.password, "Password");
                    }
                }
            });
            ui.end_row();
        });

        ui.add_space(10.0);
        // Online status
        match (&snap.server, snap.online, &snap.server_error) {
            (None, _, _) => dot(ui, WARN, "Local network only — set a server in Settings to connect from anywhere"),
            (Some(_), true, _) => dot(ui, OK, "Online — reachable from anywhere"),
            (Some(s), false, Some(e)) => dot(ui, ERR, format!("Cannot reach server {s}: {e}")),
            (Some(s), false, None) => dot(ui, WARN, format!("Connecting to {s}...")),
        }
        // Screen permission (Wayland)
        match snap.screen {
            ScreenState::Ready => {}
            ScreenState::WaitingPermission => {
                dot(ui, WARN, "Waiting for screen-sharing permission: turn ON “Allow Remote Interaction” and click Share");
            }
            ScreenState::Denied => {
                ui.horizontal(|ui| {
                    dot(ui, ERR, "Screen sharing is not allowed");
                    if ui.button("Ask again").clicked() && !remote_friend_host::retry_screen_permission() {
                        restart_app();
                    }
                });
            }
            ScreenState::NoInput => {
                ui.horizontal(|ui| {
                    dot(ui, ERR, "Remote control (mouse/keyboard) was not allowed");
                    if ui.button("Ask again").clicked() && !remote_friend_host::retry_screen_permission() {
                        restart_app();
                    }
                });
            }
        }
        if snap.sessions > 0 {
            dot(ui, ACCENT, format!("{} active connection(s) to this computer", snap.sessions));
        }

        if let Some(url) = &snap.web_url {
            ui.add_space(10.0);
            ui.label(RichText::new("From a phone or any browser, open:").weak());
            ui.horizontal(|ui| {
                ui.hyperlink_to(RichText::new(url).size(16.0), url);
                if ui.small_button("Copy").clicked() {
                    self.copy(ui.ctx(), url, "Address");
                }
            });
        }
        ui.add_space(10.0);
        ui.small("New devices must be approved here. Choose “Always allow” to let a device connect with just the password next time.");
        if snap.trusted_devices > 0 {
            ui.small(format!("Always-allowed devices: {}", snap.trusted_devices));
        }
        if snap.auto_accept {
            ui.colored_label(WARN, "Approval is off (REMOTE_FRIEND_AUTO_ACCEPT=1): anyone with the password can connect.");
        }
    }

    fn connect_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Computer ID or address").weak());
        let id_resp = ui.add(
            egui::TextEdit::singleline(&mut self.host_field)
                .hint_text("123 456 789   or   192.168.1.20")
                .font(egui::TextStyle::Heading)
                .desired_width(f32::INFINITY),
        );
        ui.label(RichText::new("Password").weak());
        let pw_resp = ui.add(egui::TextEdit::singleline(&mut self.pass_field).password(true).desired_width(f32::INFINITY));
        let enter = (id_resp.lost_focus() || pw_resp.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.add_space(4.0);
        let btn = egui::Button::new(RichText::new("Connect").size(17.0).strong().color(Color32::WHITE))
            .fill(ACCENT)
            .min_size(egui::vec2(ui.available_width(), 38.0));
        if ui.add(btn).clicked() || enter {
            self.connect();
        }
        if !self.login_msg.is_empty() {
            ui.colored_label(WARN, &self.login_msg);
        }

        ui.add_space(10.0);
        let my_name = remote_friend_host::snapshot().name;
        let lan: Vec<LanEntry> = self.lan.lock().unwrap().iter().filter(|e| e.name != my_name).cloned().collect();
        if !lan.is_empty() {
            ui.label(RichText::new("On this network").strong());
            for e in lan {
                ui.horizontal(|ui| {
                    ui.label(format!("🖥 {}", e.name));
                    ui.small(format!("{}", e.ip));
                    if ui.small_button("Connect").clicked() {
                        self.host_field = format!("{}:{}", e.ip, e.port);
                        self.connect();
                    }
                });
            }
            ui.add_space(8.0);
        }

        ui.horizontal(|ui| {
            ui.label(RichText::new("Recent").strong());
            if !self.recents.is_empty() && ui.small_button("Clear").clicked() {
                self.recents.retain(|r| r.fav);
                history::save_recents(&self.recents);
            }
        });
        if self.recents.is_empty() {
            ui.small("Computers you connect to appear here.");
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            let n = self.recents.len();
            for i in 0..n {
                let (addr, name, fav, thumb) = {
                    let r = &self.recents[i];
                    (r.addr.clone(), r.name.clone(), r.fav, r.thumb.clone())
                };
                let tex = thumb.and_then(|f| {
                    self.thumbs.entry(addr.clone()).or_insert_with(|| load_thumb_texture(ui.ctx(), &f)).clone()
                });
                let resp = ui.horizontal(|ui| {
                    match &tex {
                        Some(t) => {
                            ui.image((t.id(), egui::vec2(96.0, 54.0)));
                        }
                        None => {
                            ui.add_sized([96.0, 54.0], egui::Label::new("🖥"));
                        }
                    }
                    ui.vertical(|ui| {
                        ui.label(format!("{}{}", if fav { "★ " } else { "" }, name));
                        ui.small(remote_friend_common::format_id(&addr));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Connect").clicked() {
                            self.host_field = addr.clone();
                            return true;
                        }
                        if ui.small_button(if fav { "★" } else { "☆" }).clicked() {
                            self.recents[i].fav = !fav;
                            history::save_recents(&self.recents);
                        }
                        false
                    })
                    .inner
                });
                if resp.inner {
                    self.connect();
                    return;
                }
                ui.separator();
            }
        });
    }

    fn settings_ui(&mut self, ctx: &egui::Context, snap: &remote_friend_host::Snapshot) {
        let mut open = true;
        egui::Window::new("Settings")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.label(RichText::new("Server (to connect from anywhere)").strong());
                ui.small("Address of your RemoteFriend relay, e.g. 203.0.113.10:33202. Leave empty for local network only.");
                ui.add(egui::TextEdit::singleline(&mut self.settings_server).hint_text("server:33202").desired_width(f32::INFINITY));
                ui.small("Web address for phones (optional), e.g. https://remote.example.com");
                ui.add(egui::TextEdit::singleline(&mut self.settings_web).hint_text("https://...").desired_width(f32::INFINITY));
                if ui.button("Save and reconnect").clicked() {
                    remote_friend_host::set_server(&self.settings_server, &self.settings_web);
                    self.flash("Server settings saved");
                }
                ui.separator();

                ui.label(RichText::new("This computer").strong());
                let mut auto = self.autostart;
                if ui.checkbox(&mut auto, "Start RemoteFriend when I log in").changed() {
                    match autostart::set(auto) {
                        Ok(()) => self.autostart = auto,
                        Err(e) => self.flash(format!("Could not change startup setting: {e}")),
                    }
                }
                ui.horizontal(|ui| {
                    if !snap.password_from_env && ui.button("New password").clicked() {
                        if remote_friend_host::renew_password().is_some() {
                            self.show_password = true;
                            self.flash("New password created; the old one no longer works");
                        }
                    }
                    let label = format!("Forget always-allowed devices ({})", snap.trusted_devices);
                    if ui.add_enabled(snap.trusted_devices > 0, egui::Button::new(label)).clicked() {
                        let n = remote_friend_host::forget_devices();
                        self.flash(format!("{n} device(s) will need approval again"));
                    }
                });
                let dir = remote_friend_host::receive_dir();
                ui.horizontal(|ui| {
                    ui.label("Received files:");
                    ui.small(dir.display().to_string());
                    if ui.small_button("Open").clicked() {
                        let _ = std::fs::create_dir_all(&dir);
                        open_path(&dir);
                    }
                });
                ui.separator();
                ui.small(format!("RemoteFriend {}", env!("CARGO_PKG_VERSION")));
            });
        if !open {
            self.settings_open = false;
        }
    }

    /// Connection requests / server trust questions from the host engine.
    fn prompts_ui(&mut self, ctx: &egui::Context) {
        let seq = remote_friend_host::prompt_sequence();
        if seq != self.last_prompt_seq {
            self.last_prompt_seq = seq;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Critical));
        }
        let prompts = remote_friend_host::pending_prompts();
        let Some(p) = prompts.first() else { return };
        ctx.request_repaint_after(Duration::from_millis(250));
        egui::Modal::new(egui::Id::new(("prompt", p.id))).show(ctx, |ui| {
            ui.set_width(440.0);
            match &p.kind {
                PromptKind::Connection { peer, can_remember } => {
                    ui.label(RichText::new("Connection request").size(19.0).strong());
                    ui.add_space(4.0);
                    ui.label(format!("{peer} wants to view and control this computer."));
                    ui.small("The correct password was entered. Allow only if you expect this connection.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let allow = egui::Button::new(RichText::new("Allow").strong().color(Color32::WHITE)).fill(OK);
                        if ui.add(allow).clicked() {
                            remote_friend_host::answer(p.id, Answer::Connection(Decision::Once));
                        }
                        if *can_remember && ui.button("Always allow this device").clicked() {
                            remote_friend_host::answer(p.id, Answer::Connection(Decision::Always));
                        }
                        let deny = egui::Button::new(RichText::new("Deny").color(Color32::WHITE)).fill(ERR);
                        if ui.add(deny).clicked() {
                            remote_friend_host::answer(p.id, Answer::Connection(Decision::Deny));
                        }
                    });
                    ui.small(format!("Denied automatically in {} s", p.seconds_left));
                }
                PromptKind::TrustServer { addr, fingerprint } => {
                    ui.label(RichText::new("First connection to the server").size(19.0).strong());
                    ui.label(format!("Server: {addr}"));
                    ui.label("Certificate fingerprint:");
                    ui.add(egui::Label::new(RichText::new(fingerprint).monospace().size(12.0)).wrap());
                    ui.small("It must match the fingerprint printed at the end of the server setup. It is remembered after you trust it.");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new("Trust").strong().color(Color32::WHITE)).fill(OK)).clicked() {
                            remote_friend_host::answer(p.id, Answer::Trust(true));
                        }
                        if ui.button("Cancel").clicked() {
                            remote_friend_host::answer(p.id, Answer::Trust(false));
                        }
                    });
                }
            }
        });
    }
}

fn load_thumb_texture(ctx: &egui::Context, fname: &str) -> Option<egui::TextureHandle> {
    let data = std::fs::read(history::thumb_path(fname)).ok()?;
    let img = image::load_from_memory(&data).ok()?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let ci = egui::ColorImage::from_rgba_unmultiplied([w, h], &img.into_raw());
    Some(ctx.load_texture(format!("thumb_{fname}"), ci, egui::TextureOptions::LINEAR))
}

/// Open a folder in the system file manager.
fn open_path(path: &std::path::Path) {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

/// Start a fresh copy of the app and quit (used to ask for screen permission again).
fn restart_app() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    std::process::exit(0);
}

