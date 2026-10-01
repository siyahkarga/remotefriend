//! Host: ekranı paylaşan taraf.
//! Native TCP + tarayıcı için HTTP/WebSocket sunar.

mod web;
#[cfg(target_os = "linux")]
mod capture_pw;

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
    remote_friend_common::tls::init_crypto();
    tracing_subscriber::fmt::init();
    #[cfg(target_os = "linux")]
    capture_pw::ensure_started();
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

    // Kalıcı internet ID'si (VPS rendezvous için)
    let host_id = remote_friend_common::identity::load_or_create_host_id();
    println!(">>> Bu bilgisayarın ID'si: {} <<<", remote_friend_common::format_id(&host_id));
    match rv_target() {
        Some(t) => {
            println!("İnternet (VPS {}) açık.", t.addr);
            let (id_c, pc_c, pw_c) = (host_id.clone(), pc_name.clone(), password.clone());
            tokio::spawn(async move { uplink_loop(t, id_c, pc_c, pw_c).await });
        }
        None => println!("İnternet kapalı (RF_RV_SERVER yok): sadece LAN."),
    }

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
        // Sertleştirme: tarayıcı/bot yanlışlıkla native porta gelirse
        // ("GET / HTTP..." devasa paket sanılıp bellek şişmesin) kapıda çevir.
        {
            use tokio::io::AsyncReadExt;
            let mut peek = [0u8; 4];
            match tokio::time::timeout(std::time::Duration::from_secs(5), socket.peek(&mut peek)).await {
                Ok(Ok(_)) => {
                    if peek == *b"GET " || peek == *b"POST" || peek == *b"HEAD" || peek == *b"PUT " {
                        tracing::warn!("{peer}: native porta HTTP geldi, kapatıldı (tarayıcı :33201'e açılmalı)");
                        continue;
                    }
                }
                _ => continue, // veri yok/zaman aşımı: kapat
            }
        }
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
    let (rd, wr) = socket.into_split();
    session_native(rd, wr, peer, password, true).await
}

/// Native session: LAN TCP veya VPS dial-back (TLS) fark etmez.
/// need_approval=false ise parola sonrası direkt Accept (onay zaten alındı).
async fn session_native<R, W>(
    mut rd: R,
    mut wr: W,
    peer: String,
    password: &str,
    need_approval: bool,
) -> Result<()>
where
    R: tokio::io::AsyncReadExt + Unpin + Send + 'static,
    W: tokio::io::AsyncWriteExt + Unpin + Send + 'static,
{
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
    if need_approval {
        // parola doğru ama YETMEZ: operatör onayı şart
        write_packet(&mut wr, &Packet::WaitingForApproval).await?;
        tracing::info!("{peer} parola ok, operatör onayı bekleniyor");
        if !ask_approval(&peer) {
            write_packet(&mut wr, &Packet::Reject("host bağlantıyı reddetti".into())).await?;
            anyhow::bail!("operatör reddetti");
        }
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

// ---- internet uplink (VPS rendezvous) ----

type BoxRd = Box<dyn tokio::io::AsyncRead + Unpin + Send>;
type BoxWr = Box<dyn tokio::io::AsyncWrite + Unpin + Send>;

struct RvTarget {
    addr: String,
    fp: Option<String>,
    sni: String,
}

/// Yoksa None döner (host LAN modunda çalışmaya devam eder).
fn rv_target() -> Option<RvTarget> {
    let cfg = remote_friend_common::identity::load_rv_config();
    let addr = std::env::var("RF_RV_SERVER")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| if cfg.server.trim().is_empty() { None } else { Some(cfg.server.clone()) })?;
    let fp = std::env::var("RF_RV_FP")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| if cfg.fp.trim().is_empty() { None } else { Some(cfg.fp.clone()) });
    let sni = addr.split(':').next().unwrap_or("rv").to_string();
    Some(RvTarget { addr, fp, sni })
}

async fn rv_connect(t: &RvTarget) -> Result<(BoxRd, BoxWr)> {
    if let Some(fp) = &t.fp {
        let s = remote_friend_common::tls::tls_connect(&t.addr, &t.sni, Some(fp.clone())).await?;
        let (r, w) = tokio::io::split(s);
        Ok((Box::new(r), Box::new(w)))
    } else if std::env::var("RF_PLAIN_OK").map(|v| v == "1").unwrap_or(false) {
        tracing::warn!("!!! rendezvous DÜZ bağlanıyor (test modu)");
        let s = tokio::net::TcpStream::connect(&t.addr).await?;
        let (r, w) = s.into_split();
        Ok((Box::new(r), Box::new(w)))
    } else {
        anyhow::bail!("TLS fingerprint yok (RF_RV_FP) ve RF_PLAIN_OK=1 değil — güvensiz internet yok");
    }
}

