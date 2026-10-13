//! The windows on screen, topmost first: where they are and which app they belong to (for
//! the safe view, privacy.rs). One call lists everything at once, so it can run for every
//! captured frame and the black boxes follow windows that move.
//!
//! - Windows: EnumWindows (through xcap); app names are cached per process.
//! - macOS: one CGWindowListCopyWindowInfo call (positions in points).
//! - Linux X11: _NET_CLIENT_LIST_STACKING over one X connection, all requests sent at once.
//! - Linux Wayland: nothing; the desktop doesn't tell apps where windows are.

/// A visible top-level window (screen coordinates).
#[derive(Clone, Debug)]
pub(crate) struct Win {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// App name as the system shows it ("KeePassXC", "firefox").
    pub app: String,
    pub title: String,
    /// One of this process's windows (RemoteFriends itself).
    pub own: bool,
    /// Hides what is behind it. False for the menu bar, the Dock and see-through overlays.
    pub opaque: bool,
}

/// Windows that are on screen now, topmost first. Empty if the system can't tell.
pub(crate) fn list() -> Vec<Win> {
    imp::list()
}

/// Where the pointer is on X11 (None elsewhere, see input.rs).
#[cfg(target_os = "linux")]
pub(crate) fn pointer() -> Option<(i32, i32)> {
    imp::pointer()
}

#[cfg(windows)]
mod imp {
    use super::Win;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Reading an app's name opens its executable's version info: once per process.
    static APPS: Mutex<Option<(Instant, HashMap<u32, String>)>> = Mutex::new(None);

