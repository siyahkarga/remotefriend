//! Apply remote input to the local desktop.
//!
//! - Linux Wayland (GNOME/KDE): RemoteDesktop portal (`wayland.rs`).
//! - Linux X11, Windows, macOS: enigo (dedicated thread, in order).
//!
//! Mouse coordinates arrive here in CAPTURE space (physical pixels, before downscaling);
//! the session layer maps them from the sent frame size.

use anyhow::Result;
use remote_friend_common::{InputEvent, MouseButton, RemoteKey};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

/// Geometry of the captured monitor (enigo expects logical coordinates).
#[derive(Clone, Copy)]
pub(crate) struct CaptureGeo {
    pub scale: f32,
    pub mon_x: i32,
    pub mon_y: i32,
}

static GEO: Mutex<CaptureGeo> = Mutex::new(CaptureGeo { scale: 1.0, mon_x: 0, mon_y: 0 });

pub(crate) fn set_geo(g: CaptureGeo) {
    if let Ok(mut cur) = GEO.lock() {
        *cur = g;
    }
}

/// Tracks held keys/buttons: if the session drops, stuck Ctrl/Shift/mouse buttons are released.
#[derive(Default)]
struct Held {
    keys: HashSet<RemoteKey>,
    buttons: Vec<MouseButton>,
}

static HELD: Mutex<Option<Held>> = Mutex::new(None);

fn track(ev: &InputEvent) {
    let Ok(mut g) = HELD.lock() else { return };
    let held = g.get_or_insert_with(Held::default);
    match ev {
        InputEvent::Key { key, down: true } => {
            held.keys.insert(*key);
        }
        InputEvent::Key { key, down: false } => {
            held.keys.remove(key);
        }
        InputEvent::MouseDown { button } => {
            if !held.buttons.iter().any(|b| same_button(*b, *button)) {
                held.buttons.push(*button);
            }
        }
        InputEvent::MouseUp { button } => held.buttons.retain(|b| !same_button(*b, *button)),
        _ => {}
    }
}

fn same_button(a: MouseButton, b: MouseButton) -> bool {
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
}

/// Call when the session ends: release everything still held down.
pub(crate) fn release_all() {
    let held = HELD.lock().ok().and_then(|mut g| g.take());
    if let Some(held) = held {
        for key in held.keys {
            let _ = dispatch(InputEvent::Key { key, down: false });
        }
        for button in held.buttons {
            let _ = dispatch(InputEvent::MouseUp { button });
        }
    }
}

/// Queue the input (non-blocking).
pub(crate) fn apply(ev: InputEvent) -> Result<()> {
    // Update the held state only if the event was actually queued; that way a dropped
    // "release" event can still be made up for by release_all().
    let tracked = matches!(ev, InputEvent::Key { .. } | InputEvent::MouseDown { .. } | InputEvent::MouseUp { .. });
    let copy = if tracked { Some(ev.clone()) } else { None };
    dispatch(ev)?;
    if let Some(ev) = copy {
        track(&ev);
    }
    Ok(())
}

fn dispatch(ev: InputEvent) -> Result<()> {
    if std::env::var("RF_INPUT_DRY").map(|v| v == "1").unwrap_or(false) {
        tracing::info!("input (dry-run): {ev:?}");
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    if crate::wayland::input_ready() {
        if crate::wayland::send_input(ev) {
            return Ok(());
        }
        anyhow::bail!("portal input queue full/closed");
    }
    enigo_send(ev)
}

/// On Wayland without the portal, show the warning once.
fn warn_wayland_fallback() {
    #[cfg(target_os = "linux")]
    {
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if crate::wayland::is_wayland() && !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            tracing::warn!(
                "portal input not ready on Wayland; enigo (X11) can only control XWayland windows"
            );
        }
    }
}

static ENIGO_TX: OnceLock<std::sync::mpsc::SyncSender<InputEvent>> = OnceLock::new();

fn enigo_send(ev: InputEvent) -> Result<()> {
    use std::sync::mpsc::TrySendError;
    warn_wayland_fallback();
    let tx = ENIGO_TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<InputEvent>(512);
        std::thread::Builder::new()
            .name("rf-input".into())
            .spawn(move || {
                use enigo::{Enigo, Settings};
                let mut enigo = match Enigo::new(&Settings::default()) {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::error!("failed to open input driver (macOS: Accessibility permission required): {e}");
                        return;
                    }
                };
                while let Ok(ev) = rx.recv() {
                    if let Err(e) = enigo_apply(&mut enigo, ev) {
                        tracing::warn!("input apply error: {e:#}");
                    }
                }
            })
            .expect("failed to start input thread");
        tx
    });
    match tx.try_send(ev) {
        Ok(()) => Ok(()),
        // For mouse moves the newest position follows right away; dropping the old one
        // beats letting the queue grow and lag seconds behind.
        Err(TrySendError::Full(InputEvent::MouseMove { .. })) => Ok(()),
        Err(TrySendError::Full(_)) => anyhow::bail!("input queue full"),
        Err(TrySendError::Disconnected(_)) => anyhow::bail!("input driver closed"),
    }
}

