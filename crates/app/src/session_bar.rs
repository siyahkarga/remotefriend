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

/// The screens, re-read every few seconds (listing them is not free on every frame).
fn monitors() -> (Vec<remote_friend_host::MonitorInfo>, usize) {
    static CACHE: Mutex<Option<(Instant, Vec<remote_friend_host::MonitorInfo>, usize)>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if c.as_ref().is_none_or(|(t, _, _)| t.elapsed() > Duration::from_secs(2)) {
        *c = Some((Instant::now(), remote_friend_host::monitors(), remote_friend_host::current_monitor()));
    }
    let (_, list, cur) = c.as_ref().expect("filled above");
    (list.clone(), *cur)
}

/// The state from the host engine.
pub(crate) fn current() -> BarState {
    use remote_friend_host::sessions;
    let (monitors, cur) = monitors();
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

static HELPER_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static HELPER_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Call every frame of the main window: shows the bar while someone is connected.
pub(crate) fn show(ctx: &egui::Context) {
    let connected = remote_friend_host::sessions::count() > 0;
    if crate::privacy::wayland() && !HELPER_FAILED.load(Ordering::Relaxed) {
        if connected && !HELPER_RUNNING.swap(true, Ordering::SeqCst) {
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("rf-session-bar".into())
                .spawn(move || {
                    if !run_helper(&ctx) {
                        HELPER_FAILED.store(true, Ordering::Relaxed);
                        ctx.request_repaint();
                    }
                    HELPER_RUNNING.store(false, Ordering::SeqCst);
                })
                .ok();
        }
        return;
    }
    if !connected {
        *PLACED.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *SIZED.lock().unwrap_or_else(|e| e.into_inner()) = None;
        return;
    }
    ctx.show_viewport_deferred(egui::ViewportId::from_hash_of("rf-session-bar"), builder(), |ctx, _| {
        ctx.request_repaint_after(Duration::from_secs(1));
        for cmd in bar(ctx, &current()) {
            run(cmd, ctx);
        }
    });
}

fn builder() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title(remote_friend_host::privacy::SESSION_BAR_TITLE)
        .with_app_id("remotefriend")
        .with_inner_size([WIDTH, ROW])
        .with_decorations(false)
        // Resizable as far as the window manager is concerned, or it may refuse to let the
        // bar grow when the details open (it has no edges to drag anyway).
        .with_resizable(true)
        .with_always_on_top()
        .with_taskbar(false)
        .with_active(false)
        .with_window_type(egui::X11WindowType::Utility)
}

/// Wayland: run the helper while someone is connected, on this thread — the main window may
/// not be drawn at all while it is minimized, but the bar must keep working. False if the
/// helper can't run (the bar then becomes a window of the app).
fn run_helper(ctx: &egui::Context) -> bool {
    use std::process::{Command, Stdio};
    let Some(exe) = std::env::var_os("DISPLAY").and(std::env::current_exe().ok()) else { return false };
    let path = exe.to_string_lossy().to_string();
    let exe = std::path::PathBuf::from(path.strip_suffix(" (deleted)").unwrap_or(&path));
    let Ok(mut child) = Command::new(exe)
        .arg("--session-bar")
        // XWayland: there a window may choose its place and stay on top.
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else { return false };
    // Its clicks, as they come.
    let reader_ctx = ctx.clone();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(cmd) = serde_json::from_str::<BarCmd>(&line) else { continue };
            // A viewer's click lands on the helper like on any window: never act on it.
            if !remote_friend_host::privacy::remote_input_recent().0 {
                run(cmd, &reader_ctx);
                reader_ctx.request_repaint();
            }
        }
    });
    let started = Instant::now();
    loop {
        if remote_friend_host::sessions::count() == 0 {
            let _ = child.kill();
            let _ = child.wait();
            return true;
        }
        if child.try_wait().ok().flatten().is_some() {
            // It ended by itself: no XWayland? Early means it never worked.
            tracing::warn!("session bar helper ended");
            return started.elapsed() > Duration::from_secs(10);
        }
        let line = serde_json::to_string(&current()).unwrap_or_default();
        if writeln!(stdin, "{line}").and_then(|_| stdin.flush()).is_err() {
            let _ = child.kill();
            return false;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
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
            .with_resizable(true)
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
            ctx.request_repaint_after(Duration::from_millis(500));
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

/// How tall the details are. Worked out from what they hold, never measured from the drawn
/// window: measuring and resizing fed each other into an endless resize on Windows.
fn details_height(state: &BarState) -> f32 {
    let viewers: f32 = state.viewers.iter().map(|v| if v.transfer.is_some() { 168.0 } else { 96.0 }).sum();
    let screens = if state.screens > 1 { 40.0 } else { 0.0 };
    (ROW + 10.0 + viewers + screens + 50.0).min(520.0)
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
        // A steady dot: an animation would make the shared picture change all the time.
        let dot = |ui: &mut egui::Ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 5.0, if state.blocked { WARN } else { OK });
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
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| details(ui, state, &mut cmds));
    });
    cmds
}

/// Every viewer with its permissions, Disconnect and file progress; the shared screen.
fn details(ui: &mut egui::Ui, state: &BarState, cmds: &mut Vec<BarCmd>) {
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
