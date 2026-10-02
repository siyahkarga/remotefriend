//! Host: ekranı paylaşan taraf.
//! Native TCP + tarayıcı için HTTP/WebSocket sunar.

mod web;
#[cfg(target_os = "linux")]
mod capture_pw;

use anyhow::{Context, Result};
use remote_friend_common::{Packet, VideoCodec, VideoFrame};
use std::io::Cursor;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

pub(crate) fn target_fps() -> u32 {
    std::env::var("RF_FPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|fps: &u32| (5..=30).contains(fps))
        .unwrap_or(30)
}

pub(crate) fn frame_duration() -> Duration {
    Duration::from_micros(1_000_000 / target_fps() as u64)
}

fn max_width() -> u32 {
    std::env::var("RF_MAX_WIDTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|w: &u32| (640..=3840).contains(w))
        .map(|w| w & !1)
        .unwrap_or(1920)
}

fn bitrate_bps() -> u32 {
    std::env::var("RF_BITRATE_BPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|bps: &u32| (500_000..=30_000_000).contains(bps))
        .unwrap_or(6_000_000)
}

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
    let configured_password = std::env::var("REMOTE_FRIEND_PASS")
        .ok()
        .filter(|s| !s.is_empty());
    let password = configured_password
        .clone()
        .unwrap_or_else(remote_friend_common::new_session_password);
    if password.len() < 10 || password == "1234" {
        tracing::warn!("zayıf REMOTE_FRIEND_PASS: en az 10 karakter kullan");
    }
    let pc_name = hostname::get()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or("bilinmeyen-pc".into());
    let native_bind = std::env::var("RF_NATIVE_BIND").unwrap_or_else(|_| "0.0.0.0".into());
    let addr = format!("{native_bind}:{}", remote_friend_common::DEFAULT_PORT);
    let listener = TcpListener::bind(&addr).await?;
    println!("=== RemoteFriend Host: {pc_name} ===");
    if configured_password.is_none() {
        println!("Bu çalıştırma için üretilen şifre: {password}");
    } else {
        println!("Şifre REMOTE_FRIEND_PASS ortam değişkeninden alındı (ekrana yazılmadı).");
    }
    println!("Dinleniyor: {addr}");
    println!("Gelen her bağlantı ONAY ister (E/H). Otomatik kabul için: REMOTE_FRIEND_AUTO_ACCEPT=1");
    if native_bind != "127.0.0.1" && native_bind != "::1" {
        println!("UYARI: LAN native bağlantısı şimdilik düz TCP'dir; yalnızca güvenilir LAN/VPN'de kullan.");
    }
    println!("Video profili: {} FPS, en çok {} px genişlik, {} bit/s H.264", target_fps(), max_width(), bitrate_bps());
    tracing::info!("host dinliyor: {addr}");

    // Kalıcı internet ID'si + bu ID'yi VPS üzerinde sahiplenen cihaz sırrı.
    let host_id = remote_friend_common::identity::load_or_create_host_id();
    let host_secret = remote_friend_common::identity::load_or_create_host_secret();
    println!(">>> Bu bilgisayarın ID'si: {} <<<", remote_friend_common::format_id(&host_id));
    match rv_target() {
        Some(t) => {
            println!("İnternet (VPS {}) açık.", t.addr);
            let (id_c, secret_c, pc_c, pw_c) = (host_id.clone(), host_secret.clone(), pc_name.clone(), password.clone());
            tokio::spawn(async move { uplink_loop(t, id_c, secret_c, pc_c, pw_c).await });
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
    println!("Not: LAN HTTP güvenli bağlam değildir; tarayıcı çoğunlukla JPEG moduna düşer. Akıcı video için native istemci veya HTTPS/WSS kullan.");
    let web_bind = std::env::var("RF_WEB_BIND").unwrap_or_else(|_| "0.0.0.0".into());
    let shown_web_host = if web_bind == "0.0.0.0" || web_bind == "::" { lan_ip.clone() } else { web_bind.clone() };
    println!("Tarayıcı web bind: {web_bind}:{http_port}; açılacak adres: http://{shown_web_host}:{http_port}");
    let web_pw = password.clone();
    tokio::spawn(async move {
        if let Err(e) = web::serve(format!("{web_bind}:{http_port}"), web_pw).await {
            tracing::warn!("web sunucusu kapandı: {e:#}");
        }
    });

    // LAN discovery beacon (clientlar listede görsün)
    std::thread::spawn({
        let pc_name = pc_name.clone();
        move || remote_friend_common::discovery::broadcast_loop(pc_name, remote_friend_common::DEFAULT_PORT)
    });

    let native_limit = std::env::var("RF_MAX_NATIVE_SESSIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n: &usize| *n > 0)
        .unwrap_or(8);
    let native_slots = Arc::new(tokio::sync::Semaphore::new(native_limit));
    loop {
        let (socket, peer) = listener.accept().await?;
        let permit = match native_slots.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("native oturum limiti dolu; {peer} reddedildi");
                drop(socket);
                continue;
            }
        };
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
            let _permit = permit;
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

/// Basit sabit-zamanlı parola karşılaştırması. Uzunluk bilgisi gizli sayılmaz;
/// eşit uzunlukta ilk farklı baytta erken dönmez.
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.as_bytes()
        .iter()
        .zip(b.as_bytes())
        .fold(0u8, |diff, (x, y)| diff | (x ^ y))
        == 0
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
    let pkt = tokio::time::timeout(
        Duration::from_secs(15),
        remote_friend_common::io::read_packet_limited(&mut rd, 64 * 1024),
    )
        .await
        .context("handshake zaman aşımı")??;
    let ok = match &pkt {
        Packet::Handshake(h) => {
            h.version == remote_friend_common::PROTOCOL_VERSION && constant_time_eq(&h.password, password)
        }
        _ => false,
    };
    if !ok {
        tokio::time::sleep(Duration::from_millis(250)).await;
        write_packet(&mut wr, &Packet::Reject("şifre/version hatalı".into())).await?;
        anyhow::bail!("auth başarısız");
    }
    if need_approval {
        // parola doğru ama YETMEZ: operatör onayı şart
        write_packet(&mut wr, &Packet::WaitingForApproval).await?;
        tracing::info!("{peer} parola ok, operatör onayı bekleniyor");
        let prompt_peer = peer.clone();
        let approved = tokio::task::spawn_blocking(move || ask_approval(&prompt_peer))
            .await
            .unwrap_or(false);
        if !approved {
            write_packet(&mut wr, &Packet::Reject("host bağlantıyı reddetti".into())).await?;
            anyhow::bail!("operatör reddetti");
        }
    }
    write_packet(&mut wr, &Packet::Accept).await?;
    tracing::info!("auth ok, yayın başlıyor");

    let wr = Arc::new(Mutex::new(wr));

    // Tek global capture+encode hattına abone ol. Yavaş istemci kare biriktirmez;
    // broadcast kuyruğu dolarsa eski kareleri atlayıp en güncele yetişir.
    let wr2 = wr.clone();
    let mut video_rx = video_sender().subscribe();
    let _video_task = tokio::spawn(async move {
        loop {
            let frame = match video_rx.recv().await {
                Ok(frame) => frame,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("istemci {n} video karesi geride kaldı; eski kareler atlandı");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let packet = Packet::Video(VideoFrame {
                seq: frame.seq,
                width: frame.width,
                height: frame.height,
                codec: VideoCodec::H264,
                data: frame.data.as_ref().clone(),
                timestamp_ms: frame.timestamp_ms,
            });
            let mut g = wr2.lock().await;
            if let Err(e) = write_packet(&mut *g, &packet).await {
                tracing::warn!("video yazma hatası: {e:#}");
                break;
            }
        }
    });

    // input + dosya alıcı. Bağlantı bitince video göndericiyi mutlaka durdur.
    let session_result: Result<()> = loop {
        let pkt = match remote_friend_common::io::read_packet_limited(&mut rd, 1_000_000).await {
            Ok(pkt) => pkt,
            Err(e) => break Err(e),
        };
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
    };
    _video_task.abort();
    session_result

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
    } else {
        // Parmak izi yoksa sistem kök sertifikalarıyla dene (domain + LE kuruluysa sorunsuz).
        match remote_friend_common::tls::tls_connect(&t.addr, &t.sni, None).await {
            Ok(s) => {
                tracing::info!("rendezvous TLS (sistem sertifikası) ile bağlanıldı");
                let (r, w) = tokio::io::split(s);
                Ok((Box::new(r), Box::new(w)))
            }
            Err(e) => {
                if std::env::var("RF_PLAIN_OK").map(|v| v == "1").unwrap_or(false) {
                    tracing::warn!("!!! rendezvous DÜZ bağlanıyor (test modu)");
                    let s = tokio::net::TcpStream::connect(&t.addr).await?;
                    let (r, w) = s.into_split();
                    Ok((Box::new(r), Box::new(w)))
                } else {
                    // TOFU: sunucuya ilk kez bağlanılıyor. Parmak izini göster, operatör
                    // onaylarsa kalıcı kaydet ve pinli bağlan. Bir daha sorulmaz.
                    tracing::warn!("sistem sertifikasıyla doğrulanamadı ({e:#}); TOFU soruluyor");
                    match tofu_approve_server(&t.addr, &t.sni).await? {
                        Some(fp) => {
                            let s = remote_friend_common::tls::tls_connect(&t.addr, &t.sni, Some(fp)).await?;
                            let (r, w) = tokio::io::split(s);
                            Ok((Box::new(r), Box::new(w)))
                        }
                        None => anyhow::bail!("sunucu parmak izi onaylanmadı"),
                    }
                }
            }
        }
    }
}

/// TOFU: bilinmeyen sunucunun parmak izini terminalde göster, operatör
/// onaylarsa config'e kalıcı kaydet. Reddedilirse/zaman aşımında None.
/// Ret bir kez verildiyse bu çalıştırmada tekrar sorulmaz.
static TOFU_DECLINED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

async fn tofu_approve_server(addr: &str, sni: &str) -> Result<Option<String>> {
    if TOFU_DECLINED.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(None);
    }
    let fp = remote_friend_common::tls::fetch_server_fingerprint(addr, sni).await?;
    println!("*** Bu sunucuya ilk kez bağlanılıyor: {addr}");
    println!("*** Sertifika parmak izi: {fp}");
    println!("*** Güvenip kalıcı kaydetmek istiyor musun? (E = evet / H = hayır, 60 sn)");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let _ = tx.send(line);
    });
    let ok = match rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(line) => matches!(line.trim().to_lowercase().as_str(), "e" | "evet" | "y" | "yes"),
        Err(_) => {
            println!("*** zaman aşımı, vazgeçildi");
            false
        }
    };
    if !ok {
        TOFU_DECLINED.store(true, std::sync::atomic::Ordering::Relaxed);
        return Ok(None);
    }
    let mut cfg = remote_friend_common::identity::load_rv_config();
    cfg.server = addr.to_string();
    cfg.fp = fp.clone();
    remote_friend_common::identity::save_rv_config(&cfg);
    println!("*** parmak izi kaydedildi, bir daha sorulmayacak");
    Ok(Some(fp))
}