fn enigo_apply(enigo: &mut enigo::Enigo, ev: InputEvent) -> Result<()> {
    use enigo::{Axis, Coordinate, Direction, Keyboard, Mouse};
    match ev {
        InputEvent::MouseMove { x, y } => {
            let g = *GEO.lock().unwrap();
            // Windows: this process is per-monitor DPI aware, so capture, monitor position and
            // SetCursorPos all use physical pixels on the whole virtual desktop (every monitor,
            // any scaling). enigo's absolute move only covers the primary monitor.
            #[cfg(windows)]
            {
                let _ = enigo;
                // SAFETY: plain Win32 call with integer coordinates.
                unsafe { windows::Win32::UI::WindowsAndMessaging::SetCursorPos(g.mon_x + x as i32, g.mon_y + y as i32) }
                    .map_err(|e| anyhow::anyhow!("SetCursorPos: {e}"))?;
            }
            // macOS: capture is in pixels (Retina), the pointer in points.
            #[cfg(not(windows))]
            {
                let lx = g.mon_x + (x as f32 / g.scale).round() as i32;
                let ly = g.mon_y + (y as f32 / g.scale).round() as i32;
                enigo.move_mouse(lx, ly, Coordinate::Abs)?
            }
        }
        InputEvent::MouseDown { button } => enigo.button(map_btn(button), Direction::Press)?,
        InputEvent::MouseUp { button } => enigo.button(map_btn(button), Direction::Release)?,
        InputEvent::Scroll { dx, dy } => {
            if dy != 0 {
                enigo.scroll(dy.clamp(-20, 20), Axis::Vertical)?;
            }
            if dx != 0 {
                enigo.scroll(dx.clamp(-20, 20), Axis::Horizontal)?;
            }
        }
        InputEvent::Key { key, down } => {
            let dir = if down { Direction::Press } else { Direction::Release };
            enigo.key(map_key(key), dir)?;
        }
        InputEvent::Text(text) => {
            let clean: String = text
                .chars()
                .take(4096)
                .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                .collect();
            if !clean.is_empty() {
                enigo.text(&clean)?;
            }
        }
    }
    Ok(())
}

fn map_btn(b: MouseButton) -> enigo::Button {
    match b {
        MouseButton::Left => enigo::Button::Left,
        MouseButton::Right => enigo::Button::Right,
        MouseButton::Middle => enigo::Button::Middle,
    }
}

fn map_key(k: RemoteKey) -> enigo::Key {
    use enigo::Key as K;
    use RemoteKey as R;
    match k {
        R::Char(c) => K::Unicode(c),
        R::Enter => K::Return,
        R::Tab => K::Tab,
        R::Backspace => K::Backspace,
        R::Escape => K::Escape,
        R::Delete => K::Delete,
        R::Insert => insert_key(),
        R::Home => K::Home,
        R::End => K::End,
        R::PageUp => K::PageUp,
        R::PageDown => K::PageDown,
        R::Up => K::UpArrow,
        R::Down => K::DownArrow,
        R::Left => K::LeftArrow,
        R::Right => K::RightArrow,
        R::F1 => K::F1,
        R::F2 => K::F2,
        R::F3 => K::F3,
        R::F4 => K::F4,
        R::F5 => K::F5,
        R::F6 => K::F6,
        R::F7 => K::F7,
        R::F8 => K::F8,
        R::F9 => K::F9,
        R::F10 => K::F10,
        R::F11 => K::F11,
        R::F12 => K::F12,
        R::Shift => K::Shift,
        R::Ctrl => K::Control,
        R::Alt => K::Alt,
        R::Meta => K::Meta,
        R::CapsLock => K::CapsLock,
        R::NumLock => numlock_key(),
        R::PrintScreen => print_key(),
        R::Pause => pause_key(),
    }
}

// These keys have no macOS equivalent; fall back to a harmless key.
#[cfg(not(target_os = "macos"))]
fn insert_key() -> enigo::Key {
    enigo::Key::Insert
}
#[cfg(target_os = "macos")]
fn insert_key() -> enigo::Key {
    enigo::Key::Help
}
#[cfg(not(target_os = "macos"))]
fn numlock_key() -> enigo::Key {
    enigo::Key::Numlock
}
#[cfg(target_os = "macos")]
fn numlock_key() -> enigo::Key {
    enigo::Key::Shift
}
#[cfg(not(target_os = "macos"))]
fn print_key() -> enigo::Key {
    enigo::Key::PrintScr
}
#[cfg(target_os = "macos")]
fn print_key() -> enigo::Key {
    enigo::Key::F13
}
#[cfg(not(target_os = "macos"))]
fn pause_key() -> enigo::Key {
    enigo::Key::Pause
}
#[cfg(target_os = "macos")]
fn pause_key() -> enigo::Key {
    enigo::Key::F15
}

