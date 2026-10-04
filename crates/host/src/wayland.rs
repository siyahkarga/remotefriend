//! Wayland (GNOME/KDE): Portal RemoteDesktop + ScreenCast + PipeWire.
//!
//! Why this exists: on Wayland, apps cannot read the screen directly or inject mouse/keyboard
//! events into other windows. `xcap` calls the screenshot API for every frame
//! (shutter sound), and `enigo` delivers X11 events only to XWayland windows:
//! the real desktop cannot be controlled. Here the desktop is asked ONCE
//! ("remote control + screen sharing") and the permission is remembered with a persistent token;
//! video flows silently from PipeWire and input goes through the portal to the real desktop.
//!
//! Without the RemoteDesktop portal (e.g. wlroots) only ScreenCast is tried; input then
//! falls back to enigo. Linux only.

use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ashpd::desktop::{
    remote_desktop::{Axis, DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions},
    screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
    PersistMode,
};
use pipewire as pw;
use pw::{properties::properties, spa};
use remote_friend_common::{InputEvent, MouseButton};

use crate::convert::{PixFmt, RawFrame};

/// Latest frame + generation counter. Multiple consumers (H.264, JPEG) share the same frame.
struct Latest {
    gen: u64,
    frame: Option<Arc<RawFrame>>,
}
static LATEST: Mutex<Latest> = Mutex::new(Latest { gen: 0, frame: None });
static NEW_FRAME: Condvar = Condvar::new();

/// Are PipeWire frames flowing?
static READY: AtomicBool = AtomicBool::new(false);
/// Is a portal round in progress? (the permission dialog may be open)
static STARTING: AtomicBool = AtomicBool::new(false);
/// Last start attempt (ms since process start); failed attempts are retried every 30 s.
static LAST_ATTEMPT_MS: AtomicU64 = AtomicU64::new(0);
static EVER_TRIED: AtomicBool = AtomicBool::new(false);
/// Is portal input ready? (RemoteDesktop session open)
static INPUT_READY: AtomicBool = AtomicBool::new(false);
static INPUT_TX: Mutex<Option<tokio::sync::mpsc::Sender<InputEvent>>> = Mutex::new(None);
/// Logical size of the portal stream (mouse coordinates are given in this space).
static LOGICAL_W: AtomicI64 = AtomicI64::new(0);
static LOGICAL_H: AtomicI64 = AtomicI64::new(0);
/// Last PipeWire frame size (physical pixels).
static FRAME_W: AtomicU32 = AtomicU32::new(0);
static FRAME_H: AtomicU32 = AtomicU32::new(0);

/// The pointer is drawn into the picture (only when the desktop can't leave it out).
static CURSOR_EMBEDDED: AtomicBool = AtomicBool::new(true);

pub fn cursor_embedded() -> bool {
    CURSOR_EMBEDDED.load(Ordering::Relaxed)
}

/// Leave the pointer out of the picture when the desktop allows it: viewers show their own,
/// immediate pointer instead of a delayed copy (and never two pointers).
async fn cursor_mode(sc: &Screencast) -> CursorMode {
    let modes = sc.available_cursor_modes().await.unwrap_or_default();
    let mode = if modes.contains(CursorMode::Hidden) { CursorMode::Hidden } else { CursorMode::Embedded };
    CURSOR_EMBEDDED.store(mode == CursorMode::Embedded, Ordering::Relaxed);
    mode
}

/// Screens granted by the portal (several if the user selected more than one): PipeWire
/// node id and logical size. The viewers see one of them; switching needs no new dialog.
static SCREENS: Mutex<Vec<(u32, (i32, i32))>> = Mutex::new(Vec::new());
static ACTIVE_NODE: AtomicU32 = AtomicU32::new(0);
/// Bumped when switching screens: the PipeWire thread of the previous screen stops.
static PW_GEN: AtomicU64 = AtomicU64::new(0);
/// The portal's PipeWire connection (each stream thread gets a duplicate).
static PW_FD: Mutex<Option<OwnedFd>> = Mutex::new(None);

