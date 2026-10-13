//! What viewers see: "safe" (default) or "full".
//!
//! Safe: RemoteFriends itself and the windows of private apps — password managers and
//! anything the user adds — are blacked out. In both views a viewer's mouse and keyboard
//! can't use RemoteFriends (the app drops their clicks and keys, see [`remote_input_recent`])
//! unless it is one of the owner's own devices. Only the
//! visible part of a private window is covered, so windows in front of it stay visible.
//! Window positions are read for every captured frame (winlist.rs) and a window is covered
//! both where it is now and where it was a frame ago, so a moving window never shows through.
//! Windows, macOS and X11 tell apps where windows are; Wayland doesn't, so there only
//! RemoteFriends can keep itself out of view (the app shows a neutral screen instead).
//!
//! Full: everything is shown, as if sitting at the computer (RemoteFriends included).
//!
//! In both views a window whose app keeps it out of screen captures (Signal's "Screen
//! security") is shown as a striped box, and the computer's user and the viewers are told
//! once which app does that — otherwise it just seems to vanish.

use crate::winlist::Win;
use remote_friend_common::InputEvent;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Title of the small bar the app shows while someone is connected: it is not blacked out
/// (it holds nothing secret and tells viewers they are watched).
pub const SESSION_BAR_TITLE: &str = "RemoteFriends session";

/// Private apps hidden in safe mode unless the user changes the list.
pub const DEFAULT_PRIVATE_APPS: &[&str] = &[
    "keepass", "bitwarden", "1password", "lastpass", "dashlane", "keeper", "enpass", "proton pass", "authy",
];

fn full_flag() -> &'static AtomicBool {
    static F: OnceLock<AtomicBool> = OnceLock::new();
    F.get_or_init(|| AtomicBool::new(remote_friend_common::identity::load_host_settings().full_view))
}

/// Viewers see everything (true) or the safe view (false).
pub fn full_view() -> bool {
    full_flag().load(Ordering::Relaxed)
}

pub fn set_full_view(full: bool) {
    let mut s = remote_friend_common::identity::load_host_settings();
    s.full_view = full;
    remote_friend_common::identity::save_host_settings(&s);
    full_flag().store(full, Ordering::Relaxed);
    crate::status::notice(if full {
        "Viewers now see everything (full control)."
    } else {
        "Safe view: RemoteFriends and private apps are hidden from viewers."
    });
}

/// Name parts of apps hidden in safe mode (lowercase).
pub fn private_apps() -> Vec<String> {
    let s = remote_friend_common::identity::load_host_settings();
    if s.private_apps.is_empty() && !s.private_apps_set {
        DEFAULT_PRIVATE_APPS.iter().map(|s| s.to_string()).collect()
    } else {
        s.private_apps
    }
}

pub fn set_private_apps(list: Vec<String>) {
    let mut s = remote_friend_common::identity::load_host_settings();
    let mut clean: Vec<String> = Vec::new();
    for a in list.into_iter().map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()) {
        if !clean.contains(&a) {
            clean.push(a);
        }
    }
    s.private_apps = clean;
    s.private_apps_set = true;
    remote_friend_common::identity::save_host_settings(&s);
    *patterns_cache().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn patterns_cache() -> &'static Mutex<Option<Vec<String>>> {
    static P: Mutex<Option<Vec<String>>> = Mutex::new(None);
    &P
}

/// The private-app list without reading the settings file for every frame.
fn patterns() -> Vec<String> {
    patterns_cache().lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(private_apps).clone()
}

fn listed(name: &str, patterns: &[String]) -> bool {
    let name = name.to_lowercase();
    patterns.iter().any(|p| name.contains(p.as_str()))
}

/// An app with a window on screen now (Settings → Privacy).
#[derive(Clone, Debug)]
pub struct OpenApp {
    pub name: String,
    /// Blacked out in the safe view.
    pub hidden: bool,
}

