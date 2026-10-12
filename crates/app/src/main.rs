//! RemoteFriend desktop app: share this computer and control other computers.
//!
//! One window with a menu on the left: Home shows this computer's ID and password (others
//! connect to it) next to "Connect to a computer"; Devices, History, Access keys and Settings
//! are pages of the same window, never windows on top of each other. The host engine runs in
//! the background while the app is open.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod audio;
mod autostart;
mod history;
mod keys_ui;
mod net;
mod notices;
mod privacy;
mod ui_dialogs;
mod ui_home;
mod ui_kit;
mod ui_pages;
mod update;
mod viewer;
mod welcome;

use anyhow::Result;
use history::RecentEntry;
use net::{LanEntry, Shared};
use remote_friend_common::Packet;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{channel, Sender};

const ICON_PNG: &[u8] = include_bytes!("../../../assets/icon-256.png");

enum Screen {
    Home,
    Viewer,
}

/// Pages of the main window (the menu on the left).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Page {
    Home,
    Devices,
    History,
    Keys,
    Settings,
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
    /// The page shown in the main window.
    page: Page,
    /// Devices page: "make it my device" waits for confirmation (handle, name).
    confirm_mine: Option<(String, String)>,
    settings_server: String,
    settings_web: String,
    settings_key: String,
    settings_use_server: bool,
    /// Show the access key in Settings instead of dots.
    settings_key_visible: bool,
    /// A newer version on blobidea.com (filled by update::start).
    update: Arc<Mutex<Option<update::Update>>>,
    update_dismissed: Option<String>,
    /// Downloading / installing an update.
    update_progress: Arc<Mutex<update::Progress>>,
    /// Automatic install: when the 30-second notice started.
    update_countdown: Option<Instant>,
    /// The welcome screen (sign in with email for an access key), while it is shown.
    welcome: Option<welcome::Welcome>,
    /// Server notices for its owner (new free keys, warnings).
    notices: Arc<Mutex<notices::Notices>>,
    /// "While you were away" was shown in this run.
    notices_prompted: bool,
    /// A notice's action waits for its second click.
    notice_confirm: Option<u64>,
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
    /// Settings tab.
    settings_tab: u8,
    /// Screen list, re-read every few seconds.
    monitors: (Vec<remote_friend_host::MonitorInfo>, Option<Instant>),
}

fn load_icon() -> egui::IconData {
    let img = image::load_from_memory(ICON_PNG).expect("embedded icon").to_rgba8();
    let (w, h) = img.dimensions();
    egui::IconData { rgba: img.into_raw(), width: w, height: h }
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
    // A new install without a key starts with the welcome screen.
    let first_run = !remote_friend_common::identity::load_host_settings().welcome_done
        && server_settings.use_server
        && server_settings.key.trim().is_empty();
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
        page: Page::Home,
        confirm_mine: None,
        settings_server: server_settings.server,
        settings_web: server_settings.web_url,
        settings_key: server_settings.key,
        settings_use_server: server_settings.use_server,
        settings_key_visible: false,
        update: Arc::new(Mutex::new(None)),
        update_dismissed: None,
        update_progress: Arc::new(Mutex::new(update::Progress::Idle)),
        update_countdown: None,
        welcome: first_run.then(welcome::Welcome::new),
        notices: Default::default(),
        notices_prompted: false,
        notice_confirm: None,
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
    };

    #[cfg(debug_assertions)]
    let app = {
        let mut app = app;
        debug_page(&mut app);
        app
    };
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RemoteFriend")
            .with_app_id("remotefriend")
            .with_inner_size([1120.0, 720.0])
            .with_min_inner_size([520.0, 500.0])
            .with_icon(load_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "RemoteFriend",
        opts,
        Box::new(|cc| {
            ui_kit::setup_style(&cc.egui_ctx);
            update::start(app.update.clone(), cc.egui_ctx.clone());
            notices::start(app.notices.clone(), cc.egui_ctx.clone());
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("window: {e}"))?;
    Ok(())
}

impl eframe::App for App {
    /// Opaque: the default clear colour is see-through, which let the desktop shine through
    /// half-transparent strips.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        ui_kit::BG.to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        #[cfg(debug_assertions)]
        debug_screenshot(ctx);
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
            Screen::Home if self.welcome.is_some() => self.welcome_ui(ctx),
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

    /// The welcome screen; a key it got is saved and the computer reconnects with it.
    fn welcome_ui(&mut self, ctx: &egui::Context) {
        self.load_logo(ctx);
        let Some(welcome) = self.welcome.as_mut() else { return };
        match welcome.ui(ctx, self.logo.as_ref()) {
            welcome::Outcome::Stay => {}
            welcome::Outcome::Key(key) => {
                let s = remote_friend_host::server_settings();
                remote_friend_host::set_server(&s.server, &s.web_url, &key, true);
                self.settings_key = key;
                self.settings_use_server = true;
                mark_welcome_done();
            }
            welcome::Outcome::Close => {
                self.welcome = None;
                mark_welcome_done();
            }
        }
    }

    pub(crate) fn load_logo(&mut self, ctx: &egui::Context) {
        if self.logo.is_none() {
            let img = image::load_from_memory(ICON_PNG).expect("embedded icon").to_rgba8();
            let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], &img);
            self.logo = Some(ctx.load_texture("logo", ci, egui::TextureOptions::LINEAR));
        }
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

/// Development aid: RF_SCREENSHOT=out.png saves the window after RF_SCREENSHOT_FRAMES frames
/// (default 40) and quits. Not in release builds.
#[cfg(debug_assertions)]
fn debug_screenshot(ctx: &egui::Context) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static FRAMES: AtomicU32 = AtomicU32::new(0);
    let Ok(path) = std::env::var("RF_SCREENSHOT") else { return };
    let n = FRAMES.fetch_add(1, Ordering::Relaxed);
    let wait: u32 = std::env::var("RF_SCREENSHOT_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    if n == wait {
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
    }
    ctx.request_repaint();
    let shot = ctx.input(|i| {
        i.raw.events.iter().find_map(|e| match e {
            egui::Event::Screenshot { image, .. } => Some(image.clone()),
            _ => None,
        })
    });
    if let Some(img) = shot {
        let [w, h] = img.size;
        let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
        let _ = image::save_buffer(&path, &rgba, w as u32, h as u32, image::ColorType::Rgba8);
        std::process::exit(0);
    }
}