/// Logical sizes of the granted screens.
pub fn screens() -> Vec<(u32, u32)> {
    SCREENS
        .lock()
        .map(|l| l.iter().map(|(_, (w, h))| (*w.max(&0) as u32, *h.max(&0) as u32)).collect())
        .unwrap_or_default()
}

/// Index of the screen being shared.
pub fn active_screen() -> usize {
    let node = ACTIVE_NODE.load(Ordering::Relaxed);
    SCREENS.lock().ok().and_then(|l| l.iter().position(|(n, _)| *n == node)).unwrap_or(0)
}

/// Share another granted screen.
pub fn select_screen(index: usize) -> bool {
    let Some((node, (w, h))) = SCREENS.lock().ok().and_then(|l| l.get(index).copied()) else { return false };
    if node == ACTIVE_NODE.load(Ordering::Relaxed) {
        return true;
    }
    let Some(fd) = PW_FD.lock().ok().and_then(|g| g.as_ref().and_then(|f| f.try_clone().ok())) else { return false };
    ACTIVE_NODE.store(node, Ordering::Relaxed);
    LOGICAL_W.store(w as i64, Ordering::Relaxed);
    LOGICAL_H.store(h as i64, Ordering::Relaxed);
    PW_GEN.fetch_add(1, Ordering::Relaxed);
    if let Err(e) = spawn_pw_thread(node, fd) {
        tracing::warn!("cannot switch screens: {e:#}");
        return false;
    }
    true
}

/// Remember the granted screens and start sharing the first one.
fn start_streams(streams: &[ashpd::desktop::screencast::Stream], fd: OwnedFd) -> anyhow::Result<u32> {
    let list: Vec<(u32, (i32, i32))> =
        streams.iter().map(|s| (s.pipe_wire_node_id(), s.size().unwrap_or((0, 0)))).collect();
    let (node, (w, h)) = *list.first().ok_or_else(|| anyhow::anyhow!("portal returned no screen stream (nothing selected?)"))?;
    if list.len() > 1 {
        tracing::info!("portal granted {} screens", list.len());
    }
    *SCREENS.lock().unwrap() = list;
    ACTIVE_NODE.store(node, Ordering::Relaxed);
    if w > 0 && h > 0 {
        LOGICAL_W.store(w as i64, Ordering::Relaxed);
        LOGICAL_H.store(h as i64, Ordering::Relaxed);
    }
    let first = fd.try_clone()?;
    *PW_FD.lock().unwrap() = Some(fd);
    spawn_pw_thread(node, first)?;
    Ok(node)
}

/// Portal tasks run on tokio; calls may also come from the video thread.
static RUNTIME: std::sync::OnceLock<tokio::runtime::Handle> = std::sync::OnceLock::new();

/// Call once from main().
pub fn init_runtime(handle: tokio::runtime::Handle) {
    let _ = RUNTIME.set(handle);
}

fn process_start() -> Instant {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *T0.get_or_init(Instant::now)
}

/// Is this Wayland? (On Xorg/Windows/macOS, xcap + enigo are enough.)
pub fn is_wayland() -> bool {
    let t = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let d = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    t.eq_ignore_ascii_case("wayland") || !d.is_empty()
}