async fn uplink_loop(target: RvTarget, id: String, secret: String, pc_name: String, password: String) {
    loop {
        match uplink_once(&target, &id, &secret, &pc_name, &password).await {
            Ok(()) => tracing::warn!("uplink kapandı, 5 sn sonra yeniden denenecek"),
            Err(e) => tracing::warn!("uplink hatası: {e:#} (5 sn sonra yeniden)"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn uplink_once(target: &RvTarget, id: &str, secret: &str, pc_name: &str, password: &str) -> Result<()> {
    use remote_friend_common::io::{read_rv, write_rv};
    use remote_friend_common::RvMsg;
    use tokio::sync::mpsc;

    let (mut rd, wr) = rv_connect(target).await?;
    let wr = Arc::new(Mutex::new(wr));
    // kayıt
    {
        let mut g = wr.lock().await;
        write_rv(&mut *g, &RvMsg::Register { id: id.to_string(), name: pc_name.to_string(), secret: secret.to_string() }).await?;
        let resp = read_rv(&mut rd).await?;
        match resp {
            RvMsg::RegisteredOk => tracing::info!("rendezvous kaydı OK (ID: {})", remote_friend_common::format_id(id)),
            RvMsg::RegisterError(m) => anyhow::bail!("kayıt reddedildi: {m}"),
            _ => anyhow::bail!("kayıt sırasında beklenmeyen yanıt"),
        }
    }
    // giden kanal: heartbeat + hızlı ret
    let (out_tx, mut out_rx) = mpsc::channel::<RvMsg>(64);
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
            RvMsg::ApprovalRequest { client, addr, kind, token, auth } => {
                let peer = format!("internet:{client} ({addr}, {kind})");
                let auth_ok = auth.as_deref().is_some_and(|candidate| constant_time_eq(candidate, password));
                if !auth_ok {
                    tracing::warn!("{peer}: parola doğrulaması başarısız; kullanıcıya onay sorulmadı");
                    let _ = out_tx.try_send(RvMsg::ApprovalAnswer { client, allow: false });
                    continue;
                }
                let out_tx2 = out_tx.clone();
                let target2 = RvTarget { addr: target.addr.clone(), fp: target.fp.clone(), sni: target.sni.clone() };
                let password = password.to_string();
                let id = id.to_string();
                let secret = secret.to_string();
                tokio::spawn(async move {
                    let peer_for_prompt = peer.clone();
                    let ok = tokio::task::spawn_blocking(move || ask_approval(&peer_for_prompt))
                        .await
                        .unwrap_or(false);
                    if ok {
                        dial_back(&target2, token, &kind, &password, &id, &secret).await;
                    } else {
                        let _ = out_tx2.try_send(RvMsg::ApprovalAnswer { client, allow: false });
                    }
                });
            }
            _ => {}
        }
    }
}

/// Onaylanan client için sunucuya geri bağlan, oturumu bu hat üzerinden yürüt.
async fn dial_back(target: &RvTarget, token: u128, kind: &str, password: &str, id: &str, secret: &str) {
    use remote_friend_common::io::write_rv;
    use remote_friend_common::RvMsg;
    let peer = format!("internet:{token}");
    match rv_connect(target).await {
        Ok((mut rd, mut wr)) => {
            if write_rv(&mut wr, &RvMsg::ConnectBack { token, id: id.to_string(), secret: secret.to_string() }).await.is_err() {
                tracing::warn!("{peer}: ConnectBack yazılamadı");
                return;
            }
            tracing::info!("{peer}: dial-back kuruldu ({kind})");
            if kind == "web-jpeg" {
                web::session_kmsg_jpeg(rd, wr).await;
            } else if kind == "web" {
                web::session_kmsg(rd, wr).await;
            } else {
                let _ = session_native(rd, wr, peer.clone(), password, false).await;
            }
            tracing::info!("{peer}: oturum kapandı");
        }
        Err(e) => tracing::warn!("{peer}: dial-back bağlanamadı: {e:#}"),
    }
}

#[derive(Debug)]
pub(crate) struct EncodedFrame {
    pub(crate) seq: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) data: Arc<Vec<u8>>,
    pub(crate) timestamp_ms: u64,
}

static VIDEO_TX: OnceLock<tokio::sync::broadcast::Sender<Arc<EncodedFrame>>> = OnceLock::new();

/// Capture + yazılımsal H.264 encode yalnızca bir kez yapılır. Her istemci aynı
/// düşük gecikmeli akışa abone olur; yavaş istemci eski kareleri biriktirmez.
pub(crate) fn video_sender() -> &'static tokio::sync::broadcast::Sender<Arc<EncodedFrame>> {
    VIDEO_TX.get_or_init(|| {
        let (tx, _) = tokio::sync::broadcast::channel::<Arc<EncodedFrame>>(3);
        let worker_tx = tx.clone();
        std::thread::Builder::new()
            .name("rf-capture-h264".into())
            .spawn(move || {
                let mut encoder = H264Enc::new();
                let mut seq = 0u64;
                let mut errors = 0u32;
                let period = frame_duration();
                let mut next = Instant::now();
                let mut last_receivers = 0usize;
                let mut stat_started = Instant::now();
                let mut stat_attempts = 0u32;
                let mut stat_encoded = 0u32;
                let mut stat_encode_attempts = 0u32;
                let mut stat_capture = Duration::ZERO;
                let mut stat_encode = Duration::ZERO;
                let mut stat_size = (0u32, 0u32);
                loop {
                    let receivers = worker_tx.receiver_count();
                    if receivers == 0 {
                        last_receivers = 0;
                        stat_started = Instant::now();
                        stat_attempts = 0;
                        stat_encoded = 0;
                        stat_encode_attempts = 0;
                        stat_capture = Duration::ZERO;
                        stat_encode = Duration::ZERO;
                        std::thread::sleep(Duration::from_millis(100));
                        next = Instant::now();
                        continue;
                    }
                    if receivers > last_receivers {
                        // Yeni izleyici P-frame zincirinin ortasına girmesin. Encoder'ı
                        // yenilemek SPS/PPS + IDR başlangıcı üretir; mevcut decoderlar da
                        // bu temiz ana kare ile devam edebilir.
                        encoder = H264Enc::new();
                    }
                    last_receivers = receivers;

                    seq = seq.wrapping_add(1);
                    stat_attempts = stat_attempts.saturating_add(1);
                    let capture_started = Instant::now();
                    let captured = capture_rgba();
                    stat_capture += capture_started.elapsed();
                    match captured {
                        Ok((width, height, rgba)) => {
                            stat_size = (width, height);
                            stat_encode_attempts = stat_encode_attempts.saturating_add(1);
                            let encode_started = Instant::now();
                            let encoded = encoder.encode_frame(&rgba, width, height);
                            stat_encode += encode_started.elapsed();
                            match encoded {
                                Ok(data) if !data.is_empty() => {
                                    stat_encoded = stat_encoded.saturating_add(1);
                                    errors = 0;
                                    let timestamp_ms = SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_millis() as u64;
                                    let _ = worker_tx.send(Arc::new(EncodedFrame {
                                        seq,
                                        width,
                                        height,
                                        data: Arc::new(data),
                                        timestamp_ms,
                                    }));
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    errors += 1;
                                    if errors <= 3 || errors % 30 == 0 {
                                        tracing::warn!("global h264 encode hatası ({errors}): {e:#}");
                                    }
                                    if errors >= 30 {
                                        encoder = H264Enc::new();
                                        errors = 0;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            errors += 1;
                            if errors <= 3 || errors % 60 == 0 {
                                tracing::warn!("global capture hatası ({errors}): {e:#}");
                            }
                        }
                    }

                    let stat_elapsed = stat_started.elapsed();
                    if stat_elapsed >= Duration::from_secs(5) {
                        let seconds = stat_elapsed.as_secs_f64().max(0.001);
                        let attempts = stat_attempts.max(1) as f64;
                        let encode_attempts = stat_encode_attempts.max(1) as f64;
                        tracing::info!(
                            "video ölçüm: {:.1} fps, capture {:.1} ms, encode {:.1} ms, {}x{}, izleyici {}",
                            stat_encoded as f64 / seconds,
                            stat_capture.as_secs_f64() * 1000.0 / attempts,
                            stat_encode.as_secs_f64() * 1000.0 / encode_attempts,
                            stat_size.0,
                            stat_size.1,
                            receivers,
                        );
                        stat_started = Instant::now();
                        stat_attempts = 0;
                        stat_encoded = 0;
                        stat_encode_attempts = 0;
                        stat_capture = Duration::ZERO;
                        stat_encode = Duration::ZERO;
                    }

                    next = next + period;
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    } else {
                        // İşlem süresi hedef periyodu aştıysa fazladan uyuma ve gecikme biriktirme.
                        next = now;
                    }
                }
            })
            .expect("capture/encode thread başlatılamadı");
        tx
    })
}

thread_local! {
    /// xcap monitor listesini her karede yeniden taramak pahalıdır. Her capture
    /// thread'i seçili monitörü saklar; capture hata verirse bir sonraki karede yeniler.
    static XCAP_MONITOR: std::cell::RefCell<Option<xcap::Monitor>> = const { std::cell::RefCell::new(None) };
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
    {
        if let Some((w, h, rgba)) = capture_pw::try_get_frame() {
            if let Some(img) = image::ImageBuffer::from_raw(w, h, rgba) {
                return finish_rgba(img, 1.0, 0, 0);
            }
        }
        if capture_pw::is_ready() {
            anyhow::bail!("pipewire yeni karesi bekleniyor");
        }
    }
    // Wayland + portal izni beklenirken xcap'e düşme (deklanşör sesi yok):
    // çağrıcı bu hatada kare atlar, izin gelince sessiz akış başlar.
    #[cfg(target_os = "linux")]
    if capture_pw::portal_pending() {
        anyhow::bail!("portal izni bekleniyor (sessiz)");
    }
    let (img, scale, mx, my) = XCAP_MONITOR.with(|slot| -> Result<_> {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(
                xcap::Monitor::all()
                    .context("monitör listesi alınamadı")?
                    .into_iter()
                    .next()
                    .context("monitör yok")?,
            );
        }
        let result = {
            let mon = slot.as_ref().expect("monitor az önce oluşturuldu");
            let scale = mon.scale_factor().unwrap_or(1.0);
            let scale = if scale > 0.0 { scale } else { 1.0 };
            let (mx, my) = (mon.x().unwrap_or(0), mon.y().unwrap_or(0));
            mon.capture_image()
                .context("ekran yakalanamadı")
                .map(|img| (img, scale, mx, my))
        };
        if result.is_err() {
            *slot = None;
        }
        result
    })?;
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

    let max_w = max_width();
    let img = if w > max_w {
        let mut nh = (h as f32 * (max_w as f32 / w as f32)) as u32;
        nh &= !1; // çift yap
        image::imageops::resize(&img, max_w, nh.max(2), image::imageops::FilterType::Triangle)
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

/// Encoder ilk kurulduğunda IDR/SPS/PPS üretir; global yayın hattı bunu paylaşır.
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
                UsageType, VuiConfig,
            };
            let fps = target_fps();
            let config = EncoderConfig::new()
                .bitrate(BitRate::from_bps(bitrate_bps()))
                .max_frame_rate(FrameRate::from_hz(fps as f32))
                .usage_type(UsageType::ScreenContentRealTime)
                .profile(Profile::Baseline)
                .level(Level::Level_4_0)
                .skip_frames(true)
                .num_threads(0)
                .vui(VuiConfig::bt709())
                .intra_frame_period(IntraFramePeriod::from_num_frames(fps));
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
        // Her ~1 saniyede bir keyframe: global akışa yeni katılan istemci hızlı başlar.
        if self.frames % target_fps() as u64 == 0 {
            enc.force_intra_frame();
        }
        Ok(out)
    }
}

