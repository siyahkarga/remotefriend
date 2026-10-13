//! Keep the RemoteFriends window out of the shared picture while someone is connected, so
//! a viewer cannot see the password or settings. Windows 10 (2004+) and macOS support
//! this; on X11 the host blacks the window out instead. Wayland can't do either.

use raw_window_handle::HasWindowHandle;

pub(crate) fn exclude_from_capture(frame: &eframe::Frame, on: bool) {
    let Ok(handle) = frame.window_handle() else { return };
    match handle.as_raw() {
        #[cfg(windows)]
        raw_window_handle::RawWindowHandle::Win32(h) => {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE};
            let affinity = if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
            // SAFETY: the handle belongs to this app's live window.
            if let Err(e) = unsafe { SetWindowDisplayAffinity(HWND(h.hwnd.get() as *mut _), affinity) } {
                tracing::debug!("cannot hide the window from capture: {e}");
            }
        }
        #[cfg(target_os = "macos")]
        raw_window_handle::RawWindowHandle::AppKit(h) => {
            use objc2_app_kit::{NSView, NSWindowSharingType};
            // SAFETY: the view belongs to this app's live window; called on the main thread.
            let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
            if let Some(window) = view.window() {
                window.setSharingType(if on { NSWindowSharingType::None } else { NSWindowSharingType::ReadOnly });
            }
        }
        _ => {
            let _ = on;
        }
    }
}

/// A Linux Wayland desktop: apps can't learn where other windows are, so the safe view can't
/// black out private apps there, and RemoteFriends can't keep its own window out of the picture.
pub(crate) fn wayland() -> bool {
    // Development aid: RF_DEMO_WAYLAND=1 shows the Wayland screens on X11 (debug builds).
    #[cfg(debug_assertions)]
    if std::env::var("RF_DEMO_WAYLAND").is_ok_and(|v| v == "1") {
        return true;
    }
    cfg!(target_os = "linux")
        && (std::env::var("WAYLAND_DISPLAY").is_ok_and(|v| !v.is_empty())
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("wayland")))
}