/// Start the portal session (from a tokio runtime). Does nothing if it is already open or opening.
/// After a failure it retries no sooner than 30 s later (when a new viewer connects).
pub fn ensure_started() {
    // The test pattern needs no screen: never open the desktop's sharing dialog for it.
    let test_pattern = std::env::var("RF_TEST_PATTERN").is_ok_and(|v| v == "1");
    if !is_wayland() || test_pattern || READY.load(Ordering::Relaxed) {
        return;
    }
    let now = process_start().elapsed().as_millis() as u64;
    if EVER_TRIED.load(Ordering::Relaxed)
        && now.saturating_sub(LAST_ATTEMPT_MS.load(Ordering::Relaxed)) < 30_000
    {
        return;
    }
    let Some(rt) = tokio::runtime::Handle::try_current().ok().or_else(|| RUNTIME.get().cloned()) else {
        tracing::warn!("wayland: no tokio runtime; cannot start the portal");
        return;
    };
    if STARTING.swap(true, Ordering::AcqRel) {
        return;
    }
    EVER_TRIED.store(true, Ordering::Relaxed);
    LAST_ATTEMPT_MS.store(now, Ordering::Relaxed);
    tracing::info!("wayland: opening portal session (the desktop asks for permission the first time)");
    crate::status::update(|s| s.screen = crate::status::ScreenState::WaitingPermission);
    crate::status::notice(
        "Screen sharing: if your desktop asks for permission, turn ON 'Allow Remote Interaction' and click Share (asked once).",
    );
    rt.spawn(async move {
        let result = portal_task().await;
        STARTING.store(false, Ordering::Release);
        if let Err(e) = result {
            tracing::warn!("wayland portal failed: {e:#}");
            crate::status::update(|s| s.screen = crate::status::ScreenState::Denied);
            crate::status::notice(format!("Screen sharing permission was not granted: {e:#}"));
        }
    });
}

/// Ask again right away (desktop app button), ignoring the retry back-off.
/// Returns false when a session is still running (e.g. shared without remote control):
/// the saved permission is discarded and the app must restart to ask again.
pub fn retry() -> bool {
    let _ = std::fs::remove_file(token_path("rd_restore_token"));
    if PW_ALIVE.load(Ordering::Relaxed) {
        return false;
    }
    EVER_TRIED.store(false, Ordering::Relaxed);
    ensure_started();
    true
}

/// True while the portal is still in its permission round or the first frame is pending.
pub fn portal_pending() -> bool {
    is_wayland()
        && !READY.load(Ordering::Relaxed)
        && (STARTING.load(Ordering::Relaxed) || PW_ALIVE.load(Ordering::Relaxed))
}

/// Is the PipeWire stream ready?
pub fn is_ready() -> bool {
    READY.load(Ordering::Relaxed)
}

/// Waits at most `timeout` for a frame newer than `last_gen`.
pub fn wait_frame(last_gen: &mut u64, timeout: Duration) -> Option<Arc<RawFrame>> {
    let deadline = Instant::now() + timeout;
    let mut g = LATEST.lock().ok()?;
    loop {
        if g.gen != *last_gen {
            if let Some(f) = g.frame.clone() {
                *last_gen = g.gen;
                return Some(f);
            }
        }
        let now = Instant::now();
        // Sleep until the deadline even while waiting for permission/the first frame (so an idle loop doesn't burn CPU).
        if now >= deadline {
            return None;
        }
        g = NEW_FRAME.wait_timeout(g, deadline - now).ok()?.0;
    }
}

/// Is portal input available?
pub fn input_ready() -> bool {
    INPUT_READY.load(Ordering::Relaxed)
}

/// Forward input to the portal task. Mouse moves are dropped when the queue is full.
pub fn send_input(ev: InputEvent) -> bool {
    let tx = INPUT_TX.lock().ok().and_then(|g| g.clone());
    match tx {
        Some(tx) => match tx.try_send(ev) {
            Ok(()) => true,
            Err(tokio::sync::mpsc::error::TrySendError::Full(InputEvent::MouseMove { .. })) => true,
            Err(_) => false,
        },
        None => false,
    }
}

fn token_path(name: &str) -> std::path::PathBuf {
    remote_friend_common::identity::config_dir().join(name)
}

