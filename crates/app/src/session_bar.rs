//! A small bar on top of everything while someone is connected, pinned to the top of the
//! shared screen: how many, for how long, which screen, files on their way, and buttons to
//! take back control or end it. "Details" opens per-viewer controls like the main window's;
//! the bar can shrink to a small pill. It stays when the main window is minimized or covered,
//! so this computer's user always knows the screen is shared.
//!
//! Windows, macOS and X11: a second window of this app (egui viewport). Wayland doesn't let
//! apps place windows or keep them on top, so there the bar runs as a small helper process
//! on XWayland (`--session-bar`): the app sends it the state as JSON lines on stdin and runs
//! the commands it prints on stdout. Viewers' clicks can't use the bar either way
//! (App::raw_input_hook in the app, the same check for the helper's commands).

use egui::{Color32, RichText};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::ui_kit::{self as kit, Icon, ACCENT, ERR, MUTED, OK, TEXT, WARN};

const WIDTH: f32 = 700.0;
const ROW: f32 = 46.0;
const PILL: [f32; 2] = [250.0, 38.0];

/// What the bar shows.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct BarState {
    pub viewers: Vec<BarViewer>,
    /// "Take back control" is on.
    pub blocked: bool,
    /// Shared screen (1-based) and how many there are.
    pub screen: usize,
    pub screens: usize,
    /// The shared screen on the desktop: x, y, width, height (see MonitorInfo).
    pub monitor: Option<[f32; 4]>,
    /// Unix seconds.
    pub now: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct BarViewer {
    pub id: u64,
    pub label: String,
    pub access: String,
    pub since: u64,
    /// control, sound, files, clipboard
    pub perms: [bool; 4],
    pub direct: bool,
    /// A file on its way: (to this computer, name, done, total).
    pub transfer: Option<(bool, String, u64, u64)>,
}

/// Something the user clicked in the bar.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum BarCmd {
    Disconnect(u64),
    DisconnectAll,
    Block(bool),
    Open,
    Perms(u64, [bool; 4]),
    Screen(usize),
}

/// Pill, bar or bar with details (kept across frames; one bar per process).
static MODE: AtomicU8 = AtomicU8::new(MODE_BAR);
const MODE_PILL: u8 = 0;
const MODE_BAR: u8 = 1;
const MODE_DETAILS: u8 = 2;
/// The screen the bar was last placed on, and the size it was last given.
static PLACED: Mutex<Option<[f32; 4]>> = Mutex::new(None);
static SIZED: Mutex<Option<[f32; 2]>> = Mutex::new(None);
/// Height the details needed last frame (the window follows it).
static MEASURED: Mutex<f32> = Mutex::new(0.0);

/// The state from the host engine.
pub(crate) fn current() -> BarState {
    use remote_friend_host::sessions;
    let monitors = remote_friend_host::monitors();
    let cur = remote_friend_host::current_monitor();
    let viewers = sessions::list()
        .into_iter()
        .map(|s| BarViewer {
            id: s.id,
            label: s.label,
            access: s.access.word().to_string(),
            since: s.since,
            perms: [s.perms.control, s.perms.sound, s.perms.files, s.perms.clipboard],
            direct: s.direct,
            transfer: s.transfer.map(|t| (t.incoming, t.name, t.got, t.total)),
        })
        .collect();
    BarState {
        viewers,
        blocked: sessions::input_blocked(),
        screen: cur + 1,
        screens: monitors.len().max(1),
        monitor: monitors.iter().find(|m| m.index == cur).map(|m| [m.x as f32, m.y as f32, m.width as f32, m.height as f32]),
        now: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    }
}

/// Carry out a click from the bar.
pub(crate) fn run(cmd: BarCmd, ctx: &egui::Context) {
    use remote_friend_host::sessions;
    match cmd {
        BarCmd::Disconnect(id) => sessions::disconnect(id),
        BarCmd::DisconnectAll => {
            for s in sessions::list() {
                sessions::disconnect(s.id);
            }
        }
        BarCmd::Block(on) => sessions::set_input_blocked(on),
        BarCmd::Open => {
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
        }
        BarCmd::Perms(id, p) => {
            sessions::set_perms(id, sessions::Perms { control: p[0], sound: p[1], files: p[2], clipboard: p[3] });
        }
        BarCmd::Screen(i) => {
            remote_friend_host::select_monitor(i);
        }
    }
}