    pub(super) fn list() -> Vec<Win> {
        let Ok(all) = xcap::Window::all() else { return Vec::new() };
        let own = std::process::id();
        let mut apps = APPS.lock().unwrap_or_else(|e| e.into_inner());
        let (since, names) = apps.get_or_insert_with(|| (Instant::now(), HashMap::new()));
        // Process ids are reused: forget the names now and then.
        if since.elapsed() > Duration::from_secs(30) {
            *since = Instant::now();
            names.clear();
        }
        all.iter()
            .filter(|w| !w.is_minimized().unwrap_or(false))
            .filter_map(|w| {
                let (x, y) = (w.x().ok()?, w.y().ok()?);
                let (ww, hh) = (w.width().ok()? as i32, w.height().ok()? as i32);
                let pid = w.pid().unwrap_or(0);
                let app = names.entry(pid).or_insert_with(|| w.app_name().unwrap_or_default()).clone();
                Some(Win { x, y, w: ww, h: hh, app, title: w.title().unwrap_or_default(), own: pid == own, opaque: true })
            })
            .collect()
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Win;
    use objc2_core_foundation::{CFDictionary, CFNumber, CFNumberType, CFString, CGRect};
    use objc2_core_graphics::{CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo, CGWindowListOption};
    use std::ffi::c_void;

    fn value(dict: &CFDictionary, key: &str) -> Option<*const c_void> {
        let key = CFString::from_str(key);
        // SAFETY: a valid dictionary and key; the value stays owned by the dictionary.
        let v = unsafe { dict.value((&*key as *const CFString).cast()) };
        (!v.is_null()).then_some(v)
    }

    fn number(dict: &CFDictionary, key: &str) -> Option<f64> {
        let n = value(dict, key)? as *const CFNumber;
        let mut v = 0f64;
        // SAFETY: the window list stores numbers under these keys.
        let ok = unsafe { (*n).value(CFNumberType::Float64Type, &mut v as *mut f64 as *mut c_void) };
        ok.then_some(v)
    }

    fn string(dict: &CFDictionary, key: &str) -> String {
        // SAFETY: the window list stores strings under these keys.
        value(dict, key).map(|s| unsafe { (*(s as *const CFString)).to_string() }).unwrap_or_default()
    }

    fn bounds(dict: &CFDictionary) -> Option<CGRect> {
        let d = value(dict, "kCGWindowBounds")? as *const CFDictionary;
        let mut r = CGRect::default();
        // SAFETY: kCGWindowBounds is a rectangle dictionary.
        unsafe { CGRectMakeWithDictionaryRepresentation(Some(&*d), &mut r) }.then_some(r)
    }

    pub(super) fn list() -> Vec<Win> {
        let own = std::process::id() as f64;
        // Front to back, only what is on the current Space. Without the Screen Recording
        // permission titles are missing; app names are always there.
        let Some(all) =
            CGWindowListCopyWindowInfo(CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements, 0)
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..all.count() {
            // SAFETY: an index below the count; every element is a window dictionary.
            let d = unsafe { all.value_at_index(i) } as *const CFDictionary;
            if d.is_null() {
                continue;
            }
            let d = unsafe { &*d };
            let Some(r) = bounds(d) else { continue };
            let alpha = number(d, "kCGWindowAlpha").unwrap_or(1.0);
            if alpha <= 0.01 || r.size.width < 1.0 || r.size.height < 1.0 {
                continue;
            }
            // Layer 0 holds normal windows; above it are the menu bar, the Dock, menus and overlays.
            let layer = number(d, "kCGWindowLayer").unwrap_or(0.0);
            let (x0, y0) = (r.origin.x.floor(), r.origin.y.floor());
            out.push(Win {
                x: x0 as i32,
                y: y0 as i32,
                w: ((r.origin.x + r.size.width).ceil() - x0) as i32,
                h: ((r.origin.y + r.size.height).ceil() - y0) as i32,
                app: string(d, "kCGWindowOwnerName"),
                title: string(d, "kCGWindowName"),
                own: number(d, "kCGWindowOwnerPID") == Some(own),
                opaque: layer == 0.0 && alpha >= 0.99,
            });
        }
        out
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Win;
    use std::sync::OnceLock;
    use xcb::x;

    struct Atoms {
        stacking: x::Atom,
        frame: x::Atom,
        gtk_frame: x::Atom,
        name: x::Atom,
        utf8: x::Atom,
        pid: x::Atom,
        state: x::Atom,
        hidden: x::Atom,
        desktop: x::Atom,
        current_desktop: x::Atom,
    }

    struct Display {
        conn: xcb::Connection,
        root: x::Window,
        atoms: Atoms,
    }

    fn display() -> Option<&'static Display> {
        static D: OnceLock<Option<Display>> = OnceLock::new();
        D.get_or_init(|| {
            if crate::wayland::is_wayland() || std::env::var_os("DISPLAY").is_none() {
                return None;
            }
            let (conn, screen) = xcb::Connection::connect(None).ok()?;
            let root = conn.get_setup().roots().nth(screen.max(0) as usize)?.root();
            let atom = |name: &str| {
                let cookie = conn.send_request(&x::InternAtom { only_if_exists: false, name: name.as_bytes() });
                conn.wait_for_reply(cookie).map(|r| r.atom()).unwrap_or(x::ATOM_NONE)
            };
            let atoms = Atoms {
                stacking: atom("_NET_CLIENT_LIST_STACKING"),
                frame: atom("_NET_FRAME_EXTENTS"),
                gtk_frame: atom("_GTK_FRAME_EXTENTS"),
                name: atom("_NET_WM_NAME"),
                utf8: atom("UTF8_STRING"),
                pid: atom("_NET_WM_PID"),
                state: atom("_NET_WM_STATE"),
                hidden: atom("_NET_WM_STATE_HIDDEN"),
                desktop: atom("_NET_WM_DESKTOP"),
                current_desktop: atom("_NET_CURRENT_DESKTOP"),
            };
            Some(Display { conn, root, atoms })
        })
        .as_ref()
    }

    fn get(window: x::Window, property: x::Atom, r#type: x::Atom, long_length: u32) -> x::GetProperty {
        x::GetProperty { delete: false, window, property, r#type, long_offset: 0, long_length }
    }

    fn u32s(r: &x::GetPropertyReply) -> Vec<u32> {
        if r.format() == 32 { r.value::<u32>().to_vec() } else { Vec::new() }
    }

    fn text(r: &x::GetPropertyReply) -> String {
        if r.format() == 8 { String::from_utf8_lossy(r.value::<u8>()).into_owned() } else { String::new() }
    }

    /// Left, right, top, bottom (both frame-extent properties use this order).
    fn extents(r: &x::GetPropertyReply) -> [i32; 4] {
        match u32s(r).as_slice() {
            &[l, rt, t, b, ..] => [l, rt, t, b].map(|v| v.min(400) as i32),
            _ => [0; 4],
        }
    }

    /// Where the pointer is (root coordinates).
    pub(super) fn pointer() -> Option<(i32, i32)> {
        let d = display()?;
        let r = d.conn.wait_for_reply(d.conn.send_request(&x::QueryPointer { window: d.root })).ok()?;
        Some((r.root_x() as i32, r.root_y() as i32))
    }

    pub(super) fn list() -> Vec<Win> {
        let Some(d) = display() else { return Vec::new() };
        let (c, a) = (&d.conn, &d.atoms);
        let stack = c.send_request(&get(d.root, a.stacking, x::ATOM_NONE, 4096));
        let current = c.send_request(&get(d.root, a.current_desktop, x::ATOM_CARDINAL, 1));
        let windows: Vec<x::Window> = match c.wait_for_reply(stack) {
            Ok(r) if r.format() == 32 => r.value::<x::Window>().to_vec(),
            _ => return Vec::new(),
        };
        let current = c.wait_for_reply(current).ok().and_then(|r| u32s(&r).first().copied());
        // All requests first, then the replies: one round trip for the whole list.
        let cookies: Vec<_> = windows
            .iter()
            .map(|&w| {
                (
                    c.send_request(&x::GetWindowAttributes { window: w }),
                    c.send_request(&x::GetGeometry { drawable: x::Drawable::Window(w) }),
                    c.send_request(&x::TranslateCoordinates { src_window: w, dst_window: d.root, src_x: 0, src_y: 0 }),
                    c.send_request(&get(w, a.frame, x::ATOM_CARDINAL, 4)),
                    c.send_request(&get(w, a.gtk_frame, x::ATOM_CARDINAL, 4)),
                    c.send_request(&get(w, x::ATOM_WM_CLASS, x::ATOM_STRING, 64)),
                    c.send_request(&get(w, a.name, a.utf8, 64)),
                    c.send_request(&get(w, x::ATOM_WM_NAME, x::ATOM_NONE, 64)),
                    c.send_request(&get(w, a.pid, x::ATOM_CARDINAL, 1)),
                    c.send_request(&get(w, a.state, x::ATOM_ATOM, 16)),
                    c.send_request(&get(w, a.desktop, x::ATOM_CARDINAL, 1)),
                )
            })
            .collect();
        let own = std::process::id();
        let mut out = Vec::new();
        for (attrs, geo, pos, frame, gtk, class, name, wm_name, pid, state, desktop) in cookies {
            // Wait for every reply even when the window is skipped (a window can vanish meanwhile).
            let attrs = c.wait_for_reply(attrs);
            let geo = c.wait_for_reply(geo);
            let pos = c.wait_for_reply(pos);
            let (frame, gtk) = (c.wait_for_reply(frame), c.wait_for_reply(gtk));
            let (class, name, wm_name) = (c.wait_for_reply(class), c.wait_for_reply(name), c.wait_for_reply(wm_name));
            let (pid, state, desktop) = (c.wait_for_reply(pid), c.wait_for_reply(state), c.wait_for_reply(desktop));
            let (Ok(attrs), Ok(geo), Ok(pos)) = (attrs, geo, pos) else { continue };
            // Minimized windows and windows on other workspaces are unmapped or marked hidden.
            if attrs.map_state() != x::MapState::Viewable {
                continue;
            }
            if state.as_ref().is_ok_and(|s| s.format() == 32 && s.value::<x::Atom>().contains(&a.hidden)) {
                continue;
            }
            let on_desktop = desktop.ok().and_then(|r| u32s(&r).first().copied());
            if let (Some(on), Some(cur)) = (on_desktop, current) {
                if on != cur && on != u32::MAX {
                    continue;
                }
            }
            // Window manager frame (title bar) around it, invisible shadow inside it (GTK).
            let [fl, fr, ft, fb] = frame.map(|r| extents(&r)).unwrap_or([0; 4]);
            let [gl, gr, gt, gb] = gtk.map(|r| extents(&r)).unwrap_or([0; 4]);
            let (w, h) = (geo.width() as i32 + fl + fr - gl - gr, geo.height() as i32 + ft + fb - gt - gb);
            if w <= 0 || h <= 0 {
                continue;
            }
            let class = class.map(|r| text(&r)).unwrap_or_default();
            let mut parts = class.split('\0').filter(|s| !s.is_empty());
            let instance = parts.next().unwrap_or_default().to_string();
            let app = parts.next().map(str::to_string).unwrap_or(instance);
            let title = match name.map(|r| text(&r)) {
                Ok(t) if !t.is_empty() => t,
                _ => wm_name.map(|r| text(&r)).unwrap_or_default(),
            };
            out.push(Win {
                x: pos.dst_x() as i32 - fl + gl,
                y: pos.dst_y() as i32 - ft + gt,
                w,
                h,
                app,
                title,
                own: pid.ok().and_then(|r| u32s(&r).first().copied()) == Some(own),
                opaque: true,
            });
        }
        // The stacking list goes bottom to top.
        out.reverse();
        out
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod imp {
    pub(super) fn list() -> Vec<super::Win> {
        Vec::new()
    }
}
