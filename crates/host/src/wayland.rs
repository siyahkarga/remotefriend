//! Wayland (GNOME/KDE): Portal RemoteDesktop + ScreenCast + PipeWire.
//!
//! Neden var: Wayland'da uygulamalar ekranı doğrudan okuyamaz ve başka pencerelere
//! fare/klavye olayı basamaz. `xcap` her kare için ekran görüntüsü API'sini çağırır
//! (deklanşör sesi), `enigo` X11 olaylarını yalnızca XWayland pencerelerine iletir:
//! gerçek masaüstü kontrol edilemez. Burada masaüstüne BİR kez sorulur
//! ("uzaktan kontrol + ekran paylaşımı"), izin kalıcı belirteçle hatırlanır;
//! görüntü PipeWire'dan sessizce akar, girdi portal üzerinden gerçek masaüstüne gider.
//!
//! RemoteDesktop portalı yoksa (ör. wlroots) yalnızca ScreenCast denenir; girdi o
//! durumda enigo'ya düşer. Sadece Linux.

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

/// En yeni kare + nesil sayacı. Birden fazla tüketici (H.264, JPEG) aynı kareyi paylaşır.
struct Latest {
    gen: u64,
    frame: Option<Arc<RawFrame>>,
}
static LATEST: Mutex<Latest> = Mutex::new(Latest { gen: 0, frame: None });
static NEW_FRAME: Condvar = Condvar::new();

/// PipeWire kare akıyor mu?
static READY: AtomicBool = AtomicBool::new(false);
/// Portal turu sürüyor mu? (izin penceresi açık olabilir)
static STARTING: AtomicBool = AtomicBool::new(false);
/// Son başlatma denemesi (ms, süreç başından); başarısız denemeler 30 sn'de bir tekrarlanır.
static LAST_ATTEMPT_MS: AtomicU64 = AtomicU64::new(0);
static EVER_TRIED: AtomicBool = AtomicBool::new(false);
/// Portal girdisi hazır mı? (RemoteDesktop oturumu açık)
static INPUT_READY: AtomicBool = AtomicBool::new(false);
static INPUT_TX: Mutex<Option<tokio::sync::mpsc::Sender<InputEvent>>> = Mutex::new(None);
/// Portal akışının mantıksal boyutu (fare koordinatları bu uzayda verilir).
static LOGICAL_W: AtomicI64 = AtomicI64::new(0);
static LOGICAL_H: AtomicI64 = AtomicI64::new(0);
/// Son PipeWire kare boyutu (fiziksel piksel).
static FRAME_W: AtomicU32 = AtomicU32::new(0);
static FRAME_H: AtomicU32 = AtomicU32::new(0);

/// Portal görevleri tokio'da çalışır; çağrı video thread'inden de gelebilir.
static RUNTIME: std::sync::OnceLock<tokio::runtime::Handle> = std::sync::OnceLock::new();

/// main() içinden bir kez çağır.
pub fn init_runtime(handle: tokio::runtime::Handle) {
    let _ = RUNTIME.set(handle);
}

fn process_start() -> Instant {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *T0.get_or_init(Instant::now)
}

/// Wayland mı? (Xorg/Windows/macOS'ta xcap + enigo yeterli.)
pub fn is_wayland() -> bool {
    let t = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let d = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    t.eq_ignore_ascii_case("wayland") || !d.is_empty()
}

/// Portal oturumunu başlat (tokio runtime içinden). Zaten açıksa/açılıyorsa bir şey yapmaz.
/// Başarısız olduysa en erken 30 sn sonra tekrar dener (yeni izleyici bağlandığında).
pub fn ensure_started() {
    if !is_wayland() || READY.load(Ordering::Relaxed) {
        return;
    }
    let now = process_start().elapsed().as_millis() as u64;
    if EVER_TRIED.load(Ordering::Relaxed)
        && now.saturating_sub(LAST_ATTEMPT_MS.load(Ordering::Relaxed)) < 30_000
    {
        return;
    }
    let Some(rt) = tokio::runtime::Handle::try_current().ok().or_else(|| RUNTIME.get().cloned()) else {
        tracing::warn!("wayland: tokio çalışma zamanı yok; portal başlatılamadı");
        return;
    };
    if STARTING.swap(true, Ordering::AcqRel) {
        return;
    }
    EVER_TRIED.store(true, Ordering::Relaxed);
    LAST_ATTEMPT_MS.store(now, Ordering::Relaxed);
    tracing::info!("wayland: portal oturumu açılıyor (ilk seferde masaüstü izin sorar)");
    println!(">>> Wayland: masaüstünde 'Uzak masaüstü / ekran paylaşımı' izni çıkarsa ONAYLA (bir kez sorulur).");
    rt.spawn(async move {
        let result = portal_task().await;
        STARTING.store(false, Ordering::Release);
        if let Err(e) = result {
            tracing::warn!("wayland portal açılamadı: {e:#}");
            println!("!!! Wayland ekran/kontrol izni alınamadı: {e:#}");
        }
    });
}

