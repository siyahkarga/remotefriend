//! Host: ekranı paylaşan taraf.
//! Native TCP + tarayıcı için HTTP/WebSocket sunar.

mod web;

use anyhow::{Context, Result};
use remote_friend_common::{Packet, VideoCodec, VideoFrame};
use std::io::Cursor;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

pub(crate) const FPS_MS: u64 = 66; // ~15fps (H264 ile hafifler)
const MAX_W: u32 = 1920; // 1080p aynen, üstünü küçült

/// Son capture'ın geometrisi: client koordinatını host mantıksal koordinata çevirmek için.
/// (Windows %125/%150 ölçekte capture fiziksel piksel, mouse mantıksal piksel ister.)
#[derive(Clone, Copy)]
struct Geo {
    orig_w: u32,
    orig_h: u32,
    sent_w: u32,
    sent_h: u32,
    scale: f32,
    mon_x: i32,
    mon_y: i32,
}
static GEO: std::sync::Mutex<Geo> = std::sync::Mutex::new(Geo {
    orig_w: 0,
    orig_h: 0,
    sent_w: 0,
    sent_h: 0,
    scale: 1.0,
    mon_x: 0,
    mon_y: 0,
});

/// Windows: ekran yakalama + doğru ölçek için DPI-awareness şart.
/// (xcap "Process not DPI aware" verirse capture/ölçek bozulur.)
fn enable_dpi_awareness() {
    #[cfg(windows)]
    {
        use windows::Win32::UI::HiDpi::*;
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    enable_dpi_awareness();
    tracing_subscriber::fmt::init();
    let password = std::env::var("REMOTE_FRIEND_PASS").unwrap_or("1234".into());
    let pc_name = hostname::get()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or("bilinmeyen-pc".into());
    let addr = format!("0.0.0.0:{}", remote_friend_common::DEFAULT_PORT);
    let listener = TcpListener::bind(&addr).await?;
    println!("=== RemoteFriend Host: {pc_name} ===");
    println!("Dinleniyor: {addr} | şifre: {password}");
    println!("Gelen her bağlantı ONAY ister (E/H). Otomatik kabul için: REMOTE_FRIEND_AUTO_ACCEPT=1");
    tracing::info!("host dinliyor: {addr}");

    // Tarayıcı client için web sunucusu (kurulumsuz bağlantı)
    let http_port: u16 = std::env::var("RF_HTTP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(33201); // discovery UDP/33201 ile çakışmaz (TCP)
    let lan_ip = local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .unwrap_or("127.0.0.1".into());
    println!("Tarayıcı ile bağlan: http://{lan_ip}:{http_port}  (aynı ağdan)");
    let web_pw = password.clone();
    tokio::spawn(async move {
        if let Err(e) = web::serve(format!("0.0.0.0:{http_port}"), web_pw).await {
            tracing::warn!("web sunucusu kapandı: {e:#}");
        }
    });

    // LAN discovery beacon (clientlar listede görsün)
    std::thread::spawn({
        let pc_name = pc_name.clone();
        move || remote_friend_common::discovery::broadcast_loop(pc_name, remote_friend_common::DEFAULT_PORT)
    });

    loop {
        let (socket, peer) = listener.accept().await?;
        tracing::info!("client bağlandı: {peer}");
        let pw = password.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(socket, peer.to_string(), &pw).await {
                tracing::warn!("client kapandı {peer}: {e:#}");
            }
        });
    }
}

/// Aynı anda tek onay sorusu (karışmasın diye)
static APPROVAL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Host operatörüne sor. true = kabul. 30 sn cevap yoksa ret.
pub(crate) fn ask_approval(peer: &str) -> bool {
    if std::env::var("REMOTE_FRIEND_AUTO_ACCEPT").map(|v| v == "1").unwrap_or(false) {
        println!("*** {peer}: otomatik kabul (REMOTE_FRIEND_AUTO_ACCEPT=1)");
        return true;
    }
    let _guard = APPROVAL_LOCK.lock().unwrap();
    println!("*** Bağlantı isteği: {peer}");
    println!("*** Kabul ediyor musun? (E = evet / H = hayır, 30 sn içinde, varsayılan HAYIR)");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let _ = tx.send(line);
    });
    match rx.recv_timeout(std::time::Duration::from_secs(30)) {
        Ok(line) => {
            let ok = matches!(line.trim().to_lowercase().as_str(), "e" | "evet" | "y" | "yes");
            println!("*** {peer}: {}", if ok { "KABUL" } else { "RET" });
            ok
        }
        Err(_) => {
            println!("*** {peer}: zaman aşımı, REDDEDİLDİ");
            false
        }
    }
}