static INPUT_TX: OnceLock<std::sync::mpsc::SyncSender<remote_friend_common::InputEvent>> = OnceLock::new();

fn input_sender() -> &'static std::sync::mpsc::SyncSender<remote_friend_common::InputEvent> {
    INPUT_TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<remote_friend_common::InputEvent>(512);
        std::thread::Builder::new()
            .name("rf-input".into())
            .spawn(move || {
                use enigo::{Enigo, Settings};
                let mut enigo = match Enigo::new(&Settings::default()) {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::error!("enigo açılamadı: {e}");
                        return;
                    }
                };
                while let Ok(ev) = rx.recv() {
                    if let Err(e) = apply_input_now(&mut enigo, ev) {
                        tracing::warn!("input uygulama hatası: {e:#}");
                    }
                }
            })
            .expect("input thread başlatılamadı");
        tx
    })
}

pub(crate) fn apply_input(ev: remote_friend_common::InputEvent) -> Result<()> {
    use remote_friend_common::InputEvent;
    use std::sync::mpsc::TrySendError;
    match input_sender().try_send(ev) {
        Ok(()) => Ok(()),
        // Mouse hareketinde en yeni koordinat kısa süre sonra geleceği için eskiyi düşürmek,
        // input kuyruğunun büyüyüp saniyeler geriden gelmesinden daha doğrudur.
        Err(TrySendError::Full(InputEvent::MouseMove { .. })) => Ok(()),
        Err(TrySendError::Full(_)) => anyhow::bail!("input kuyruğu dolu"),
        Err(TrySendError::Disconnected(_)) => anyhow::bail!("input worker kapalı"),
    }
}