// ---- in the app ----

/// The Wayland helper: its stdin, its clicks, and whether it works at all.
struct Helper {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    cmds: Arc<Mutex<Vec<BarCmd>>>,
    sent: Instant,
}

static HELPER: Mutex<Option<Helper>> = Mutex::new(None);
static HELPER_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Call every frame of the main window: shows the bar while someone is connected.
pub(crate) fn show(ctx: &egui::Context) {
    let connected = remote_friend_host::sessions::count() > 0;
    if crate::privacy::wayland() && !HELPER_FAILED.load(Ordering::Relaxed) {
        helper_frame(ctx, connected);
        return;
    }
    if !connected {
        *PLACED.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *SIZED.lock().unwrap_or_else(|e| e.into_inner()) = None;
        return;
    }
    let builder = egui::ViewportBuilder::default()
        .with_title(remote_friend_host::privacy::SESSION_BAR_TITLE)
        .with_app_id("remotefriend")
        .with_inner_size([WIDTH, ROW])
        .with_decorations(false)
        .with_resizable(false)
        .with_always_on_top()
        .with_taskbar(false)
        .with_active(false)
        .with_window_type(egui::X11WindowType::Utility);
    ctx.show_viewport_deferred(egui::ViewportId::from_hash_of("rf-session-bar"), builder, |ctx, _| {
        ctx.request_repaint_after(Duration::from_millis(500));
        for cmd in bar(ctx, &current()) {
            run(cmd, ctx);
        }
    });
}

/// Wayland: keep the helper running while someone is connected and pass it the state.
fn helper_frame(ctx: &egui::Context, connected: bool) {
    let mut g = HELPER.lock().unwrap_or_else(|e| e.into_inner());
    if !connected {
        if let Some(mut h) = g.take() {
            let _ = h.child.kill();
            let _ = h.child.wait();
        }
        return;
    }
    if g.as_mut().is_some_and(|h| h.child.try_wait().ok().flatten().is_some()) {
        // It ended by itself (no XWayland?): the bar becomes a window of the app.
        tracing::warn!("session bar helper ended; showing the bar in the app");
        *g = None;
        HELPER_FAILED.store(true, Ordering::Relaxed);
        return;
    }
    if g.is_none() {
        match spawn_helper() {
            Some(h) => *g = Some(h),
            None => {
                HELPER_FAILED.store(true, Ordering::Relaxed);
                return;
            }
        }
    }
    let Some(h) = g.as_mut() else { return };
    if h.sent.elapsed() >= Duration::from_millis(300) {
        h.sent = Instant::now();
        let line = serde_json::to_string(&current()).unwrap_or_default();
        if writeln!(h.stdin, "{line}").and_then(|_| h.stdin.flush()).is_err() {
            HELPER_FAILED.store(true, Ordering::Relaxed);
        }
    }
    let cmds = std::mem::take(&mut *h.cmds.lock().unwrap_or_else(|e| e.into_inner()));
    drop(g);
    // A viewer's click lands on the helper like on any window: never act on it.
    if !cmds.is_empty() && !remote_friend_host::privacy::remote_input_recent().0 {
        for cmd in cmds {
            run(cmd, ctx);
        }
    }
    ctx.request_repaint_after(Duration::from_millis(300));
}

fn spawn_helper() -> Option<Helper> {
    use std::process::{Command, Stdio};
    std::env::var_os("DISPLAY")?;
    let exe = std::env::current_exe().ok()?;
    let path = exe.to_string_lossy().to_string();
    let exe = std::path::PathBuf::from(path.strip_suffix(" (deleted)").unwrap_or(&path));
    let mut child = Command::new(exe)
        .arg("--session-bar")
        // XWayland: there a window may choose its place and stay on top.
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let cmds = Arc::new(Mutex::new(Vec::new()));
    let sink = cmds.clone();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(cmd) = serde_json::from_str::<BarCmd>(&line) {
                sink.lock().unwrap_or_else(|e| e.into_inner()).push(cmd);
            }
        }
    });
    Some(Helper { child, stdin, cmds, sent: Instant::now() - Duration::from_secs(1) })
}