async fn handle_client(socket: TcpStream, peer: String, password: &str) -> Result<()> {
    let (mut rd, mut wr) = socket.into_split();
    // handshake
    let pkt = read_packet(&mut rd).await?;
    let ok = match &pkt {
        Packet::Handshake(h) => {
            h.version == remote_friend_common::PROTOCOL_VERSION && h.password == password
        }
        _ => false,
    };
    if !ok {
        write_packet(&mut wr, &Packet::Reject("şifre/version hatalı".into())).await?;
        anyhow::bail!("auth başarısız");
    }
    // parola doğru ama YETMEZ: operatör onayı şart
    write_packet(&mut wr, &Packet::WaitingForApproval).await?;
    tracing::info!("{peer} parola ok, operatör onayı bekleniyor");
    if !ask_approval(&peer) {
        write_packet(&mut wr, &Packet::Reject("host bağlantıyı reddetti".into())).await?;
        anyhow::bail!("operatör reddetti");
    }
    write_packet(&mut wr, &Packet::Accept).await?;
    tracing::info!("auth ok, yayın başlıyor");

    let wr = Arc::new(Mutex::new(wr));

    // video gönderici task (bağlantı başına taze H264 encoder: ilk frame IDR olur)
    let wr2 = wr.clone();
    let _video_task = tokio::spawn(async move {
        let mut seq = 0u64;
        let mut h264 = H264Enc::new();
        let mut err_n = 0u32;
        loop {
            seq += 1;
            match capture_rgba() {
                Ok((w, h, rgba)) => {
                    match h264.encode_frame(&rgba, w, h) {
                        Ok(nal) => {
                            err_n = 0;
                            let ts = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64;
                            let frame = VideoFrame {
                                seq,
                                width: w,
                                height: h,
                                codec: VideoCodec::H264,
                                data: nal,
                                timestamp_ms: ts,
                            };
                            let mut g = wr2.lock().await;
                            if let Err(e) = write_packet(&mut *g, &Packet::Video(frame)).await {
                                tracing::warn!("video yazma hatası: {e:#}");
                                break;
                            }
                        }
                        Err(e) => {
                            err_n += 1;
                            tracing::warn!("h264 encode hatası ({err_n}): {e:#}");
                            if err_n > 30 {
                                tracing::warn!("çok hata, encoder sıfırlanıyor");
                                h264 = H264Enc::new();
                                err_n = 0;
                            }
                        }
                    }
                }
                Err(e) => {
                    // capture çalışmazsa client askıda kalmasın diye placeholder gönder
                    tracing::warn!("capture hatası (seq {seq}): {e:#}");
                    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
                    let frame = VideoFrame {
                        seq,
                        width: 640,
                        height: 360,
                        codec: VideoCodec::RawRgba,
                        data: vec![((seq * 7) % 255) as u8; 64],
                        timestamp_ms: ts,
                    };
                    let mut g = wr2.lock().await;
                    if let Err(e2) = write_packet(&mut *g, &Packet::Video(frame)).await {
                        tracing::warn!("video yazma hatası: {e2:#}");
                        break;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(FPS_MS)).await;
        }
    });

    // input + dosya alıcı (bu taskte)
    loop {
        let pkt = read_packet(&mut rd).await?;
        match pkt {
            Packet::Input(ev) => {
                if let Err(e) = apply_input(ev) {
                    tracing::warn!("input hatası: {e:#}");
                }
            }
            Packet::File(chunk) => {
                if let Err(e) = save_chunk(chunk) {
                    tracing::warn!("dosya hatası: {e:#}");
                }
            }
            _ => {}
        }
    }
    // video_task abort on disconnect
    #[allow(unreachable_code)]
    {
        _video_task.abort();
        Ok(())
    }
}

/// Ekran görüntüsü: RGBA + H264 için çift boyut garantili.
/// Dönen boyutlar her zaman çifttir (YUV420 şartı).
pub(crate) fn capture_rgba() -> Result<(u32, u32, Vec<u8>)> {
    let monitors = xcap::Monitor::all().context("monitör listesi alınamadı")?;
    let mon = monitors.into_iter().next().context("monitör yok")?;
    let scale = mon.scale_factor().unwrap_or(1.0);
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let (mx, my) = (mon.x().unwrap_or(0), mon.y().unwrap_or(0));
    let img = mon.capture_image().context("ekran yakalanamadı")?;
    let (w, h) = (img.width(), img.height());

    let img = if w > MAX_W {
        let mut nh = (h as f32 * (MAX_W as f32 / w as f32)) as u32;
        nh &= !1; // çift yap
        image::imageops::resize(&img, MAX_W, nh.max(2), image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let (mut w2, mut h2) = (img.width(), img.height());
    w2 &= !1;
    h2 &= !1;
    // tek piksellik kırpma gerekiyorsa (boyut tekti) güvenli kırp
    let img = if w2 != img.width() || h2 != img.height() {
        image::imageops::crop_imm(&img, 0, 0, w2.max(2), h2.max(2)).to_image()
    } else {
        img
    };
    let (w2, h2) = (img.width(), img.height());

    *GEO.lock().unwrap() = Geo {
        orig_w: w,
        orig_h: h,
        sent_w: w2,
        sent_h: h2,
        scale,
        mon_x: mx,
        mon_y: my,
    };

    Ok((w2, h2, img.into_raw()))
}

/// Bağlantı başına bir tane: ilk frame her zaman IDR (SPS/PPS dahil).
pub(crate) struct H264Enc {
    enc: Option<openh264::encoder::Encoder>,
    w: u32,
    h: u32,
    frames: u64,
}

impl H264Enc {
    pub(crate) fn new() -> Self {
        Self { enc: None, w: 0, h: 0, frames: 0 }
    }

    pub(crate) fn encode_frame(&mut self, rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
        use openh264::formats::{RgbSliceU8, YUVBuffer};

        if self.enc.is_none() || self.w != w || self.h != h {
            use openh264::encoder::{
                BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, Level, Profile,
                UsageType,
            };
            let config = EncoderConfig::new()
                .bitrate(BitRate::from_bps(4_000_000))
                .max_frame_rate(FrameRate::from_hz(15.0))
                .usage_type(UsageType::ScreenContentRealTime)
                .profile(Profile::Baseline)
                .level(Level::Level_4_0)
                .intra_frame_period(IntraFramePeriod::from_num_frames(75));
            let mut enc = Encoder::with_api_config(openh264::OpenH264API::from_source(), config)
                .context("h264 encoder açılamadı")?;
            enc.force_intra_frame();
            self.enc = Some(enc);
            self.w = w;
            self.h = h;
            self.frames = 0;
        }
        let enc = self.enc.as_mut().unwrap();

        // RGBA -> RGB (alpha at)
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for px in rgba.chunks_exact(4) {
            rgb.extend_from_slice(&px[..3]);
        }
        let rgb_src = RgbSliceU8::new(&rgb, (w as usize, h as usize));
        let yuv = YUVBuffer::from_rgb_source(rgb_src);
        let bitstream = enc.encode(&yuv).context("h264 encode")?;
        let out = bitstream.to_vec();
        self.frames += 1;
        // her 5 sn'de bir keyframe (geç katılan/bozulan stream kendini toparlar)
        if self.frames % 75 == 0 {
            enc.force_intra_frame();
        }
        Ok(out)
    }
}

pub(crate) fn apply_input(ev: remote_friend_common::InputEvent) -> Result<()> {
    use enigo::{Axis, Coordinate, Enigo, Keyboard, Mouse, Settings};
    use remote_friend_common::InputEvent as E;
    // her eventte yeni Enigo (MVP basitliği, sonra kalıcı tut)
    let mut enigo = Enigo::new(&Settings::default()).context("enigo açılamadı")?;
    match ev {
        E::MouseMove { x, y } => {
            // client koordinatı (gönderilen frame uzayı) -> fiziksel -> mantıksal
            let g = GEO.lock().unwrap();
            let fx = if g.sent_w > 0 { x as f32 * g.orig_w as f32 / g.sent_w as f32 } else { x as f32 };
            let fy = if g.sent_h > 0 { y as f32 * g.orig_h as f32 / g.sent_h as f32 } else { y as f32 };
            let lx = g.mon_x + (fx / g.scale) as i32;
            let ly = g.mon_y + (fy / g.scale) as i32;
            enigo.move_mouse(lx, ly, Coordinate::Abs)?
        }
        E::MouseDown { button } => enigo.button(map_btn(button), enigo::Direction::Press)?,
        E::MouseUp { button } => enigo.button(map_btn(button), enigo::Direction::Release)?,
        E::Scroll { dx, dy } => {
            if dy != 0 {
                enigo.scroll(dy, Axis::Vertical)?;
            }
            if dx != 0 {
                enigo.scroll(dx, Axis::Horizontal)?;
            }
        }
        E::Key { key, down } => {
            let dir = if down { enigo::Direction::Press } else { enigo::Direction::Release };
            enigo.key(map_key(key), dir)?;
        }
    }
    Ok(())
}

fn map_key(k: remote_friend_common::RemoteKey) -> enigo::Key {
    use remote_friend_common::RemoteKey as R;
    match k {
        R::Char(c) => enigo::Key::Unicode(c),
        R::Enter => enigo::Key::Return,
        R::Tab => enigo::Key::Tab,
        R::Backspace => enigo::Key::Backspace,
        R::Escape => enigo::Key::Escape,
        R::Delete => enigo::Key::Delete,
        R::Insert => enigo::Key::Insert,
        R::Home => enigo::Key::Home,
        R::End => enigo::Key::End,
        R::PageUp => enigo::Key::PageUp,
        R::PageDown => enigo::Key::PageDown,
        R::Up => enigo::Key::UpArrow,
        R::Down => enigo::Key::DownArrow,
        R::Left => enigo::Key::LeftArrow,
        R::Right => enigo::Key::RightArrow,
        R::F1 => enigo::Key::F1,
        R::F2 => enigo::Key::F2,
        R::F3 => enigo::Key::F3,
        R::F4 => enigo::Key::F4,
        R::F5 => enigo::Key::F5,
        R::F6 => enigo::Key::F6,
        R::F7 => enigo::Key::F7,
        R::F8 => enigo::Key::F8,
        R::F9 => enigo::Key::F9,
        R::F10 => enigo::Key::F10,
        R::F11 => enigo::Key::F11,
        R::F12 => enigo::Key::F12,
        R::Shift => enigo::Key::Shift,
        R::Ctrl => enigo::Key::Control,
        R::Alt => enigo::Key::Alt,
        R::Meta => enigo::Key::Meta,
        R::CapsLock => enigo::Key::CapsLock,
        R::NumLock => enigo::Key::Numlock,
        R::PrintScreen => enigo::Key::PrintScr,
        R::Pause => enigo::Key::Pause,
    }
}

fn map_btn(b: remote_friend_common::MouseButton) -> enigo::Button {
    match b {
        remote_friend_common::MouseButton::Left => enigo::Button::Left,
        remote_friend_common::MouseButton::Right => enigo::Button::Right,
        remote_friend_common::MouseButton::Middle => enigo::Button::Middle,
    }
}

pub(crate) fn save_chunk(c: remote_friend_common::FileChunk) -> Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    let dir = std::env::var("REMOTE_FRIEND_DIR").unwrap_or("/tmp".into());
    std::fs::create_dir_all(&dir)?;
    // güvenlik: sadece dosya adı, path traversal yok
    let safe: String = c.name.rsplit(['/', '\\']).next().unwrap_or("dosya").to_string();
    let path = format!("{dir}/rf_{}_{safe}", c.transfer_id);
    let mut f = std::fs::OpenOptions::new().create(true).write(true).open(&path)?;
    f.seek(SeekFrom::Start(c.offset))?;
    f.write_all(&c.data)?;
    tracing::info!("dosya parçası: {path} {}/{} last={}", c.offset + c.data.len() as u64, c.total, c.last);
    Ok(())
}

async fn read_packet(r: &mut tokio::net::tcp::OwnedReadHalf) -> Result<Packet> {
    let len = r.read_u32().await? as usize;
    if len > 20_000_000 {
        anyhow::bail!("paket çok büyük: {len}");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(remote_friend_common::decode(&buf)?)
}

async fn write_packet(w: &mut tokio::net::tcp::OwnedWriteHalf, p: &Packet) -> Result<()> {
    let buf = remote_friend_common::encode(p)?;
    w.write_u32(buf.len() as u32).await?;
    w.write_all(&buf).await?;
    Ok(())
}

// Cursor import kullanıldı mı kontrolü için (derleyici uyarısını önle)
#[allow(dead_code)]
fn _use_cursor(b: &[u8]) {
    let _ = Cursor::new(b);
}
