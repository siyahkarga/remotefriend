//! RemoteFriend desktop app: share this computer and control other computers.
//!
//! One window, like AnyDesk/TeamViewer: the left side shows this computer's ID and
//! password (others connect to it), the right side connects to another computer.
//! The host engine runs in the background while the app is open.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod audio;
mod autostart;
mod history;
mod keys_ui;
mod net;
mod privacy;
mod ui_home;
mod update;
mod viewer;

use anyhow::Result;
use egui::{Color32, RichText};
use history::RecentEntry;
use net::{LanEntry, Shared};
use remote_friend_common::Packet;
use remote_friend_host::{Answer, Decision, PromptKind};
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
    settings_key: String,
    settings_use_server: bool,
    /// Show the access key in Settings instead of dots.
    settings_key_visible: bool,
    /// A newer version on blobidea.com (filled by update::start).
    update: Arc<Mutex<Option<update::Update>>>,
    update_dismissed: Option<String>,
    /// Settings → Server → Manage access keys (server owner only).
    keys_ui: keys_ui::KeysUi,
    lan_enabled: bool,
    lan_changed: bool,
    autostart: bool,
    last_prompt_seq: u64,
    flash: Option<(String, Instant)>,
    logo: Option<egui::TextureHandle>,
    /// Very narrow window: which panel is shown (0 = this computer, 1 = connect).
    narrow_tab: u8,
    /// Play the remote computer's sound in the viewer.
    sound_on: bool,
    /// Closing was requested while remote access is on: ask first.
    close_confirm: bool,
    /// The user confirmed closing.
    allow_close: bool,
    /// Sessions in the previous frame (to minimize the window when the first one starts).
    last_sessions: usize,
    /// The window is currently excluded from screen capture.
    hidden_from_capture: bool,
    /// Viewer: the remote computer asks to switch sides (shown as a question).
    switch_question: bool,
    /// Viewer: we offered to switch sides and wait for the answer.
    switch_pending: bool,
    /// Preview of the shared screen (refreshed about once a second).
    preview_tex: Option<egui::TextureHandle>,
    preview_at: Option<Instant>,
    /// Viewer: the Files window is open.
    files_open: bool,
    /// Settings: private apps (comma separated) hidden in the safe view.
    settings_private_apps: String,
    /// Password typed for the current connection (saved once the computer accepts it).
    pending_password: String,
    /// Settings tab, and the small History / Trusted devices windows.
    settings_tab: u8,
    history_open: bool,
    /// Screen list, re-read every few seconds.
    monitors: (Vec<remote_friend_host::MonitorInfo>, Option<Instant>),
}

fn load_icon() -> egui::IconData {
    let img = image::load_from_memory(ICON_PNG).expect("embedded icon").to_rgba8();
    let (w, h) = img.dimensions();
    egui::IconData { rgba: img.into_raw(), width: w, height: h }
}