// ---- the helper process (Wayland) ----

/// `remotefriend --session-bar`: the bar alone, fed by the app on stdin; ends with it.
pub(crate) fn helper_main() -> anyhow::Result<()> {
    let state: Arc<Mutex<Option<BarState>>> = Arc::new(Mutex::new(None));
    let feed = state.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if let Ok(s) = serde_json::from_str::<BarState>(&line) {
                *feed.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
            }
        }
        // The app closed (or stopped the bar).
        std::process::exit(0);
    });
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(remote_friend_host::privacy::SESSION_BAR_TITLE)
            .with_app_id("remotefriend")
            .with_inner_size([WIDTH, ROW])
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top()
            .with_taskbar(false)
            .with_active(false)
            .with_window_type(egui::X11WindowType::Utility),
        ..Default::default()
    };
    struct HelperApp(Arc<Mutex<Option<BarState>>>);
    impl eframe::App for HelperApp {
        fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
            kit::SIDEBAR.to_normalized_gamma_f32()
        }
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            ctx.request_repaint_after(Duration::from_millis(250));
            let Some(state) = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
            let mut out = std::io::stdout().lock();
            for cmd in bar(ctx, &state) {
                let _ = writeln!(out, "{}", serde_json::to_string(&cmd).unwrap_or_default());
            }
            let _ = out.flush();
        }
    }
    eframe::run_native(
        "RemoteFriends session",
        opts,
        Box::new(move |cc| {
            kit::setup_style(&cc.egui_ctx);
            Ok(Box::new(HelperApp(state)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("session bar: {e}"))
}

// ---- drawing ----

/// "4:05" or "1:02:33".
fn clock(secs: u64) -> String {
    match secs {
        s if s < 3600 => format!("{}:{:02}", s / 60, s % 60),
        s => format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60),
    }
}

/// How tall the details are: what they needed last frame, or a guess before that.
fn details_height(state: &BarState) -> f32 {
    let measured = *MEASURED.lock().unwrap_or_else(|e| e.into_inner());
    if measured > ROW {
        return measured.ceil();
    }
    let viewers: f32 = state.viewers.iter().map(|v| if v.transfer.is_some() { 170.0 } else { 100.0 }).sum();
    ROW + 14.0 + viewers + if state.screens > 1 { 40.0 } else { 0.0 } + 50.0
}

/// Six dots to grab the bar by (it can also be dragged by any empty part).
fn grip(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(14.0, 24.0), egui::Sense::drag());
    let color = if resp.hovered() { TEXT } else { MUTED };
    for row in 0..3 {
        for col in 0..2 {
            let c = rect.center() + egui::vec2(-3.0 + col as f32 * 6.0, -6.0 + row as f32 * 6.0);
            ui.painter().circle_filled(c, 1.6, color);
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::Grab).on_hover_text("Drag to move")
}

/// Draw the bar for `state`; returns what was clicked.
fn bar(ctx: &egui::Context, state: &BarState) -> Vec<BarCmd> {
    let mut cmds = Vec::new();
    let mode = MODE.load(Ordering::Relaxed);
    let size = match mode {
        MODE_PILL => PILL,
        MODE_DETAILS => [WIDTH, details_height(state)],
        _ => [WIDTH, ROW],
    };
    if *SIZED.lock().unwrap_or_else(|e| e.into_inner()) != Some(size) {
        *SIZED.lock().unwrap_or_else(|e| e.into_inner()) = Some(size);
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size.into()));
        *PLACED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    // Pinned to the top middle of the shared screen; it moves along when another screen is
    // shared (dragging it elsewhere is kept until then).
    if let Some(m) = state.monitor {
        let mut placed = PLACED.lock().unwrap_or_else(|e| e.into_inner());
        if *placed != Some(m) {
            *placed = Some(m);
            // Monitor positions are pixels (points on macOS); the command takes points.
            let k = if cfg!(target_os = "macos") { 1.0 } else { 1.0 / ctx.pixels_per_point() };
            let pos = egui::pos2(m[0] * k + ((m[2] * k - size[0]) / 2.0).max(0.0), m[1] * k + 4.0);
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        }
    }
    let since = state.viewers.iter().map(|v| v.since).min().unwrap_or(state.now);
    let n = state.viewers.len();
    let moving = state.viewers.iter().find_map(|v| v.transfer.clone());
    let frame = egui::Frame::new()
        .fill(kit::SIDEBAR)
        .stroke(egui::Stroke::new(1.0_f32, kit::tint(if state.blocked { WARN } else { OK }, 0.5)))
        .corner_radius(kit::R_ROW)
        .inner_margin(egui::Margin::symmetric(8, 6));
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        // Text can't be selected here: a press anywhere that isn't a button drags the bar.
        ui.style_mut().interaction.selectable_labels = false;
        let drag = ui.interact(ui.max_rect(), egui::Id::new("session-bar-drag"), egui::Sense::drag());
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        let t = ctx.input(|i| i.time) as f32;
        let dot = |ui: &mut egui::Ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            let glow = 0.55 + 0.45 * (t * 2.5).sin().abs();
            ui.painter().circle_filled(rect.center(), 5.0, (if state.blocked { WARN } else { OK }).gamma_multiply(glow));
        };
        if mode == MODE_PILL {
            ui.horizontal_centered(|ui| {
                if grip(ui).drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                dot(ui);
                ui.label(RichText::new(format!("{n} · {}", clock(state.now.saturating_sub(since)))).color(TEXT).strong());
                if let Some((incoming, _, done, total)) = &moving {
                    let pct = if *total > 0 { done * 100 / total } else { 0 };
                    ui.label(RichText::new(format!("{} {pct}%", if *incoming { "⬇" } else { "⬆" })).color(ACCENT));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if kit::icon(ui, Icon::More, "Show the bar").clicked() {
                        MODE.store(MODE_BAR, Ordering::Relaxed);
                    }
                });
            });
            return;
        }
        ui.horizontal(|ui| {
            ui.set_height(ROW - 12.0);
            if grip(ui).drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            dot(ui);
            let who: Vec<String> = state.viewers.iter().map(|v| format!("{} ({})", v.label, v.access)).collect();
            let count = if n == 1 { "1 connected".to_string() } else { format!("{n} connected") };
            ui.label(RichText::new(count).color(TEXT).strong()).on_hover_text(who.join("\n"));
            ui.label(RichText::new(clock(state.now.saturating_sub(since))).color(MUTED).monospace());
            let shown = if state.screens > 1 { format!("Screen {} of {}", state.screen, state.screens) } else { "Screen shared".into() };
            ui.label(RichText::new(shown).color(MUTED));
            if let Some((incoming, name, done, total)) = &moving {
                let pct = if *total > 0 { done * 100 / total } else { 0 };
                ui.label(RichText::new(format!("{} {pct}%", if *incoming { "⬇" } else { "⬆" })).color(ACCENT).strong()).on_hover_text(
                    format!("{} {name}: {} of {}", if *incoming { "Receiving" } else { "Sending" }, kit::human_size(*done), kit::human_size(*total)),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if kit::icon(ui, Icon::Minus, "Make the bar small").clicked() {
                    MODE.store(MODE_PILL, Ordering::Relaxed);
                }
                let (icon, tip) = if mode == MODE_DETAILS { (Icon::Less, "Fewer details") } else { (Icon::More, "Details: viewers, permissions, files") };
                if kit::icon(ui, icon, tip).clicked() {
                    MODE.store(if mode == MODE_DETAILS { MODE_BAR } else { MODE_DETAILS }, Ordering::Relaxed);
                }
                let end = egui::Button::new(RichText::new("Disconnect").color(Color32::WHITE)).fill(kit::tint(ERR, 0.7));
                if ui.add(end).on_hover_text("End every session now").clicked() {
                    cmds.push(BarCmd::DisconnectAll);
                }
                let (label, tip) = if state.blocked {
                    ("Give back control", "Viewers may use mouse and keyboard again")
                } else {
                    ("Take back control", "Viewers can only watch until you give it back")
                };
                if ui.add(kit::secondary(label)).on_hover_text(tip).clicked() {
                    cmds.push(BarCmd::Block(!state.blocked));
                }
            });
        });
        if mode != MODE_DETAILS {
            return;
        }
        ui.add_space(6.0);
        for v in &state.viewers {
            egui::Frame::new().fill(kit::SURFACE_HI).corner_radius(kit::R_ROW).inner_margin(10).show(ui, |ui| {
                ui.set_width(ui.available_width());
                kit::split(
                    ui,
                    24.0,
                    |ui| {
                        let path = if v.direct { "direct" } else { "via server" };
                        kit::row_text(ui, &v.label, &format!("{} · {} · {path}", v.access, clock(state.now.saturating_sub(v.since))));
                    },
                    |ui| {
                        if ui.add(kit::danger("Disconnect")).clicked() {
                            cmds.push(BarCmd::Disconnect(v.id));
                        }
                    },
                );
                let mut p = v.perms;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (i, word) in ["Control", "Sound", "Files", "Clipboard"].iter().enumerate() {
                        let on = p[i];
                        let text = RichText::new(*word).size(13.0).color(if on { Color32::WHITE } else { kit::FAINT });
                        let fill = if on { kit::tint(ACCENT, 0.55) } else { kit::SURFACE };
                        if ui
                            .add(egui::Button::new(text).fill(fill).corner_radius(kit::R_BTN))
                            .on_hover_text(if on { "Allowed (click to turn off)" } else { "Off (click to allow)" })
                            .clicked()
                        {
                            p[i] = !on;
                        }
                    }
                });
                if p != v.perms {
                    cmds.push(BarCmd::Perms(v.id, p));
                }
                if let Some((incoming, name, done, total)) = &v.transfer {
                    ui.add_space(4.0);
                    kit::transfer(ui, if *incoming { "Receiving" } else { "Sending" }, name, *done, *total, false);
                }
            });
            ui.add_space(4.0);
        }
        if state.screens > 1 {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Shared screen:").color(MUTED));
                for i in 1..=state.screens {
                    let on = i == state.screen;
                    let text = RichText::new(format!("Screen {i}")).color(if on { Color32::WHITE } else { TEXT });
                    let b = egui::Button::new(text).fill(if on { kit::tint(ACCENT, 0.55) } else { kit::SURFACE });
                    if ui.add(b).clicked() && !on {
                        cmds.push(BarCmd::Screen(i - 1));
                    }
                }
            });
        }
        ui.horizontal(|ui| {
            if ui.add(kit::secondary("Open RemoteFriends")).clicked() {
                cmds.push(BarCmd::Open);
            }
        });
        // Grow or shrink the window to what the details need (margins and border included).
        let needed = ui.min_rect().height() + 14.0;
        let mut measured = MEASURED.lock().unwrap_or_else(|e| e.into_inner());
        if (needed - *measured).abs() > 2.0 {
            *measured = needed;
            ctx.request_repaint();
        }
    });
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_reads_like_a_stopwatch() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(245), "4:05");
        assert_eq!(clock(3753), "1:02:33");
    }

    #[test]
    fn state_and_clicks_cross_the_pipe() {
        let s = BarState {
            viewers: vec![BarViewer { id: 7, label: "phone".into(), transfer: Some((true, "a.mp4".into(), 5, 10)), ..Default::default() }],
            screen: 2,
            screens: 2,
            monitor: Some([1920.0, 0.0, 1920.0, 1080.0]),
            ..Default::default()
        };
        let back: BarState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.viewers[0].transfer.as_ref().unwrap().1, "a.mp4");
        let cmd: BarCmd = serde_json::from_str(&serde_json::to_string(&BarCmd::Perms(7, [true, false, true, true])).unwrap()).unwrap();
        assert!(matches!(cmd, BarCmd::Perms(7, [true, false, true, true])));
    }
}