/// Portal hâlâ izin turundaysa ya da ilk kare bekleniyorsa true.
pub fn portal_pending() -> bool {
    is_wayland()
        && !READY.load(Ordering::Relaxed)
        && (STARTING.load(Ordering::Relaxed) || PW_ALIVE.load(Ordering::Relaxed))
}

/// PipeWire akışı hazır mı?
pub fn is_ready() -> bool {
    READY.load(Ordering::Relaxed)
}

/// `last_gen`'den yeni bir kare gelene kadar en çok `timeout` bekler.
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
        // İzin/ilk kare beklenirken de süre dolana kadar uyu (boş döngü CPU yakmasın).
        if now >= deadline {
            return None;
        }
        g = NEW_FRAME.wait_timeout(g, deadline - now).ok()?.0;
    }
}

/// Portal girdisi kullanılabilir mi?
pub fn input_ready() -> bool {
    INPUT_READY.load(Ordering::Relaxed)
}

/// Girdiyi portal görevine ilet. Kuyruk doluysa fare hareketi düşürülür.
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
        tracing::warn!("portal restore token yazılamadı: {e}");
    }
}

async fn portal_task() -> anyhow::Result<()> {
    if std::env::var("RF_NO_REMOTE_DESKTOP").map(|v| v == "1").unwrap_or(false) {
        return screencast_only().await;
    }
    match remote_desktop_session().await {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::warn!("RemoteDesktop portalı kullanılamadı ({e:#}); yalnız ekran paylaşımı deneniyor");
            screencast_only().await
        }
    }
}

/// Görüntü + girdi tek oturumda. Fonksiyon oturum boyunca girdi kuyruğunu işler.
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
    sc.select_sources(
        &session,
        SelectSourcesOptions::default()
            .set_cursor_mode(CursorMode::Embedded)
            .set_sources(ashpd::enumflags2::BitFlags::from(SourceType::Monitor))
            .set_multiple(false),
    )
    .await?;
    let response = rd.start(&session, None, Default::default()).await?.response()?;
    let devices = response.devices();
    let mut input_ok = devices.contains(DeviceType::Pointer) || devices.contains(DeviceType::Keyboard);
    if !input_ok {
        // Bazı masaüstleri verilen cihazları yanıtta bildirmez. (0,0) göreli hareket
        // imleci kıpırdatmaz; portal kabul ederse kontrol izni vardır.
        input_ok = rd.notify_pointer_motion(&session, 0.0, 0.0, Default::default()).await.is_ok();
        tracing::info!("portal cihaz listesi boş; girdi yoklaması: {}", if input_ok { "izin var" } else { "izin yok" });
    }
    match response.restore_token() {
        // Girdi izni verilmediyse belirteci saklama: bir sonraki açılışta yeniden sorulsun.
        Some(t) if input_ok => save_token("rd_restore_token", t),
        _ => {
            let _ = std::fs::remove_file(token_path("rd_restore_token"));
        }
    }
    let stream = response
        .streams()
        .first()
        .ok_or_else(|| anyhow::anyhow!("portal ekran akışı vermedi (seçim yapılmadı?)"))?
        .to_owned();
    let node_id = stream.pipe_wire_node_id();
    if let Some((w, h)) = stream.size() {
        LOGICAL_W.store(w as i64, Ordering::Relaxed);
        LOGICAL_H.store(h as i64, Ordering::Relaxed);
    }
    let fd: OwnedFd = sc.open_pipe_wire_remote(&session, Default::default()).await?;
    tracing::info!(
        "portal izni OK (uzaktan kontrol + ekran, node {node_id}, mantıksal {:?}, cihazlar {:?})",
        stream.size(),
        response.devices()
    );
    spawn_pw_thread(node_id, fd)?;
    if !input_ok {
        println!("!!! Wayland: ekran paylaşıldı ama UZAKTAN KONTROL izni verilmedi (fare/klavye çalışmaz).");
        println!("!!! Host'u yeniden başlat; izin penceresinde 'Uzaktan etkileşime izin ver' seçeneğini AÇIK bırak.");
        tracing::warn!("RemoteDesktop: girdi cihazı izni yok ({devices:?})");
        // Oturum yine de görüntü için canlı kalmalı.
        tokio::spawn(async move {
            while PW_ALIVE.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            let _ = session.close().await;
            drop(rd);
        });
        return Ok(());
    }
    println!(">>> Wayland izni alındı: ekran + fare/klavye kontrolü hazır.");

    let (tx, rx) = tokio::sync::mpsc::channel::<InputEvent>(4096);
    *INPUT_TX.lock().unwrap() = Some(tx);
    INPUT_READY.store(true, Ordering::Release);
    // Oturum nesneleri girdi görevinde yaşar; akış bitince görev de biter.
    tokio::spawn(input_loop(rd, session, node_id, rx));
    Ok(())
}