/// X11 keysym (for the Wayland portal's `NotifyKeyboardKeysym`).
#[allow(dead_code)]
pub(crate) fn keysym(k: RemoteKey) -> u32 {
    use RemoteKey as R;
    match k {
        R::Char(c) => char_keysym(c),
        R::Enter => 0xff0d,
        R::Tab => 0xff09,
        R::Backspace => 0xff08,
        R::Escape => 0xff1b,
        R::Delete => 0xffff,
        R::Insert => 0xff63,
        R::Home => 0xff50,
        R::End => 0xff57,
        R::PageUp => 0xff55,
        R::PageDown => 0xff56,
        R::Up => 0xff52,
        R::Down => 0xff54,
        R::Left => 0xff51,
        R::Right => 0xff53,
        R::F1 => 0xffbe,
        R::F2 => 0xffbf,
        R::F3 => 0xffc0,
        R::F4 => 0xffc1,
        R::F5 => 0xffc2,
        R::F6 => 0xffc3,
        R::F7 => 0xffc4,
        R::F8 => 0xffc5,
        R::F9 => 0xffc6,
        R::F10 => 0xffc7,
        R::F11 => 0xffc8,
        R::F12 => 0xffc9,
        R::Shift => 0xffe1,
        R::Ctrl => 0xffe3,
        R::Alt => 0xffe9,
        R::Meta => 0xffeb,
        R::CapsLock => 0xffe5,
        R::NumLock => 0xff7f,
        R::PrintScreen => 0xff61,
        R::Pause => 0xff13,
    }
}

/// Unicode character -> keysym. Latin-1 maps directly; Turkish letters use the classic keysyms
/// (xkb layouts include them); everything else uses a Unicode keysym (0x01000000 + code point).
#[allow(dead_code)]
pub(crate) fn char_keysym(c: char) -> u32 {
    let cp = c as u32;
    match c {
        'ş' => 0x1ba,
        'Ş' => 0x1aa,
        'ğ' => 0x2bb,
        'Ğ' => 0x2ab,
        'ı' => 0x2b9,
        'İ' => 0x2a9,
        '€' => 0x20ac,
        _ if (0x20..=0x7e).contains(&cp) || (0xa0..=0xff).contains(&cp) => cp,
        _ => 0x0100_0000 + cp,
    }
}

/// Browser `KeyboardEvent.key` -> RemoteKey. A single character is a letter; anything else is a key name.
pub(crate) fn key_from_web(code: &str) -> Option<RemoteKey> {
    let mut chars = code.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(RemoteKey::Char(c));
    }
    Some(match code {
        "Enter" => RemoteKey::Enter,
        "Tab" => RemoteKey::Tab,
        "Backspace" => RemoteKey::Backspace,
        "Escape" | "Esc" => RemoteKey::Escape,
        "Delete" | "Del" => RemoteKey::Delete,
        "Insert" => RemoteKey::Insert,
        "Home" => RemoteKey::Home,
        "End" => RemoteKey::End,
        "PageUp" => RemoteKey::PageUp,
        "PageDown" => RemoteKey::PageDown,
        "ArrowUp" | "Up" => RemoteKey::Up,
        "ArrowDown" | "Down" => RemoteKey::Down,
        "ArrowLeft" | "Left" => RemoteKey::Left,
        "ArrowRight" | "Right" => RemoteKey::Right,
        "F1" => RemoteKey::F1,
        "F2" => RemoteKey::F2,
        "F3" => RemoteKey::F3,
        "F4" => RemoteKey::F4,
        "F5" => RemoteKey::F5,
        "F6" => RemoteKey::F6,
        "F7" => RemoteKey::F7,
        "F8" => RemoteKey::F8,
        "F9" => RemoteKey::F9,
        "F10" => RemoteKey::F10,
        "F11" => RemoteKey::F11,
        "F12" => RemoteKey::F12,
        "Shift" => RemoteKey::Shift,
        "Control" => RemoteKey::Ctrl,
        "Alt" | "AltGraph" => RemoteKey::Alt,
        "Meta" | "OS" | "Super" => RemoteKey::Meta,
        "CapsLock" => RemoteKey::CapsLock,
        "NumLock" => RemoteKey::NumLock,
        "PrintScreen" => RemoteKey::PrintScreen,
        "Pause" => RemoteKey::Pause,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysyms_for_common_characters() {
        assert_eq!(char_keysym('a'), 0x61);
        assert_eq!(char_keysym('ç'), 0xe7);
        assert_eq!(char_keysym('ş'), 0x1ba);
        assert_eq!(char_keysym('я'), 0x0100_044f);
        assert_eq!(keysym(RemoteKey::Enter), 0xff0d);
    }

    #[test]
    fn web_key_names() {
        assert_eq!(key_from_web("a"), Some(RemoteKey::Char('a')));
        assert_eq!(key_from_web("ğ"), Some(RemoteKey::Char('ğ')));
        assert_eq!(key_from_web(" "), Some(RemoteKey::Char(' ')));
        assert_eq!(key_from_web("ArrowLeft"), Some(RemoteKey::Left));
        assert_eq!(key_from_web("Dead"), None);
    }
}