fn load_token(name: &str) -> Option<String> {
    std::fs::read_to_string(token_path(name))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn save_token(name: &str, t: &str) {
    if let Err(e) = remote_friend_common::identity::write_private(&token_path(name), t.as_bytes()) {
        tracing::warn!("failed to write portal restore token: {e}");
    }
}

async fn portal_task() -> anyhow::Result<()> {
    if std::env::var("RF_NO_REMOTE_DESKTOP").map(|v| v == "1").unwrap_or(false) {
        return screencast_only().await;
    }
    match remote_desktop_session().await {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::warn!("RemoteDesktop portal unavailable ({e:#}); trying screen sharing only");
            screencast_only().await
        }
    }
}

/// Video + input in a single session. The input queue is processed for the lifetime of the session.
async fn remote_desktop_session() -> anyhow::Result<()> {
    let rd = RemoteDesktop::new().await?;
    let sc = Screencast::new().await?;
    let session = rd.create_session(Default::default()).await?;
    let token = load_token("rd_restore_token");
    rd.select_devices(
        &session,
        SelectDevicesOptions::default()
            .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(token.as_deref()),
    )
    .await?;
    let cursor = cursor_mode(&sc).await;
    sc.select_sources(
        &session,
        SelectSourcesOptions::default()
            .set_cursor_mode(cursor)
            .set_sources(ashpd::enumflags2::BitFlags::from(SourceType::Monitor))
            // Several screens may be picked; the app switches between them without asking again.
            .set_multiple(true),
    )
    .await?;
    let response = rd.start(&session, None, Default::default()).await?.response()?;
    let devices = response.devices();
    let mut input_ok = devices.contains(DeviceType::Pointer) || devices.contains(DeviceType::Keyboard);
    if !input_ok {
        // Some desktops don't list the granted devices in the response. A (0,0) relative motion
        // doesn't move the cursor; if the portal accepts it, control is permitted.
        input_ok = rd.notify_pointer_motion(&session, 0.0, 0.0, Default::default()).await.is_ok();
        tracing::info!("portal device list empty; input probe: {}", if input_ok { "allowed" } else { "not allowed" });
    }
    match response.restore_token() {
        // Don't keep the token if input was not allowed, so the next start asks again.
        Some(t) if input_ok => save_token("rd_restore_token", t),
        _ => {
            let _ = std::fs::remove_file(token_path("rd_restore_token"));
        }
    }
    let fd: OwnedFd = sc.open_pipe_wire_remote(&session, Default::default()).await?;
    let node_id = start_streams(response.streams(), fd)?;
    tracing::info!(
        "portal permission OK (remote control + screen, node {node_id}, {} screen(s), devices {:?})",
        response.streams().len(),
        response.devices()
    );
    if !input_ok {
        crate::status::update(|s| s.screen = crate::status::ScreenState::NoInput);
        crate::status::notice(
            "Screen is shared but REMOTE CONTROL was not allowed (mouse/keyboard won't work). Click 'Ask again' and turn ON 'Allow Remote Interaction'.",
        );
        tracing::warn!("RemoteDesktop: no input device permission ({devices:?})");
        // The session must still stay alive for video.
        tokio::spawn(async move {
            while PW_ALIVE.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            let _ = session.close().await;
            drop(rd);
        });
        return Ok(());
    }
    crate::status::update(|s| s.screen = crate::status::ScreenState::Ready);
    crate::status::notice("Screen sharing and remote control are ready.");

    let (tx, rx) = tokio::sync::mpsc::channel::<InputEvent>(4096);
    *INPUT_TX.lock().unwrap() = Some(tx);
    INPUT_READY.store(true, Ordering::Release);
    // The session objects live in the input task; the task ends when the stream ends.
    tokio::spawn(input_loop(rd, session, rx));
    Ok(())
}

