//! What viewers see: "safe" (default) or "full".
//!
//! Safe: the RemoteFriend window is kept out of the picture (the app minimizes it and, on
//! Windows/macOS, excludes it from capture) and windows of private apps — password managers
//! and anything the user adds — are blacked out. Only the visible part of such a window is
//! covered, so windows in front of it stay visible. This needs window positions, which
//! Windows, macOS and X11 provide; on Wayland the desktop does not tell apps where windows are.
//!
//! Full: everything is shown, as if sitting at the computer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

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
        "Safe view: RemoteFriend and private apps are hidden from viewers."
    });
}

/// Name parts of apps hidden in safe mode (case is ignored).
pub fn private_apps() -> Vec<String> {
    let list = remote_friend_common::identity::load_host_settings().private_apps;
    if list.is_empty() {
        DEFAULT_PRIVATE_APPS.iter().map(|s| s.to_string()).collect()
    } else {
        list
    }
}

pub fn set_private_apps(list: Vec<String>) {
    let mut s = remote_friend_common::identity::load_host_settings();
    s.private_apps = list.into_iter().map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()).collect();
    remote_friend_common::identity::save_host_settings(&s);
    *cache().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// A top-level window (screen coordinates); topmost first in the list.
struct Win {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    private: bool,
}

fn cache() -> &'static Mutex<Option<(Instant, Vec<Win>)>> {
    static C: Mutex<Option<(Instant, Vec<Win>)>> = Mutex::new(None);
    &C
}

/// Visible windows, refreshed at most every 400 ms (listing windows is not free).
fn with_windows<R>(f: impl FnOnce(&[Win]) -> R) -> R {
    let mut g = cache().lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_none_or(|(t, _)| t.elapsed() > Duration::from_millis(400)) {
        let own = std::process::id();
        let patterns = private_apps();
        let list = xcap::Window::all()
            .map(|all| {
                all.iter()
                    .filter(|w| !w.is_minimized().unwrap_or(false))
                    .filter_map(|w| {
                        let (x, y) = (w.x().ok()?, w.y().ok()?);
                        let (ww, hh) = (w.width().ok()? as i32, w.height().ok()? as i32);
                        if ww <= 0 || hh <= 0 {
                            return None;
                        }
                        let name = format!("{} {}", w.app_name().unwrap_or_default(), w.title().unwrap_or_default()).to_lowercase();
                        let private = w.pid().is_ok_and(|p| p == own)
                            || name.contains("remotefriend")
                            || patterns.iter().any(|p| name.contains(p.as_str()));
                        Some(Win { x, y, w: ww, h: hh, private })
                    })
                    .collect()
            })
            .unwrap_or_default();
        *g = Some((Instant::now(), list));
    }
    f(g.as_ref().map(|(_, l)| l.as_slice()).unwrap_or(&[]))
}

/// Safe view: black out the visible parts of private windows in a captured monitor image
/// (RGBA, `width`×`height`, monitor origin at `mon_x`/`mon_y`, `scale` pixels per window unit).
pub(crate) fn mask(rgba: &mut [u8], width: u32, height: u32, mon_x: i32, mon_y: i32, scale: f32) {
    if full_view() {
        return;
    }
    let rects: Vec<((i32, i32, i32, i32), bool)> = with_windows(|wins| {
        let to_px = |v: i32, origin: i32| ((v - origin) as f32 * scale).round() as i32;
        wins.iter()
            .map(|w| ((to_px(w.x, mon_x), to_px(w.y, mon_y), to_px(w.x + w.w, mon_x), to_px(w.y + w.h, mon_y)), w.private))
            .collect()
    });
    paint(rgba, width, height, &rects);
}

/// Black out the visible parts of the private rectangles; `rects` are (x0, y0, x1, y1) in
/// image pixels, topmost first.
fn paint(rgba: &mut [u8], width: u32, height: u32, rects: &[((i32, i32, i32, i32), bool)]) {
    for (i, &((x0, y0, x1, y1), private)) in rects.iter().enumerate() {
        if !private {
            continue;
        }
        for y in y0.max(0)..y1.min(height as i32) {
            // This row of the window minus the windows in front of it.
            let mut spans = vec![(x0.max(0), x1.min(width as i32))];
            for &((ax0, ay0, ax1, ay1), _) in &rects[..i] {
                if y < ay0 || y >= ay1 {
                    continue;
                }
                spans = spans
                    .into_iter()
                    .flat_map(|(s0, s1)| [(s0, s1.min(ax0)), (s0.max(ax1), s1)])
                    .filter(|(s0, s1)| s1 > s0)
                    .collect();
            }
            for (s0, s1) in spans {
                let row = y as usize * width as usize * 4;
                for px in rgba[row + s0 as usize * 4..row + s1 as usize * 4].chunks_exact_mut(4) {
                    px.copy_from_slice(&[24, 24, 28, 255]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_visible_part_of_a_private_window_is_covered() {
        let (w, h) = (100u32, 50u32);
        let mut img = vec![255u8; (w * h * 4) as usize];
        // A normal window (topmost) overlaps the right half of a private window behind it.
        let rects = [((50, 0, 100, 50), false), ((20, 10, 80, 40), true)];
        paint(&mut img, w, h, &rects);
        let px = |x: u32, y: u32| img[((y * w + x) * 4) as usize];
        assert_eq!(px(30, 20), 24); // private and visible: covered
        assert_eq!(px(60, 20), 255); // behind the normal window: untouched
        assert_eq!(px(10, 20), 255); // outside: untouched
        assert_eq!(px(30, 45), 255);
    }
}