async fn input_loop(
    rd: RemoteDesktop,
    session: ashpd::desktop::Session<RemoteDesktop>,
    node_id: u32,
    mut rx: tokio::sync::mpsc::Receiver<InputEvent>,
) {
    let mut errors = 0u32;
    let mut alive = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            ev = rx.recv() => {
                let Some(ev) = ev else { break };
                if let Err(e) = inject(&rd, &session, node_id, ev).await {
                    errors += 1;
                    if errors <= 3 || errors % 100 == 0 {
                        tracing::warn!("portal girdi hatası ({errors}): {e}");
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

/// Eski yol: sadece ekran paylaşımı (girdi enigo'ya kalır).
async fn screencast_only() -> anyhow::Result<()> {
    let proxy = Screencast::new().await?;
    let session = proxy.create_session(Default::default()).await?;
    let saved = load_token("pw_restore_token");
    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Embedded)
                .set_sources(ashpd::enumflags2::BitFlags::from(SourceType::Monitor))
                .set_multiple(false)
                .set_restore_token(saved.as_deref())
                .set_persist_mode(PersistMode::ExplicitlyRevoked),
        )
        .await?;
    let response = proxy.start(&session, None, Default::default()).await?.response()?;
    if let Some(t) = response.restore_token() {
        save_token("pw_restore_token", t);
    }
    let stream = response
        .streams()
        .first()
        .ok_or_else(|| anyhow::anyhow!("portal stream vermedi (seçim yapılmadı?)"))?
        .to_owned();
    let node_id = stream.pipe_wire_node_id();
    let fd: OwnedFd = proxy.open_pipe_wire_remote(&session, Default::default()).await?;
    tracing::warn!("yalnız ekran paylaşımı açık: fare/klavye XWayland dışındaki pencerelere ulaşmayabilir");
    spawn_pw_thread(node_id, fd)?;
    // Oturum nesnesi düşerse portal akışı kapatır: akış bitene kadar canlı tut.
    tokio::spawn(async move {
        while PW_ALIVE.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let _ = session.close().await;
    });
    Ok(())
}

/// PipeWire thread'i çalışıyor mu? (oturum nesnelerini o sürece canlı tutar)
static PW_ALIVE: AtomicBool = AtomicBool::new(false);

fn spawn_pw_thread(node_id: u32, fd: OwnedFd) -> anyhow::Result<()> {
    PW_ALIVE.store(true, Ordering::Relaxed);
    std::thread::Builder::new()
        .name("rf-pipewire".into())
        .spawn(move || {
            if let Err(e) = pw_thread(node_id, fd) {
                tracing::warn!("pipewire thread kapandı: {e:#}");
            }
            PW_ALIVE.store(false, Ordering::Relaxed);
            READY.store(false, Ordering::Relaxed);
            INPUT_READY.store(false, Ordering::Relaxed);
            NEW_FRAME.notify_all();
            tracing::warn!("ekran paylaşımı sona erdi; yeni izleyici bağlanınca tekrar istenecek");
        })?;
    Ok(())
}

/// Linux evdev düğme kodları.
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
            // x,y yakalama (fiziksel piksel) uzayında; portal mantıksal koordinat ister.
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
        tracing::info!("ilk pipewire karesi alındı: sessiz yakalama aktif");
    }
}

fn pw_thread(node_id: u32, fd: OwnedFd) -> anyhow::Result<()> {
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
            tracing::info!("pipewire stream durumu: {old:?} -> {new:?}");
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
                    tracing::warn!("desteklenmeyen pipewire formatı: {other:?}");
                    return;
                }
            };
            tracing::info!("pipewire format: {:?} {}x{} @ {:?}", info.format(), size.width, size.height, info.framerate());
            *FORMAT.lock().unwrap() = Some(Fmt { fmt, w: size.width, h: size.height });
        })
        .process(|stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
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
                return; // boş (yalnız meta) buffer
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

    // 32-bit paket formatlar; 30 fps tercih, 60'a kadar izin (mutter 0..max ister).
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
        tracing::warn!("pipewire activate başarısız: {e:#}");
    }
    tracing::info!("pipewire stream bağlandı");
    mainloop.run();
    Ok(())
}

/// MemFd'yi salt-okunur map'le (MAP_BUFFERS eşlemediyse). fd pool'a ait, dup'lanır.
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