async fn input_loop(
    rd: RemoteDesktop,
    session: ashpd::desktop::Session<RemoteDesktop>,
    mut rx: tokio::sync::mpsc::Receiver<InputEvent>,
) {
    let mut errors = 0u32;
    let mut alive = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            ev = rx.recv() => {
                let Some(ev) = ev else { break };
                // Absolute pointer positions refer to the screen being shared.
                let node_id = ACTIVE_NODE.load(Ordering::Relaxed);
                if let Err(e) = inject(&rd, &session, node_id, ev).await {
                    errors += 1;
                    if errors <= 3 || errors % 100 == 0 {
                        tracing::warn!("portal input error ({errors}): {e}");
                    }
                }
            }
            _ = alive.tick() => {
                if !PW_ALIVE.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }
    INPUT_READY.store(false, Ordering::Release);
    *INPUT_TX.lock().unwrap() = None;
    let _ = session.close().await;
}

/// Legacy path: screen sharing only (input is left to enigo).
async fn screencast_only() -> anyhow::Result<()> {
    let proxy = Screencast::new().await?;
    let session = proxy.create_session(Default::default()).await?;
    let saved = load_token("pw_restore_token");
    let cursor = cursor_mode(&proxy).await;
    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(cursor)
                .set_sources(ashpd::enumflags2::BitFlags::from(SourceType::Monitor))
                .set_multiple(true)
                .set_restore_token(saved.as_deref())
                .set_persist_mode(PersistMode::ExplicitlyRevoked),
        )
        .await?;
    let response = proxy.start(&session, None, Default::default()).await?.response()?;
    if let Some(t) = response.restore_token() {
        save_token("pw_restore_token", t);
    }
    let fd: OwnedFd = proxy.open_pipe_wire_remote(&session, Default::default()).await?;
    tracing::warn!("screen sharing only: mouse/keyboard may not reach non-XWayland windows");
    start_streams(response.streams(), fd)?;
    // If the session object is dropped the portal closes the stream: keep it alive until the stream ends.
    tokio::spawn(async move {
        while PW_ALIVE.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let _ = session.close().await;
    });
    Ok(())
}

/// Is the PipeWire thread running? (session objects are kept alive while it is)
static PW_ALIVE: AtomicBool = AtomicBool::new(false);

fn spawn_pw_thread(node_id: u32, fd: OwnedFd) -> anyhow::Result<()> {
    PW_ALIVE.store(true, Ordering::Relaxed);
    let gen = PW_GEN.load(Ordering::Relaxed);
    std::thread::Builder::new()
        .name("rf-pipewire".into())
        .spawn(move || {
            if let Err(e) = pw_thread(node_id, fd, gen) {
                tracing::warn!("pipewire thread exited: {e:#}");
            }
            if PW_GEN.load(Ordering::Relaxed) != gen {
                return; // replaced by another screen: the session goes on
            }
            *PW_FD.lock().unwrap_or_else(|e| e.into_inner()) = None;
            SCREENS.lock().unwrap_or_else(|e| e.into_inner()).clear();
            PW_ALIVE.store(false, Ordering::Relaxed);
            READY.store(false, Ordering::Relaxed);
            INPUT_READY.store(false, Ordering::Relaxed);
            NEW_FRAME.notify_all();
            crate::status::update(|s| s.screen = crate::status::ScreenState::Denied);
            crate::status::notice("Screen sharing stopped; it will be requested again on the next connection.");
        })?;
    Ok(())
}

/// Linux evdev button codes.
fn evdev_button(b: MouseButton) -> i32 {
    match b {
        MouseButton::Left => 0x110,
        MouseButton::Right => 0x111,
        MouseButton::Middle => 0x112,
    }
}