/// Apps with a window on screen, by name (empty where the system doesn't tell, i.e. Wayland).
pub fn open_apps() -> Vec<OpenApp> {
    let patterns = patterns();
    let mut apps: Vec<OpenApp> = Vec::new();
    for w in crate::winlist::list() {
        let name = w.app.trim();
        if w.own || !w.opaque || name.is_empty() || name.to_lowercase().contains("remotefriend") {
            continue;
        }
        if !apps.iter().any(|a| a.name.eq_ignore_ascii_case(name)) {
            apps.push(OpenApp { name: name.to_string(), hidden: listed(name, &patterns) });
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// Hide an app in the safe view (adds its name to the list) or show it again (removes every
/// list entry that matches its name).
pub fn set_app_hidden(name: &str, hidden: bool) {
    let lower = name.trim().to_lowercase();
    let mut list = private_apps();
    if hidden {
        if !listed(&lower, &list) {
            list.push(lower);
        }
    } else {
        list.retain(|p| !lower.contains(p.as_str()));
    }
    set_private_apps(list);
}

// ---- remote input never operates RemoteFriends itself ----

/// Clicks and keys arriving this soon after a viewer's are taken to be the viewer's.
const REMOTE_ECHO: Duration = Duration::from_millis(700);

static REMOTE_POINTER: Mutex<Option<Instant>> = Mutex::new(None);
static REMOTE_KEYS: Mutex<Option<Instant>> = Mutex::new(None);

/// A viewer's input is about to reach the desktop (`mine`: one of the owner's own devices,
/// which may also use RemoteFriends).
pub(crate) fn remote_input(ev: &InputEvent, mine: bool) {
    let slot = match ev {
        _ if mine => return,
        InputEvent::MouseDown { .. } | InputEvent::MouseUp { .. } | InputEvent::Scroll { .. } => &REMOTE_POINTER,
        InputEvent::Key { .. } | InputEvent::Text(_) => &REMOTE_KEYS,
        InputEvent::MouseMove { .. } => return,
    };
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
}

/// Did a viewer click (`.0`) or type (`.1`) just now? The app then ignores clicks or keys it
/// receives, so viewers can't use RemoteFriends — in either view, and even blindly where its
/// window is hidden from them. Only the owner's own devices may.
pub fn remote_input_recent() -> (bool, bool) {
    let recent = |slot: &Mutex<Option<Instant>>| slot.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| t.elapsed() < REMOTE_ECHO);
    (recent(&REMOTE_POINTER), recent(&REMOTE_KEYS))
}

// ---- the mask ----

/// Browsers whose window is private only because of the open tab's title keep their tab strip
/// and address bar visible, so a viewer can switch to another tab. Height at 100 % scale.
const BROWSER_BARS: f32 = 88.0;
const BROWSERS: &[&str] =
    &["chrome", "chromium", "firefox", "msedge", "edge", "brave", "opera", "vivaldi", "safari", "librewolf", "waterfox", "zen", "arc"];

fn is_browser(app: &str) -> bool {
    BROWSERS.iter().any(|b| app.contains(b))
}

/// A window for the mask, in screen coordinates (window units).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shape {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    private: bool,
    /// Its app keeps it out of the capture: shown as stripes.
    excluded: bool,
    opaque: bool,
    /// Left visible at the top of a private window (a browser's tab strip and address bar).
    keep_top: i32,
}

/// Which windows are private (`full`: none, everything is shown). `bars` is the size of the
/// browser bars per BROWSER_BARS unit.
fn shapes(wins: &[Win], patterns: &[String], bars: f32, full: bool) -> Vec<Shape> {
    wins.iter()
        .map(|w| {
            let app = w.app.to_lowercase();
            let title = w.title.to_lowercase();
            // The session bar, the desktop and the taskbar are never hidden.
            let keep = full || w.title == SESSION_BAR_TITLE || w.shell;
            let whole_window = !keep && (w.own || app.contains("remotefriend") || title == "remotefriends" || listed(&app, patterns));
            let by_title = !keep && listed(&title, patterns);
            // Only the page of a browser tab is private: its bars stay visible.
            let keep_top = if !whole_window && by_title && is_browser(&app) {
                ((BROWSER_BARS * bars).round() as i32).min(w.h / 3)
            } else {
                0
            };
            let excluded = w.excluded && !w.own;
            Shape { x: w.x, y: w.y, w: w.w, h: w.h, private: whole_window || by_title, excluded, opaque: w.opaque, keep_top }
        })
        .collect()
}

/// Apps whose windows hide from screen captures, already mentioned in this run.
static TOLD: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn notes_tx() -> &'static tokio::sync::watch::Sender<Option<String>> {
    static TX: OnceLock<tokio::sync::watch::Sender<Option<String>>> = OnceLock::new();
    TX.get_or_init(|| tokio::sync::watch::channel(None).0)
}

