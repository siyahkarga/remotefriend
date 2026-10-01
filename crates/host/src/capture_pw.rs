//! Wayland sessiz ekran yakalama: Portal ScreenCast + PipeWire.
//!
//! Neden var: `xcap` Wayland'da her kare için `org.gnome.Shell.Screenshot`
//! fotoğraf API'sini çağırır (saniyede ~15 deklanşör sesi + flaş).
//! Bu modül GNOME'a BİR kez sorar (paylaşım izni), sonra PipeWire üzerinden
//! sessizce sürekli kare akıtır. Portal/pipewire yoksa sessizce devre dışı
//! kalır, çağrıcı `xcap` yoluna düşer.
//!
//! Sadece Linux. Diğer platformlarda dosya derlenmez.

use std::os::fd::OwnedFd;
use std::sync::{Mutex, Once};

use ashpd::desktop::{
    PersistMode,
    screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
};
use pipewire as pw;
use pw::{properties::properties, spa};

/// Son PipeWire karesi (RGBA, host encode'una hazır). Yoksa None -> xcap fallback.
static LATEST: Mutex<Option<(u32, u32, Vec<u8>)>> = Mutex::new(None);
/// Portal+PipeWire hattı ayakta mı?
static READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Portal kesin başarısız mı? (izin reddi / portal yok -> xcap fallback serbest.)
static FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static START: Once = Once::new();

/// Wayland mı? (Xorg'da xcap zaten sessiz + hızlı, buna gerek yok.)
fn is_wayland() -> bool {
    let t = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let d = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    t.eq_ignore_ascii_case("wayland") || d.to_lowercase().contains("wayland")
}

/// Host başlangıcında bir kez çağır (tokio runtime içinden). Wayland değilse
/// ya da portal yoksa hiçbir şey yapmaz.
pub fn ensure_started() {
    if !is_wayland() {
        return;
    }
    START.call_once(|| {
        tracing::info!("wayland görüldü: sessiz PipeWire yakalama başlatılıyor (bir kez izin sorar)");
        tokio::spawn(async move {
            if let Err(e) = portal_task().await {
                FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!("pipewire yakalama açılamadı, xcap fallback: {e:#}");
            }
        });
    });
}

/// Çağrıcı (capture_rgba) her karede buraya bakar. Frame hazırsa kullanır.
pub fn try_get_frame() -> Option<(u32, u32, Vec<u8>)> {
    if !READY.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    LATEST.lock().ok()?.clone()
}

/// Portal hâlâ izin turundaysa true: çağrıcı kare atlayıp bekler
/// (xcap'e düşüp deklanşör sesi çıkarmaz). Portal kesin öldüyse false.
pub fn portal_pending() -> bool {
    is_wayland()
        && !READY.load(std::sync::atomic::Ordering::Relaxed)
        && !FAILED.load(std::sync::atomic::Ordering::Relaxed)
}

async fn portal_task() -> anyhow::Result<()> {
    let proxy = Screencast::new().await?;
    let session = proxy.create_session(Default::default()).await?;
    // Kalıcı izin: ilk onaydan sonra bir daha dialog çıkmaz.
    let saved = load_restore_token();
    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Embedded) // uzak taraf imleci görsün
                .set_sources(SourceType::Monitor | SourceType::Window)
                .set_multiple(false)
                .set_restore_token(saved.as_deref())
                .set_persist_mode(PersistMode::Application),
        )
        .await?;
    // GNOME burada İLK seferde "Ekranı paylaş?" diye sorar. Reddedilirse hata -> fallback.
    let response = proxy.start(&session, None, Default::default()).await?.response()?;
    if let Some(t) = response.restore_token() {
        save_restore_token(t);
    }
    let stream = response
        .streams()
        .first()
        .ok_or_else(|| anyhow::anyhow!("portal stream vermedi (seçim yapılmadı?)"))?
        .to_owned();
    let node_id = stream.pipe_wire_node_id();
    let fd: OwnedFd = proxy.open_pipe_wire_remote(&session, Default::default()).await?;
    tracing::info!("portal izni OK (node {node_id}), pipewire akışı açılıyor");
    // PipeWire mainloop bloklar -> ayrı thread.
    std::thread::Builder::new()
        .name("rf-pipewire".into())
        .spawn(move || {
            if let Err(e) = pw_thread(node_id, fd) {
                tracing::warn!("pipewire thread kapandı: {e:#}");
            }
        })?;
    Ok(())
}

struct Fmt {
    format: spa::param::video::VideoFormat,
    w: u32,
    h: u32,
}
static FORMAT: Mutex<Option<Fmt>> = Mutex::new(None);