async fn inject(
    rd: &RemoteDesktop,
    session: &ashpd::desktop::Session<RemoteDesktop>,
    node: u32,
    ev: InputEvent,
) -> ashpd::Result<()> {
    match ev {
        InputEvent::MouseMove { x, y } => {
            // x,y are in capture (physical pixel) space; the portal expects logical coordinates.
            let (fw, fh) = (FRAME_W.load(Ordering::Relaxed), FRAME_H.load(Ordering::Relaxed));
            let (lw, lh) = (LOGICAL_W.load(Ordering::Relaxed), LOGICAL_H.load(Ordering::Relaxed));
            let (mut lx, mut ly) = (x as f64, y as f64);
            if fw > 0 && fh > 0 && lw > 0 && lh > 0 {
                lx = lx * lw as f64 / fw as f64;
                ly = ly * lh as f64 / fh as f64;
            }
            rd.notify_pointer_motion_absolute(session, node, lx, ly, Default::default())
                .await
        }
        InputEvent::MouseDown { button } => {
            rd.notify_pointer_button(session, evdev_button(button), KeyState::Pressed, Default::default())
                .await
        }
        InputEvent::MouseUp { button } => {
            rd.notify_pointer_button(session, evdev_button(button), KeyState::Released, Default::default())
                .await
        }
        InputEvent::Scroll { dx, dy } => {
            if dy != 0 {
                rd.notify_pointer_axis_discrete(session, Axis::Vertical, dy.clamp(-20, 20), Default::default())
                    .await?;
            }
            if dx != 0 {
                rd.notify_pointer_axis_discrete(session, Axis::Horizontal, dx.clamp(-20, 20), Default::default())
                    .await?;
            }
            Ok(())
        }
        InputEvent::Key { key, down } => {
            let sym = crate::input::keysym(key) as i32;
            let state = if down { KeyState::Pressed } else { KeyState::Released };
            rd.notify_keyboard_keysym(session, sym, state, Default::default()).await
        }
        InputEvent::Text(text) => {
            for c in text.chars().take(4096) {
                let sym = match c {
                    '\n' | '\r' => 0xff0d,
                    '\t' => 0xff09,
                    c if c.is_control() => continue,
                    c => crate::input::char_keysym(c),
                } as i32;
                rd.notify_keyboard_keysym(session, sym, KeyState::Pressed, Default::default()).await?;
                rd.notify_keyboard_keysym(session, sym, KeyState::Released, Default::default()).await?;
            }
            Ok(())
        }
    }
}

struct Fmt {
    fmt: PixFmt,
    w: u32,
    h: u32,
}
static FORMAT: Mutex<Option<Fmt>> = Mutex::new(None);

fn publish(frame: RawFrame) {
    FRAME_W.store(frame.w, Ordering::Relaxed);
    FRAME_H.store(frame.h, Ordering::Relaxed);
    let mut g = LATEST.lock().unwrap();
    g.gen = g.gen.wrapping_add(1);
    g.frame = Some(Arc::new(frame));
    drop(g);
    NEW_FRAME.notify_all();
    if !READY.swap(true, Ordering::Relaxed) {
        tracing::info!("first pipewire frame received: silent capture active");
    }
}