/// Short explanations for every viewer (each session forwards them as {t:"note"}).
pub(crate) fn notes() -> tokio::sync::watch::Receiver<Option<String>> {
    notes_tx().subscribe()
}

/// Say once per app when it keeps its window out of screen captures.
fn tell_excluded(wins: &[Win]) {
    for w in wins.iter().filter(|w| w.excluded && !w.own) {
        let name = if w.app.trim().is_empty() { w.title.trim().to_string() } else { w.app.trim().to_string() };
        {
            let mut told = TOLD.lock().unwrap_or_else(|e| e.into_inner());
            if told.contains(&name) {
                continue;
            }
            told.push(name.clone());
        }
        crate::status::notice(format!(
            "{name} hides its window from screen sharing (its own setting, e.g. Signal: Settings, Privacy, Screen security). Viewers see a striped box there."
        ));
        notes_tx().send_replace(Some(format!("{name} hides its window from screen sharing; you see a striped box where it is.")));
    }
}

/// The windows of the previous frame, to cover a window that moved where it was before too.
static LAST: Mutex<Option<(Instant, Vec<Shape>)>> = Mutex::new(None);

/// Black out the visible parts of private windows (safe view) and stripe the windows their
/// apps keep out of the capture, in a captured monitor image (RGBA, `width`×`height`, monitor
/// origin at `mon_x`/`mon_y`, `scale` pixels per window unit, `bars` = display scale in
/// window units for the browser bars).
pub(crate) fn mask(rgba: &mut [u8], width: u32, height: u32, mon_x: i32, mon_y: i32, scale: f32, bars: f32) {
    let full = full_view();
    let wins = crate::winlist::list();
    tell_excluded(&wins);
    let patterns = if full { Vec::new() } else { patterns() };
    let now = shapes(&wins, &patterns, bars, full);
    let before = {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let before = last.take().filter(|(t, _)| t.elapsed() < Duration::from_secs(1)).map(|(_, s)| s);
        *last = Some((Instant::now(), now.clone()));
        before
    };
    // The picture was taken between the two lists: cover what is private in either.
    if let Some(before) = before.filter(|b| *b != now) {
        paint(rgba, width, height, &rects(&before, mon_x, mon_y, scale));
    }
    paint(rgba, width, height, &rects(&now, mon_x, mon_y, scale));
}

/// Windows in a monitor image's pixels (monitor origin `mon_x`/`mon_y`, `scale` pixels per
/// window unit).
fn rects(list: &[Shape], mon_x: i32, mon_y: i32, scale: f32) -> Vec<Rect> {
    let to_px = |v: i32, origin: i32| ((v - origin) as f32 * scale).round() as i32;
    list.iter()
        .map(|s| Rect {
            x0: to_px(s.x, mon_x),
            y0: to_px(s.y, mon_y),
            x1: to_px(s.x + s.w, mon_x),
            y1: to_px(s.y + s.h, mon_y),
            private: s.private,
            excluded: s.excluded,
            opaque: s.opaque,
            keep_top: (s.keep_top as f32 * scale).round() as i32,
        })
        .collect()
}

/// A window in image pixels.
#[derive(Clone, Copy)]
struct Rect {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    private: bool,
    /// Left out of the capture by its app: stripes.
    excluded: bool,
    /// Hides what is behind it (see-through overlays and the menu bar don't).
    opaque: bool,
    /// Rows at the top that stay visible (a browser's bars).
    keep_top: i32,
}

