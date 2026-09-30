//! Host: ekranı paylaşan taraf.
//! Gerçek capture (xcap) + JPEG + input (enigo) + dosya alma.

use anyhow::{Context, Result};
use remote_friend_common::{Packet, VideoCodec, VideoFrame};
use std::io::Cursor;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

const JPEG_QUALITY: u8 = 80;
const FPS_MS: u64 = 80; // ~12fps
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let password = std::env::var("REMOTE_FRIEND_PASS").unwrap_or("1234".into());
    let addr = format!("0.0.0.0:{}", remote_friend_common::DEFAULT_PORT);
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("host dinliyor: {addr} | şifre: {password} | bu IP'yi clienta ver");

    loop {
        let (socket, peer) = listener.accept().await?;
        tracing::info!("client bağlandı: {peer}");
        let pw = password.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(socket, &pw).await {
                tracing::warn!("client kapandı {peer}: {e:#}");
            }
        });
    }
}

async fn handle_client(socket: TcpStream, password: &str) -> Result<()> {
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
    write_packet(&mut wr, &Packet::Accept).await?;
    tracing::info!("auth ok, yayın başlıyor");

    let wr = Arc::new(Mutex::new(wr));

    // video gönderici task
    let wr2 = wr.clone();
    let video_task = tokio::spawn(async move {
        let mut seq = 0u64;
        loop {
            seq += 1;
            match capture_jpeg() {
                Ok((w, h, jpeg)) => {
                    let ts = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64;
                    let frame = VideoFrame {
                        seq,
                        width: w,
                        height: h,
                        codec: VideoCodec::Jpeg,
                        data: jpeg,
                        timestamp_ms: ts,
                    };
                    let mut g = wr2.lock().await;
                    if let Err(e) = write_packet(&mut *g, &Packet::Video(frame)).await {
                        tracing::warn!("video yazma hatası: {e:#}");
                        break;
                    }
                }
                Err(e) => {
                    // Wayland-GNOME'da xcap çalışmazsa client askıda kalmasın diye placeholder gönder
                    tracing::warn!("capture hatası (seq {seq}): {e:#} -- Linux Wayland ise Xorg ile giriş yap ya da Windows'ta host çalıştır");
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
        video_task.abort();
        Ok(())
    }
}

fn capture_jpeg() -> Result<(u32, u32, Vec<u8>)> {
    let monitors = xcap::Monitor::all().context("monitör listesi alınamadı (Wayland ise portal izni gerek)")?;
    let mon = monitors.into_iter().next().context("monitör yok")?;
    let scale = mon.scale_factor().unwrap_or(1.0);
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let (mx, my) = (mon.x().unwrap_or(0), mon.y().unwrap_or(0));
    let img = mon.capture_image().context("ekran yakalanamadı")?;
    let (w, h) = (img.width(), img.height());

    // hız için çok büyük ekranı küçült
    let img = if w > MAX_W {
        let nh = (h as f32 * (MAX_W as f32 / w as f32)) as u32;
        image::imageops::resize(&img, MAX_W, nh, image::imageops::FilterType::Triangle)
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

    let mut buf = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, JPEG_QUALITY);
    enc.encode_image(&img).context("jpeg encode")?;
    Ok((w2, h2, buf))
}

fn apply_input(ev: remote_friend_common::InputEvent) -> Result<()> {
    use enigo::{Button, Coordinate, Enigo, Keyboard, Mouse, Settings};
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
        E::KeyDown { code } => {
            // MVP: code ASCII ise text yaz
            if let Some(c) = char::from_u32(code) {
                enigo.key(enigo::Key::Unicode(c), enigo::Direction::Press)?;
            }
        }
        E::KeyUp { code } => {
            if let Some(c) = char::from_u32(code) {
                enigo.key(enigo::Key::Unicode(c), enigo::Direction::Release)?;
            }
        }
    }
    Ok(())
}

fn map_btn(b: remote_friend_common::MouseButton) -> enigo::Button {
    match b {
        remote_friend_common::MouseButton::Left => enigo::Button::Left,
        remote_friend_common::MouseButton::Right => enigo::Button::Right,
        remote_friend_common::MouseButton::Middle => enigo::Button::Middle,
    }
}

fn save_chunk(c: remote_friend_common::FileChunk) -> Result<()> {
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