/// Development aid: RF_PAGE=devices|history|keys|settings[:tab] opens that page first.
#[cfg(debug_assertions)]
fn debug_page(app: &mut App) {
    let page = std::env::var("RF_PAGE").unwrap_or_default();
    let (name, tab) = page.split_once(':').unwrap_or((page.as_str(), "0"));
    app.page = match name {
        "devices" => Page::Devices,
        "history" => Page::History,
        "keys" => Page::Keys,
        "settings" => Page::Settings,
        _ => Page::Home,
    };
    app.settings_tab = tab.parse().unwrap_or(0);
    if let Ok(target) = std::env::var("RF_AUTOCONNECT") {
        app.host_field = target;
        app.pass_field = std::env::var("RF_AUTOPASS").unwrap_or_default();
        app.connect();
    }
    if let Ok(step) = std::env::var("RF_DEMO_WELCOME") {
        app.welcome = Some(welcome::Welcome::demo(&step));
    }
    if std::env::var("RF_DEMO_UPDATE").is_ok() {
        *app.update.lock().unwrap() = Some(update::Update {
            version: "0.14.1".into(),
            security: false,
            install: Some(update::Installer { file: "demo".into(), size: 1, sha256: String::new(), needs_password: true }),
        });
    }
    if std::env::var("RF_DEMO_NOTICES").is_ok() {
        let event = |id: u64, kind: &str, text: &str, action: &str| serde_json::json!({"id": id, "time": "2026-10-12 21:40", "kind": kind, "text": text, "seen": false, "action": action, "key": "k1"});
        *app.notices.lock().unwrap() = notices::Notices {
            unseen: 2,
            events: vec![
                event(2, "warning", "One internet address connected to 10 different computers today (it is stopped at 40). Who it is cannot be known: connections are encrypted and need no account. The computers belong to: ali@example.com (2), Bayram, your own key (1). Ask them whether they expected it; if not, block the address.", "block"),
                event(1, "key", "New free key for ali@example.com (2 computers, until 2027-10-12).", "revoke"),
            ],
            mail: true,
            signup: true,
            loaded: true,
            ..Default::default()
        };
    }
    if std::env::var("RF_DEMO_PROMPT").is_ok() {
        remote_friend_host::demo_prompt(std::env::var("RF_DEMO_PROMPT").unwrap_or_default() == "trusted");
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

fn mark_welcome_done() {
    let mut s = remote_friend_common::identity::load_host_settings();
    if !s.welcome_done {
        s.welcome_done = true;
        remote_friend_common::identity::save_host_settings(&s);
    }
}

/// Start a fresh copy of the app and quit (used to ask for screen permission again).
fn restart_app() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    std::process::exit(0);
}