/// Black out the visible parts of the private rectangles and stripe the excluded ones
/// (topmost first). Whole windows hide what is behind them, including a browser's bars.
fn paint(rgba: &mut [u8], width: u32, height: u32, rects: &[Rect]) {
    for (i, r) in rects.iter().enumerate() {
        let Rect { x0, y0, x1, y1, private, excluded, .. } = *r;
        if !private && !excluded {
            continue;
        }
        let keep_top = if private { r.keep_top } else { 0 };
        for y in (y0 + keep_top).max(0)..y1.min(height as i32) {
            // This row of the window minus the windows in front of it.
            let mut spans = vec![(x0.max(0), x1.min(width as i32))];
            for a in rects[..i].iter().filter(|a| a.opaque) {
                if y < a.y0 || y >= a.y1 {
                    continue;
                }
                spans = spans
                    .into_iter()
                    .flat_map(|(s0, s1)| [(s0, s1.min(a.x0)), (s0.max(a.x1), s1)])
                    .filter(|(s0, s1)| s1 > s0)
                    .collect();
            }
            for (s0, s1) in spans {
                let row = y as usize * width as usize * 4;
                for (k, px) in rgba[row + s0 as usize * 4..row + s1 as usize * 4].chunks_exact_mut(4).enumerate() {
                    let color = if private || ((s0 as usize + k + y as usize) / 14) % 2 == 0 { [24, 24, 28, 255] } else { [46, 46, 54, 255] };
                    px.copy_from_slice(&color);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x0: i32, y0: i32, x1: i32, y1: i32, private: bool) -> Rect {
        Rect { x0, y0, x1, y1, private, excluded: false, opaque: true, keep_top: 0 }
    }

    #[test]
    fn only_the_visible_part_of_a_private_window_is_covered() {
        let (w, h) = (100u32, 50u32);
        let mut img = vec![255u8; (w * h * 4) as usize];
        // A normal window (topmost) overlaps the right half of a private window behind it.
        let rects = [r(50, 0, 100, 50, false), r(20, 10, 80, 40, true)];
        paint(&mut img, w, h, &rects);
        let px = |x: u32, y: u32| img[((y * w + x) * 4) as usize];
        assert_eq!(px(30, 20), 24); // private and visible: covered
        assert_eq!(px(60, 20), 255); // behind the normal window: untouched
        assert_eq!(px(10, 20), 255); // outside: untouched
        assert_eq!(px(30, 45), 255);
    }

    #[test]
    fn see_through_windows_in_front_do_not_protect_what_is_behind() {
        let (w, h) = (100u32, 50u32);
        let mut img = vec![255u8; (w * h * 4) as usize];
        // A full-screen overlay (menu bar layer, notification shade) above a private window.
        let overlay = Rect { opaque: false, ..r(0, 0, 100, 50, false) };
        paint(&mut img, w, h, &[overlay, r(20, 10, 80, 40, true)]);
        assert_eq!(img[((20 * w + 30) * 4) as usize], 24);
    }

    #[test]
    fn a_private_browser_tab_keeps_its_bars_visible() {
        let (w, h) = (100u32, 100u32);
        let mut img = vec![255u8; (w * h * 4) as usize];
        let rects = [Rect { keep_top: 20, ..r(0, 0, 100, 100, true) }];
        paint(&mut img, w, h, &rects);
        let px = |x: u32, y: u32| img[((y * w + x) * 4) as usize];
        assert_eq!(px(50, 10), 255); // tab strip: visible
        assert_eq!(px(50, 50), 24); // page: covered
    }

    fn win(app: &str, title: &str, own: bool) -> Win {
        Win { x: 0, y: 0, w: 800, h: 600, app: app.into(), title: title.into(), own, opaque: true, shell: false, excluded: false }
    }

    #[test]
    fn hiding_file_explorer_leaves_the_desktop_and_taskbar_alone() {
        let patterns = vec!["windows explorer".to_string()];
        let taskbar = Win { shell: true, ..win("Windows Explorer", "", false) };
        let s = shapes(&[win("Windows Explorer", "Downloads", false), taskbar], &patterns, 1.0, false);
        assert!(s[0].private);
        assert!(!s[1].private);
    }

    #[test]
    fn everything_hides_nothing_but_still_marks_self_hiding_windows() {
        let patterns: Vec<String> = DEFAULT_PRIVATE_APPS.iter().map(|s| s.to_string()).collect();
        let signal = Win { excluded: true, ..win("Signal", "Signal", false) };
        let s = shapes(&[win("KeePassXC", "x", false), signal, win("remotefriend", "RemoteFriends", true)], &patterns, 1.0, true);
        assert!(!s[0].private && !s[2].private);
        assert!(s[1].excluded && !s[1].private);
    }

    #[test]
    fn self_hiding_windows_get_stripes() {
        let (w, h) = (100u32, 40u32);
        let mut img = vec![255u8; (w * h * 4) as usize];
        paint(&mut img, w, h, &[Rect { excluded: true, ..r(0, 0, 100, 40, false) }]);
        let px = |x: u32, y: u32| img[((y * w + x) * 4) as usize];
        assert_ne!(px(1, 1), 255);
        assert_ne!(px(1, 1), px(20, 1)); // two shades
    }

    #[test]
    fn private_windows_are_found_by_app_title_or_owner() {
        let patterns: Vec<String> = DEFAULT_PRIVATE_APPS.iter().map(|s| s.to_string()).collect();
        let list = [
            win("KeePassXC", "Passwords.kdbx - KeePassXC", false),
            win("firefox", "Bitwarden Web Vault — Mozilla Firefox", false),
            win("gnome-terminal", "~", false),
            win("remotefriend", "RemoteFriends", true),
            win("firefox", "remotefriend - GitHub", false),
        ];
        let s = shapes(&list, &patterns, 1.0, false);
        assert!(s[0].private && s[0].keep_top == 0);
        assert!(s[1].private && s[1].keep_top == 88); // only the page of the tab
        assert!(!s[2].private);
        assert!(s[3].private);
        assert!(!s[4].private); // a web page about RemoteFriends is not RemoteFriends
    }

    #[test]
    fn viewer_clicks_and_keys_are_remembered_separately() {
        remote_input(&InputEvent::Key { key: remote_friend_common::RemoteKey::Enter, down: true }, false);
        let recent = |slot: &Mutex<Option<Instant>>| slot.lock().unwrap().is_some_and(|t| t.elapsed() < REMOTE_ECHO);
        assert!(recent(&REMOTE_KEYS));
        // The owner's own devices may use RemoteFriends.
        *REMOTE_POINTER.lock().unwrap() = None;
        remote_input(&InputEvent::MouseDown { button: remote_friend_common::MouseButton::Left }, true);
        assert!(!recent(&REMOTE_POINTER));
        remote_input(&InputEvent::MouseMove { x: 1, y: 1 }, false);
        assert!(!recent(&REMOTE_POINTER));
    }
}

/// On a real desktop (CI sets RF_GUI_TESTS=1 on its Windows and macOS machines): other apps'
/// windows are found, a hidden app's window is blacked out in a captured picture and shown in
/// the Everything view, a window kept out of captures is recognised, the pointer can be read.
#[cfg(all(test, any(windows, target_os = "macos")))]
mod gui {
    use super::*;
    use crate::winlist::Win;

    fn enabled() -> bool {
        std::env::var("RF_GUI_TESTS").is_ok_and(|v| v == "1")
    }

    fn wait_for(what: &str, find: impl Fn(&Win) -> bool) -> Win {
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            let list = crate::winlist::list();
            if let Some(w) = list.iter().find(|w| find(w)) {
                return w.clone();
            }
            let seen: Vec<(String, String)> = list.iter().map(|w| (w.app.clone(), w.title.clone())).collect();
            assert!(Instant::now() < deadline, "no {what} window among {seen:?}");
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    /// Capture the monitor holding `w`, mask it like a frame, and return the share of `w`'s
    /// area that was blacked out.
    #[cfg(windows)]
    fn covered_share(w: &Win, patterns: &[String], full: bool) -> f32 {
        let (cx, cy) = (w.x + w.w / 2, w.y + w.h / 2);
        let mon = xcap::Monitor::all()
            .expect("monitors")
            .into_iter()
            .find(|m| {
                let (mx, my) = (m.x().unwrap_or(0), m.y().unwrap_or(0));
                let (mw, mh) = (m.width().unwrap_or(0) as i32, m.height().unwrap_or(0) as i32);
                cx >= mx && cx < mx + mw && cy >= my && cy < my + mh
            })
            .expect("a monitor showing the window");
        let (mx, my) = (mon.x().unwrap_or(0), mon.y().unwrap_or(0));
        let img = mon.capture_image().expect("screen capture");
        let (iw, ih) = (img.width(), img.height());
        let mut data = img.into_raw();
        let list = shapes(&crate::winlist::list(), patterns, 1.0, full);
        paint(&mut data, iw, ih, &rects(&list, mx, my, 1.0));
        let (mut covered, mut total) = (0u32, 0u32);
        for y in (w.y - my).max(0)..(w.y + w.h - my).min(ih as i32) {
            for x in (w.x - mx).max(0)..(w.x + w.w - mx).min(iw as i32) {
                let i = (y as usize * iw as usize + x as usize) * 4;
                total += 1;
                if data[i..i + 3] == [24, 24, 28] {
                    covered += 1;
                }
            }
        }
        covered as f32 / total.max(1) as f32
    }

    #[cfg(windows)]
    #[test]
    fn windows_real_windows() {
        if !enabled() {
            return;
        }
        // 1. A hidden app (Notepad here) is blacked out in the safe view, shown in Everything.
        let mut notepad = std::process::Command::new("notepad.exe").spawn().expect("notepad");
        let w = wait_for("Notepad", |w| w.app.to_lowercase().contains("notepad") || w.title.to_lowercase().contains("notepad"));
        std::thread::sleep(Duration::from_millis(800));
        let patterns = vec!["notepad".to_string()];
        let safe = covered_share(&w, &patterns, false);
        let everything = covered_share(&w, &patterns, true);
        let _ = notepad.kill();
        eprintln!("Notepad {:?}: covered {safe:.2} in the safe view, {everything:.2} in Everything", (w.x, w.y, w.w, w.h));
        assert!(safe > 0.5, "only {safe:.2} of Notepad's window was blacked out in the safe view");
        assert!(everything < 0.05, "Everything must show Notepad ({everything:.2} covered)");

        // 2. The desktop and the taskbar are never hidden by the name of their app.
        for s in crate::winlist::list().iter().filter(|w| w.shell) {
            let shape = shapes(std::slice::from_ref(s), &[s.app.to_lowercase()], 1.0, false);
            assert!(!shape[0].private, "the shell window {s:?} would be blacked out");
        }

        // 3. A window its app keeps out of screen captures (like Signal's Screen security).
        let script = r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class Wda { [DllImport("user32.dll")] public static extern bool SetWindowDisplayAffinity(IntPtr h, uint a); }'
$f = New-Object System.Windows.Forms.Form
$f.Text = 'RF test hidden from capture'
$f.Add_Shown({ [void][Wda]::SetWindowDisplayAffinity($f.Handle, 0x11) })
[System.Windows.Forms.Application]::Run($f)
"#;
        let mut ps = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script])
            .spawn()
            .expect("powershell");
        wait_for("self-hiding test", |w| w.title.contains("RF test hidden from capture"));
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut excluded = false;
        while !excluded && Instant::now() < deadline {
            excluded = crate::winlist::list().iter().any(|w| w.title.contains("RF test hidden from capture") && w.excluded);
            std::thread::sleep(Duration::from_millis(300));
        }
        let _ = ps.kill();
        assert!(excluded, "a window kept out of captures was not recognised");

        // 4. The pointer: where we put it is where it is read.
        assert!(crate::input::pointer().is_some(), "the pointer position can't be read");
        // SAFETY: plain Win32 call with integer coordinates.
        unsafe { windows::Win32::UI::WindowsAndMessaging::SetCursorPos(123, 145) }.expect("SetCursorPos");
        assert_eq!(crate::input::pointer(), Some((123, 145)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_real_windows() {
        if !enabled() {
            return;
        }
        // Another app's window is found by its app name (no Screen Recording permission needed)
        // and would be blacked out; the pointer can be read.
        let file = std::env::temp_dir().join("rf-gui-test.txt");
        std::fs::write(&file, "RemoteFriends test").expect("test file");
        let _ = std::process::Command::new("open").args(["-a", "TextEdit"]).arg(&file).status();
        let w = wait_for("TextEdit", |w| w.app == "TextEdit");
        let _ = std::process::Command::new("osascript").args(["-e", "quit app \"TextEdit\""]).status();
        let shape = shapes(&[w.clone()], &["textedit".to_string()], 1.0, false);
        assert!(shape[0].private, "TextEdit {w:?} would not be blacked out");
        assert!(shapes(&[w], &["textedit".to_string()], 1.0, true)[0].private == false);
        assert!(crate::input::pointer().is_some(), "the pointer position can't be read");
    }
}