fn pw_thread(node_id: u32, fd: OwnedFd) -> anyhow::Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopBox::new(None)?;
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

    let _listener = stream
        .add_local_listener_with_user_data(())
        .param_changed(|_, _, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) =
                spa::param::format_utils::parse_format(param)
            else {
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
            let size = info.size();
            let fmt = info.format();
            *FORMAT.lock().unwrap() = Some(Fmt {
                format: fmt,
                w: size.width as u32,
                h: size.height as u32,
            });
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 3 {
                tracing::info!("pipewire format: {fmt:?} {}x{}", size.width, size.height);
            }
        })
        .process(|stream, _| {
            let Some(fmt) = FORMAT.lock().unwrap().as_ref().map(|f| (f.format, f.w, f.h)) else {
                // format henüz bilinmiyor: buffer'ı iade et, bekle
                if let Some(_b) = stream.dequeue_buffer() {}
                return;
            };
            let (vf, w, h) = fmt;
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            // Portal MemFd (paylaşımlı bellek) ya da MemPtr verir. DMA-BUF atlanır.
            static M: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if M.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 5 {
                tracing::info!("pipewire buffer: type={:?} stride={} size={}", data.type_(), data.chunk().stride(), data.chunk().size());
            }
            let is_ptr = data.type_() == spa::buffer::DataType::MemPtr;
            let is_fd = data.type_() == spa::buffer::DataType::MemFd;
            if !is_ptr && !is_fd {
                return;
            }
            let stride = data.chunk().stride();
            let offset = data.chunk().offset() as usize;
            let rgba = if is_ptr {
                match data.data() {
                    Some(mem) => convert_row(w, h, stride, offset, mem, vf),
                    None => None,
                }
            } else {
                // MemFd: fd'yi mmap'le, oradan oku (her karede aç/kapat, ~µs mertebesi).
                let fd = data.fd();
                read_memfd(fd).and_then(|mmap| convert_row(w, h, stride, offset, &mmap, vf))
            };
            if let Some(rgba) = rgba {
                *LATEST.lock().unwrap() = Some((w, h, rgba));
                if !READY.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    tracing::info!("ilk pipewire karesi alındı ({w}x{h}), xcap devreden çıktı (sessiz)");
                }
            }
            // buffer drop'ta otomatik iade edilir
        })
        .register()?;

    // 32-bit paket formatlar yeterli (dönüşüm tek satır). YUV istenmiyor.
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
            spa::utils::Rectangle { width: 4096, height: 4096 }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 15, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 30, denom: 1 }
        ),
    );
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow::anyhow!("pod serialize: {e}"))?
    .0
    .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&values)
        .ok_or_else(|| anyhow::anyhow!("pod parse"))?];

    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    tracing::info!("pipewire stream bağlandı, sessiz kareler alınıyor");
    mainloop.run();
    Ok(())
}

/// Portal restore token sakla/oku (kalıcı ekran izni).
fn token_path() -> std::path::PathBuf {
    remote_friend_common::identity::config_dir().join("pw_restore_token")
}

fn load_restore_token() -> Option<String> {
    std::fs::read_to_string(token_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn save_restore_token(t: &str) {
    let _ = std::fs::write(token_path(), t);
}

/// MemFd'yi salt-okunur map'le. fd pool'a ait, dup'lanıp hemen kapatılır.
fn read_memfd(fd: std::os::fd::RawFd) -> Option<memmap2::Mmap> {
    use std::os::fd::{FromRawFd, OwnedFd};
    if fd < 0 {
        return None;
    }
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        return None;
    }
    let owned: OwnedFd = unsafe { OwnedFd::from_raw_fd(dup) };
    let file = std::fs::File::from(owned);
    // Dosya boyutu kadar map'le (pool dosyasını aşmayız).
    let len = file.metadata().ok()?.len() as usize;
    if len == 0 {
        return None;
    }
    unsafe { memmap2::MmapOptions::new().len(len.min(64 * 1024 * 1024)).map(&file).ok() }
}

/// PipeWire satırını RGBA'ya çevir (stride dolgulu olabilir).
fn convert_row(
    w: u32,
    h: u32,
    stride: i32,
    offset: usize,
    mem: &[u8],
    vf: spa::param::video::VideoFormat,
) -> Option<Vec<u8>> {
    use spa::param::video::VideoFormat as V;
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    let stride = if stride <= 0 { (w as usize) * 4 } else { stride as usize };
    if stride < (w as usize) * 4 {
        return None;
    }
    let need = offset + stride * (h as usize);
    if mem.len() < need {
        return None;
    }
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..(h as usize) {
        let src = &mem[offset + y * stride..offset + y * stride + (w as usize) * 4];
        let dst = &mut out[y * (w as usize) * 4..(y + 1) * (w as usize) * 4];
        if vf == V::BGRx {
            for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = 255;
            }
        } else if vf == V::RGBx {
            for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                d[0] = s[0];
                d[1] = s[1];
                d[2] = s[2];
                d[3] = 255;
            }
        } else if vf == V::BGRA {
            for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = s[3];
            }
        } else if vf == V::RGBA {
            dst.copy_from_slice(src);
        } else {
            return None;
        }
    }
    Some(out)
}