fn setup_style(ctx: &egui::Context) {
    use egui::{FontId, TextStyle};
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut(|s| {
        s.text_styles = [
            (TextStyle::Heading, FontId::proportional(20.0)),
            (TextStyle::Body, FontId::proportional(15.0)),
            (TextStyle::Button, FontId::proportional(14.5)),
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Monospace, FontId::monospace(14.5)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 7.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        let v = &mut s.visuals;
        v.panel_fill = Color32::from_rgb(19, 21, 26);
        v.window_fill = Color32::from_rgb(28, 31, 38);
        v.faint_bg_color = Color32::from_rgb(28, 31, 38);
        v.extreme_bg_color = Color32::from_rgb(14, 16, 20);
        v.window_corner_radius = 14.into();
        v.window_stroke = egui::Stroke::new(1.0_f32, Color32::from_rgb(48, 52, 62));
        v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, Color32::from_rgb(40, 44, 52));
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(40, 44, 54);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(52, 57, 70);
        v.selection.bg_fill = ACCENT;
        v.hyperlink_color = ACCENT;
        for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
            w.corner_radius = 8.into();
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

    let server_settings = remote_friend_host::server_settings();
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
        settings_server: server_settings.server,
        settings_web: server_settings.web_url,
        settings_key: server_settings.key,
        settings_use_server: server_settings.use_server,
        settings_key_visible: false,
        update: Arc::new(Mutex::new(None)),
        update_dismissed: None,
        keys_ui: keys_ui::KeysUi::default(),
        lan_enabled: remote_friend_host::lan_enabled(),
        lan_changed: false,
        autostart: autostart::is_enabled(),
        last_prompt_seq: 0,
        flash: None,
        logo: None,
        narrow_tab: 0,
        sound_on: true,
        close_confirm: false,
        allow_close: false,
        last_sessions: 0,
        hidden_from_capture: false,
        switch_question: false,
        switch_pending: false,
        preview_tex: None,
        preview_at: None,
        monitors: (Vec::new(), None),
        files_open: false,
        settings_private_apps: String::new(),
        pending_password: String::new(),
        settings_tab: 0,
        history_open: false,
    };

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RemoteFriend")
            .with_app_id("remotefriend")
            .with_inner_size([1060.0, 680.0])
            .with_min_inner_size([480.0, 480.0])
            .with_icon(load_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "RemoteFriend",
        opts,
        Box::new(|cc| {
            setup_style(&cc.egui_ctx);
            update::start(app.update.clone(), cc.egui_ctx.clone());
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("window: {e}"))?;
    Ok(())
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Switching sides: the computer we were connected to accepted; control it now.
        if let Some(t) = remote_friend_host::take_switch() {
            if matches!(self.screen, Screen::Viewer) {
                self.disconnect();
            }
            self.flash(format!("Switched sides: you now control {}", t.label));
            self.connect_to(t.id, String::new(), Some(t.token));
        }
        self.privacy(ctx, frame);
        self.close_guard(ctx);
        // Incoming connection requests must be answerable from any screen.
        self.prompts_ui(ctx);
        match self.screen {
            Screen::Home => self.home_ui(ctx),
            Screen::Viewer => self.viewer_ui(ctx),
        }
    }
}

impl App {
    /// While someone is connected: the window is minimized once and kept out of the shared
    /// picture (Windows, macOS), so viewers can't read the password or change settings.
    fn privacy(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let n = remote_friend_host::sessions::count();
        // Full view: viewers see everything, RemoteFriend included.
        let safe = !remote_friend_host::privacy::full_view();
        if safe && n > 0 && self.last_sessions == 0 && matches!(self.screen, Screen::Home) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        let hide = safe && n > 0;
        if hide != self.hidden_from_capture {
            privacy::exclude_from_capture(frame, hide);
            self.hidden_from_capture = hide;
        }
        self.last_sessions = n;
        if n > 0 {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    /// Closing ends remote access: ask first (also when a viewer tries to close it remotely).
    fn close_guard(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            let snap = remote_friend_host::snapshot();
            if snap.sessions > 0 || snap.online || matches!(self.screen, Screen::Viewer) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.close_confirm = true;
            }
        }
        if !self.close_confirm {
            return;
        }
        let sessions = remote_friend_host::sessions::count();
        egui::Modal::new(egui::Id::new("close_confirm")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new("Close RemoteFriend?").size(19.0).strong());
            ui.add_space(4.0);
            if sessions > 0 {
                ui.label(format!(
                    "{sessions} device(s) are connected to this computer right now. Closing ends their session."
                ));
            }
            if matches!(self.screen, Screen::Viewer) {
                ui.label("Your connection to the remote computer ends too.");
            }
            ui.label("While RemoteFriend is closed, this computer cannot be reached remotely.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Minimize instead").clicked() {
                    self.close_confirm = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                let close = egui::Button::new(RichText::new("Close RemoteFriend").color(Color32::WHITE)).fill(ERR);
                if ui.add(close).clicked() {
                    self.close_confirm = false;
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Cancel").clicked() {
                    self.close_confirm = false;
                }
            });
        });
    }

    /// Connect button: start the network thread and switch to the viewer.
    /// 9 digits = computer ID (via the relay server), otherwise a LAN address.
    fn connect(&mut self) {
        let host = self.host_field.trim().to_string();
        let device = history::device_for(&self.recents, &host);
        // An empty field uses the saved password for that computer (if any).
        let mut password = self.pass_field.clone();
        if password.trim().is_empty() && device.is_none() {
            password = history::password_for(&self.recents, &host).unwrap_or_default();
        }
        self.pending_password = password.clone();
        self.connect_to(host, password, device);
    }

    /// Open the viewer for `host` with a password, or as a trusted device with `device`.
    fn connect_to(&mut self, host: String, password: String, device: Option<String>) {
        if host.is_empty() {
            self.login_msg = "Enter a computer ID or address.".into();
            return;
        }
        if password.trim().is_empty() && device.is_none() {
            self.login_msg = "Enter the password shown on that computer.".into();
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
        let shared = Arc::new(Mutex::new(Shared::new(self.sound_on, device)));
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

impl App {
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
                PromptKind::SwitchSides { label } => {
                    ui.label(RichText::new("Switch sides?").size(19.0).strong());
                    ui.add_space(4.0);
                    ui.label(format!("{label} offers to switch: you would control their computer, and they stop controlling this one."));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let yes = egui::Button::new(RichText::new("Switch").strong().color(Color32::WHITE)).fill(OK);
                        if ui.add(yes).clicked() {
                            remote_friend_host::answer(p.id, Answer::Switch(true));
                        }
                        if ui.button("No").clicked() {
                            remote_friend_host::answer(p.id, Answer::Switch(false));
                        }
                    });
                    ui.small(format!("Declined automatically in {} s", p.seconds_left));
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
pub(crate) fn open_path(path: &std::path::Path) {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

/// Opens a web or mailto: link with the system's default program.
pub(crate) fn open_url(url: &str) {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
}

/// Start a fresh copy of the app and quit (used to ask for screen permission again).
fn restart_app() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    std::process::exit(0);
}