fn apply_input_now(enigo: &mut enigo::Enigo, ev: remote_friend_common::InputEvent) -> Result<()> {
    use enigo::{Axis, Coordinate, Keyboard, Mouse};
    use remote_friend_common::InputEvent as E;
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

struct IncomingTransfer {
    file: std::fs::File,
    part_path: std::path::PathBuf,
    final_path: std::path::PathBuf,
    expected: u64,
    total: u64,
    updated: Instant,
}

static TRANSFERS: OnceLock<std::sync::Mutex<std::collections::HashMap<u64, IncomingTransfer>>> = OnceLock::new();

fn transfer_map() -> &'static std::sync::Mutex<std::collections::HashMap<u64, IncomingTransfer>> {
    TRANSFERS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn max_file_bytes() -> u64 {
    std::env::var("RF_MAX_FILE_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n: &u64| *n > 0)
        .unwrap_or(512 * 1024 * 1024)
}

fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("dosya");
    let mut out = String::with_capacity(base.len().min(128));
    for c in base.chars().take(128) {
        if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() || out == "." || out == ".." {
        "dosya".into()
    } else {
        out
    }
}

pub(crate) fn save_chunk(c: remote_friend_common::FileChunk) -> Result<()> {
    use std::io::Write as _;

    const MAX_CHUNK: usize = 256 * 1024;
    if c.transfer_id == 0 {
        anyhow::bail!("geçersiz transfer ID");
    }
    if c.total == 0 || c.total > max_file_bytes() {
        anyhow::bail!("dosya boyutu sınır dışında: {}", c.total);
    }
    if c.data.is_empty() || c.data.len() > MAX_CHUNK {
        anyhow::bail!("dosya parçası sınır dışında: {}", c.data.len());
    }
    let end = c.offset
        .checked_add(c.data.len() as u64)
        .context("dosya offset taşması")?;
    if end > c.total {
        anyhow::bail!("dosya parçası bildirilen toplamı aşıyor");
    }
    // Hatalı son-parça işaretini dosyayı oluşturmadan/yazmadan önce reddet.
    // Aksi halde saldırgan geçersiz ilk chunk'larla transfer yuvalarını 10 dakika
    // boyunca doldurabilirdi.
    let reached_end = end == c.total;
    if c.last != reached_end {
        anyhow::bail!("dosya son-parça işareti toplam boyutla uyuşmuyor");
    }

    let dir = std::env::var("REMOTE_FRIEND_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| remote_friend_common::identity::config_dir().join("received"));
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }

    let mut transfers = transfer_map().lock().unwrap();
    // Yarım kalan transferleri sonsuza kadar açık tutma.
    let stale: Vec<u64> = transfers
        .iter()
        .filter(|(_, t)| t.updated.elapsed() > Duration::from_secs(600))
        .map(|(id, _)| *id)
        .collect();
    for id in stale {
        if let Some(t) = transfers.remove(&id) {
            let _ = std::fs::remove_file(t.part_path);
        }
    }

    if c.offset == 0 {
        if transfers.len() >= 8 {
            anyhow::bail!("çok fazla eşzamanlı dosya transferi");
        }
        if transfers.contains_key(&c.transfer_id) {
            anyhow::bail!("transfer ID zaten kullanımda");
        }
        let safe = safe_file_name(&c.name);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let random = remote_friend_common::new_secret_hex();
        let final_path = dir.join(format!("rf_{stamp}_{}_{}", &random[..8], safe));
        let part_path = final_path.with_extension("part");
        let mut opts = std::fs::OpenOptions::new();
        opts.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(&part_path)?;
        transfers.insert(c.transfer_id, IncomingTransfer {
            file,
            part_path,
            final_path,
            expected: 0,
            total: c.total,
            updated: Instant::now(),
        });
    }

    let complete = {
        let state = transfers
            .get_mut(&c.transfer_id)
            .context("transfer ilk parça ile başlamadı")?;
        if state.total != c.total || state.expected != c.offset {
            anyhow::bail!("dosya parçaları sırasız veya toplam boyut değişti");
        }
        state.file.write_all(&c.data)?;
        state.expected = end;
        state.updated = Instant::now();
        debug_assert_eq!(state.expected == state.total, reached_end);
        reached_end
    };

    if complete {
        let mut state = transfers.remove(&c.transfer_id).expect("transfer az önce vardı");
        state.file.flush()?;
        state.file.sync_all()?;
        drop(state.file);
        std::fs::rename(&state.part_path, &state.final_path)?;
        tracing::info!("dosya tamamlandı: {} ({} byte)", state.final_path.display(), state.total);
    } else {
        tracing::debug!("dosya parçası: id={} {}/{}", c.transfer_id, end, c.total);
    }
    Ok(())
}

// Framed paket IO ortak modülden (TCP/TLS/dial-back hepsi aynı).
use remote_friend_common::io::{read_packet, write_packet};

// Cursor import kullanıldı mı kontrolü için (derleyici uyarısını önle)
#[allow(dead_code)]
fn _use_cursor(b: &[u8]) {
    let _ = Cursor::new(b);
}