async fn uplink_loop(target: RvTarget, id: String, pc_name: String, password: String) {
    loop {
        match uplink_once(&target, &id, &pc_name, &password).await {
            Ok(()) => tracing::warn!("uplink kapandı, 5 sn sonra yeniden denenecek"),
            Err(e) => tracing::warn!("uplink hatası: {e:#} (5 sn sonra yeniden)"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn uplink_once(target: &RvTarget, id: &str, pc_name: &str, password: &str) -> Result<()> {
    use remote_friend_common::io::{read_rv, write_rv};
    use remote_friend_common::RvMsg;
    use tokio::sync::mpsc;

    let (mut rd, wr) = rv_connect(target).await?;
    let wr = Arc::new(Mutex::new(wr));
    // kayıt
    {
        let mut g = wr.lock().await;
        write_rv(&mut *g, &RvMsg::Register { id: id.to_string(), name: pc_name.to_string() }).await?;
        let resp = read_rv(&mut rd).await?;
        match resp {
            RvMsg::RegisteredOk => tracing::info!("rendezvous kaydı OK (ID: {})", remote_friend_common::format_id(id)),
            RvMsg::RegisterError(m) => anyhow::bail!("kayıt reddedildi: {m}"),
            _ => anyhow::bail!("kayıt sırasında beklenmeyen yanıt"),
        }
    }
    // giden kanal: heartbeat + hızlı ret
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RvMsg>();
    let wr2 = wr.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(20));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    let mut g = wr2.lock().await;
                    if write_rv(&mut *g, &RvMsg::Heartbeat).await.is_err() { break; }
                }
                msg = out_rx.recv() => {
                    match msg {
                        Some(m) => {
                            let mut g = wr2.lock().await;
                            if write_rv(&mut *g, &m).await.is_err() { break; }
                        }
                        None => break,
                    }
                }
            }
        }
    });
    // gelen: onay istekleri
    loop {
        match read_rv(&mut rd).await? {
            RvMsg::ApprovalRequest { client, addr, kind, token } => {
                let peer = format!("internet:{client} ({addr}, {kind})");
                println!("*** Bağlantı isteği: {peer}");
                let out_tx2 = out_tx.clone();
                let target2 = RvTarget { addr: target.addr.clone(), fp: target.fp.clone(), sni: target.sni.clone() };
                let password = password.to_string();
                tokio::spawn(async move {
                    let ok = tokio::task::spawn_blocking(move || ask_approval(&peer))
                        .await
                        .unwrap_or(false);
                    if ok {
                        dial_back(&target2, token, &kind, &password).await;
                    } else {
                        let _ = out_tx2.send(RvMsg::ApprovalAnswer { client, allow: false });
                    }
                });
            }
            _ => {}
        }
    }
}

/// Onaylanan client için sunucuya geri bağlan, oturumu bu hat üzerinden yürüt.
async fn dial_back(target: &RvTarget, token: u64, kind: &str, password: &str) {
    use remote_friend_common::io::write_rv;
    use remote_friend_common::RvMsg;
    let peer = format!("internet:{token}");
    match rv_connect(target).await {
        Ok((mut rd, mut wr)) => {
            if write_rv(&mut wr, &RvMsg::ConnectBack { token }).await.is_err() {
                tracing::warn!("{peer}: ConnectBack yazılamadı");
                return;
            }
            tracing::info!("{peer}: dial-back kuruldu ({kind})");
            if kind == "web" {
                web::session_kmsg(rd, wr).await;
            } else {
                let _ = session_native(rd, wr, peer.clone(), password, false).await;
            }
            tracing::info!("{peer}: oturum kapandı");
        }
        Err(e) => tracing::warn!("{peer}: dial-back bağlanamadı: {e:#}"),
    }
}

/// Ekran görüntüsü: RGBA + H264 için çift boyut garantili.
/// Dönen boyutlar her zaman çifttir (YUV420 şartı).
/// Wayland'da önce sessiz PipeWire akışı denenir (deklanşör sesi yok),
/// hazır değilse klasik xcap yoluna düşülür.
pub(crate) fn capture_rgba() -> Result<(u32, u32, Vec<u8>)> {
    // CI/test için sentetik kare (portal/xcap yok): renk zamanla kayar.
    if std::env::var("RF_TEST_PATTERN").map(|v| v == "1").unwrap_or(false) {
        let (w, h) = (960u32, 540u32);
        let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u32;
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                rgba.push((x * 255 / w) as u8);
                rgba.push((y * 255 / h) as u8);
                rgba.push((t / 40 % 256) as u8);
                rgba.push(255);
            }
        }
        return Ok((w, h, rgba));
    }
    #[cfg(target_os = "linux")]
    if let Some((w, h, rgba)) = capture_pw::try_get_frame() {
        if let Some(img) = image::ImageBuffer::from_raw(w, h, rgba) {
            return finish_rgba(img, 1.0, 0, 0);
        }
    }
    // Wayland + portal izni beklenirken xcap'e düşme (deklanşör sesi yok):
    // çağrıcı bu hatada kare atlar, izin gelince sessiz akış başlar.
    #[cfg(target_os = "linux")]
    if capture_pw::portal_pending() {
        anyhow::bail!("portal izni bekleniyor (sessiz)");
    }
    let monitors = xcap::Monitor::all().context("monitör listesi alınamadı")?;
    let mon = monitors.into_iter().next().context("monitör yok")?;
    let scale = mon.scale_factor().unwrap_or(1.0);
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let (mx, my) = (mon.x().unwrap_or(0), mon.y().unwrap_or(0));
    let img = mon.capture_image().context("ekran yakalanamadı")?;
    finish_rgba(img, scale, mx, my)
}

/// Ortak son işlem: 1080p üstünü küçült + çift boyuta kırp + GEO kaydet.
fn finish_rgba(
    img: image::RgbaImage,
    scale: f32,
    mx: i32,
    my: i32,
) -> Result<(u32, u32, Vec<u8>)> {
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

// Framed paket IO ortak modülden (TCP/TLS/dial-back hepsi aynı).
use remote_friend_common::io::{read_packet, write_packet};

// Cursor import kullanıldı mı kontrolü için (derleyici uyarısını önle)
#[allow(dead_code)]
fn _use_cursor(b: &[u8]) {
    let _ = Cursor::new(b);
}