fn pw_thread(node_id: u32, fd: OwnedFd, gen: u64) -> anyhow::Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect_fd(fd, None)?;

    let stream = pw::stream::StreamBox::new(
        &core,
        "remotefriend-screen",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let weak_loop = mainloop.downgrade();
    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |_, _, old, new| {
            tracing::info!("pipewire stream state: {old:?} -> {new:?}");
            if matches!(new, pw::stream::StreamState::Error(_) | pw::stream::StreamState::Unconnected) {
                READY.store(false, Ordering::Relaxed);
                if let Some(ml) = weak_loop.upgrade() {
                    ml.quit();
                }
            }
        })
        .param_changed(|_, _, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) = spa::param::format_utils::parse_format(param) else {
                return;
            };
            if media_type != spa::param::format::MediaType::Video
                || media_subtype != spa::param::format::MediaSubtype::Raw
            {
                return;
            }
            let mut info = spa::param::video::VideoInfoRaw::default();
            if info.parse(param).is_err() {
                return;
            }
            use spa::param::video::VideoFormat as V;
            let size = info.size();
            let fmt = match info.format() {
                V::BGRx | V::BGRA => PixFmt::Bgrx,
                V::RGBx | V::RGBA => PixFmt::Rgbx,
                other => {
                    tracing::warn!("unsupported pipewire format: {other:?}");
                    return;
                }
            };
            tracing::info!("pipewire format: {:?} {}x{} @ {:?}", info.format(), size.width, size.height, info.framerate());
            *FORMAT.lock().unwrap() = Some(Fmt { fmt, w: size.width, h: size.height });
        })
        .process(move |stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            if PW_GEN.load(Ordering::Relaxed) != gen {
                return; // another screen is being shared now
            }
            let Some((fmt, w, h)) = FORMAT.lock().unwrap().as_ref().map(|f| (f.fmt, f.w, f.h)) else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() || w == 0 || h == 0 || w > 8192 || h > 8192 {
                return;
            }
            let data = &mut datas[0];
            let chunk_size = data.chunk().size() as usize;
            let stride = match data.chunk().stride() {
                s if s > 0 => s as usize,
                _ => w as usize * 4,
            };
            let offset = data.chunk().offset() as usize;
            let need = stride * (h as usize - 1) + w as usize * 4;
            if chunk_size == 0 || stride < w as usize * 4 {
                return; // empty (metadata-only) buffer
            }
            let copy = |mem: &[u8]| -> Option<Vec<u8>> {
                let end = offset.checked_add(need)?;
                mem.get(offset..end).map(|s| s.to_vec())
            };
            let is_memfd = data.type_() == spa::buffer::DataType::MemFd;
            let raw_fd = data.fd();
            let bytes = match data.data() {
                Some(mem) => copy(mem),
                None if is_memfd => read_memfd(raw_fd).and_then(|m| copy(&m)),
                None => None,
            };
            if let Some(bytes) = bytes {
                publish(RawFrame { w, h, stride, fmt, data: bytes });
            }
        })
        .register()?;

    // 32-bit packed formats; 30 fps preferred, up to 60 allowed (mutter expects 0..max).
    let obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::RGBx,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::RGBA,
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle { width: 1920, height: 1080 },
            spa::utils::Rectangle { width: 1, height: 1 },
            spa::utils::Rectangle { width: 8192, height: 8192 }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 30, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 60, denom: 1 }
        ),
    );
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow::anyhow!("pod serialize: {e}"))?
    .0
    .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&values).ok_or_else(|| anyhow::anyhow!("pod parse"))?];

    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    if let Err(e) = stream.set_active(true) {
        tracing::warn!("pipewire activate failed: {e:#}");
    }
    tracing::info!("pipewire stream connected");
    // Stop when another screen gets shared.
    let weak = mainloop.downgrade();
    let switch_timer = mainloop.loop_().add_timer(move |_| {
        if PW_GEN.load(Ordering::Relaxed) != gen {
            if let Some(ml) = weak.upgrade() {
                ml.quit();
            }
        }
    });
    let _ = switch_timer.update_timer(Some(Duration::from_millis(200)), Some(Duration::from_millis(200)));
    mainloop.run();
    Ok(())
}

/// Map the MemFd read-only (if MAP_BUFFERS didn't map it). The fd belongs to the pool, so it is dup'ed.
fn read_memfd(fd: std::os::fd::RawFd) -> Option<memmap2::Mmap> {
    use std::os::fd::FromRawFd;
    if fd < 0 {
        return None;
    }
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        return None;
    }
    let owned: OwnedFd = unsafe { OwnedFd::from_raw_fd(dup) };
    let file = std::fs::File::from(owned);
    let len = file.metadata().ok()?.len() as usize;
    if len == 0 {
        return None;
    }
    unsafe { memmap2::MmapOptions::new().len(len.min(256 * 1024 * 1024)).map(&file).ok() }
}
