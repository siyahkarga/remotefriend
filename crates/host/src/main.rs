//! Host: ekranı paylaşan taraf.
//! Native TCP + tarayıcı için HTTP/WebSocket sunar, isteğe bağlı VPS rölesine bağlanır.
//!
//! Platformlar: Linux Wayland (portal + PipeWire), Linux X11, Windows, macOS (xcap + enigo).

mod approval;
mod auth;
mod convert;
mod files;
mod input;
mod video;
#[cfg(target_os = "linux")]
mod wayland;
mod web;

use anyhow::{Context, Result};
use remote_friend_common::io::{read_packet_limited, write_packet};
use remote_friend_common::{Packet, VideoCodec, VideoFrame};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, Mutex};

static HOST_NAME: OnceLock<String> = OnceLock::new();

pub(crate) fn host_name() -> &'static str {
    HOST_NAME.get().map(String::as_str).unwrap_or("bilgisayar")
}

/// Windows: ekran yakalama + doğru ölçek için DPI-awareness şart.
fn enable_dpi_awareness() {
    #[cfg(windows)]
    {
        use windows::Win32::UI::HiDpi::*;
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }
}

/// Günlük: varsayılan "info"; D-Bus/portal kütüphanelerinin zararsız uyarıları gizlenir.
/// RUST_LOG ile değiştirilebilir (ör. RUST_LOG=debug,zbus=warn).
fn init_logging() {
    use tracing_subscriber::prelude::*;
    let default = "info,zbus=error,ashpd=error,pipewire=warn";
    let filter: tracing_subscriber::filter::Targets = std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| default.parse().expect("varsayılan günlük filtresi"));
    tracing_subscriber::registry().with(tracing_subscriber::fmt::layer()).with(filter).init();
}

fn env_port(name: &str, default: u16) -> u16 {
    std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

/// Telefon/tarayıcı için VPS web adresi (RF_WEB_URL, yoksa sunucu adından tahmin).
fn relay_web_url(server: &str) -> String {
    if let Ok(u) = std::env::var("RF_WEB_URL") {
        if !u.trim().is_empty() {
            return u.trim().to_string();
        }
    }
    let host = server.rsplit_once(':').map(|(h, _)| h).unwrap_or(server);
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        format!("https://{}.sslip.io", host.replace('.', "-"))
    } else {
        format!("https://{host}")
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    enable_dpi_awareness();
    remote_friend_common::tls::init_crypto();
    init_logging();

    let pc_name = hostname::get()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "bilgisayar".into());
    let _ = HOST_NAME.set(pc_name.clone());

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--forget-devices") {
        println!("{} güvenilir cihaz silindi; bir sonraki bağlantıda yine onay istenecek.", approval::forget_devices());
        return Ok(());
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("remote-friend-host [--new-password] [--forget-devices]");
        println!("  --new-password    kalıcı şifreyi yenile (eski şifre artık çalışmaz)");
        println!("  --forget-devices  'kalıcı izin' verilmiş tüm cihazları unut");
        return Ok(());
    }
    let (password, from_env) = auth::init(args.iter().any(|a| a == "--new-password"));

    // Wayland: izin penceresi host başlarken çıksın (operatör bilgisayar başındayken).
    #[cfg(target_os = "linux")]
    {
        wayland::init_runtime(tokio::runtime::Handle::current());
        if std::env::var("RF_TEST_PATTERN").map(|v| v != "1").unwrap_or(true) {
            wayland::ensure_started();
        }
    }

    let native_bind = std::env::var("RF_NATIVE_BIND").unwrap_or_else(|_| "0.0.0.0".into());
    let native_port = env_port("RF_NATIVE_PORT", remote_friend_common::DEFAULT_PORT);
    let addr = format!("{native_bind}:{native_port}");
    let listener = TcpListener::bind(&addr).await.with_context(|| {
        format!("{addr} dinlenemedi (başka bir host zaten çalışıyor olabilir; RF_NATIVE_PORT ile değiştir)")
    })?;

    let host_id = remote_friend_common::identity::load_or_create_host_id();
    let host_secret = remote_friend_common::identity::load_or_create_host_secret();
    let http_port = env_port("RF_HTTP_PORT", 33201);
    let lan_ip = local_ip_address::local_ip().map(|ip| ip.to_string()).unwrap_or_else(|_| "127.0.0.1".into());
    let web_bind = std::env::var("RF_WEB_BIND").unwrap_or_else(|_| "0.0.0.0".into());
    let rv = rv_target();

    println!();
    println!("  ┌────────────────────────────────────────────────────────┐");
    println!("    RemoteFriend Host  ·  {pc_name}");
    println!("  ├────────────────────────────────────────────────────────┤");
    println!("    Bilgisayar kodu : {}", remote_friend_common::format_id(&host_id));
    if from_env {
        println!("    Şifre           : (REMOTE_FRIEND_PASS'tan)");
    } else {
        println!("    Şifre           : {password}");
    }
    if let Some(t) = &rv {
        println!("    İnternetten     : {}", relay_web_url(&t.addr));
    } else {
        println!("    İnternetten     : kapalı (RF_RV_SERVER yok)");
    }
    println!("    Yerel ağdan     : http://{lan_ip}:{http_port}");
    println!("  └────────────────────────────────────────────────────────┘");
    let trusted = approval::trusted_count();
    if std::env::var("REMOTE_FRIEND_AUTO_ACCEPT").map(|v| v == "1").unwrap_or(false) {
        println!("  Onay KAPALI (REMOTE_FRIEND_AUTO_ACCEPT=1): doğru şifreyi bilen herkes bağlanır.");
    } else {
        println!("  Yeni cihazlar bu terminalde onay ister: E = bu sefer, K = kalıcı (cihaz hatırlanır).");
        if trusted > 0 {
            println!("  Kalıcı izinli cihaz: {trusted}  (hepsini unutmak için: remote-friend-host --forget-devices)");
        }
    }
    if !from_env {
        println!("  Şifre kalıcıdır; yenilemek için: remote-friend-host --new-password");
    }
    if native_bind != "127.0.0.1" && native_bind != "::1" {
        println!("  Not: yerel ağ yolu şifresizdir; internet yolu TLS/HTTPS kullanır.");
    }
    let prof = video::profile(video::preset());
    println!(
        "  Görüntü: {} ({} fps, en çok {} px, {} kbit/s)",
        video::preset().name(),
        prof.fps,
        prof.max_w,
        prof.bitrate / 1000
    );
    println!();

    if let Some(t) = rv {
        let (id_c, secret_c, pc_c) = (host_id.clone(), host_secret.clone(), pc_name.clone());
        tokio::spawn(async move { uplink_loop(t, id_c, secret_c, pc_c).await });
    }

    tokio::spawn({
        let name = pc_name.clone();
        async move {
            if let Err(e) = web::serve(format!("{web_bind}:{http_port}"), name).await {
                tracing::warn!("web sunucusu kapandı: {e:#}");
            }
        }
    });

    std::thread::spawn({
        let pc_name = pc_name.clone();
        move || remote_friend_common::discovery::broadcast_loop(pc_name, native_port)
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
                continue;
            }
        };
        tokio::spawn(async move {
            let _permit = permit;
            // Tarayıcı/bot yanlışlıkla native porta gelirse kapıda çevir.
            let mut peek = [0u8; 4];
            match tokio::time::timeout(Duration::from_secs(5), socket.peek(&mut peek)).await {
                Ok(Ok(4)) if matches!(&peek, b"GET " | b"POST" | b"HEAD" | b"PUT ") => {
                    tracing::warn!("{peer}: native porta HTTP geldi (tarayıcı :{http_port} adresine açılmalı)");
                    return;
                }
                Ok(Ok(n)) if n > 0 => {}
                _ => return,
            }
            let _ = socket.set_nodelay(true);
            tracing::info!("native istemci: {peer}");
            if let Err(e) = handle_client(socket, peer.to_string()).await {
                tracing::warn!("native istemci kapandı {peer}: {e:#}");
            }
        });
    }
}

async fn handle_client(socket: TcpStream, peer: String) -> Result<()> {
    let (rd, wr) = socket.into_split();
    session_native(rd, wr, peer, true).await
}

/// Native oturum: LAN TCP veya VPS dial-back (TLS) fark etmez.
/// need_approval=false ise parola sonrası direkt kabul (onay zaten alındı).
async fn session_native<R, W>(mut rd: R, mut wr: W, peer: String, need_approval: bool) -> Result<()>
where
    R: tokio::io::AsyncReadExt + Unpin + Send + 'static,
    W: tokio::io::AsyncWriteExt + Unpin + Send + 'static,
{
    let pkt = tokio::time::timeout(Duration::from_secs(15), read_packet_limited(&mut rd, 64 * 1024))
        .await
        .context("handshake zaman aşımı")??;
    let Packet::Handshake(h) = pkt else {
        anyhow::bail!("beklenmeyen ilk paket");
    };
    if h.version != remote_friend_common::PROTOCOL_VERSION {
        write_packet(
            &mut wr,
            &Packet::Reject(format!(
                "sürüm uyumsuz (host {}, istemci {}): ikisini de güncelle",
                remote_friend_common::PROTOCOL_VERSION,
                h.version
            )),
        )
        .await?;
        anyhow::bail!("protokol sürümü uyumsuz");
    }
    match auth::check_password(&h.password, &peer) {
        auth::Auth::Ok => {}
        auth::Auth::Bad => {
            tokio::time::sleep(Duration::from_millis(400)).await;
            write_packet(&mut wr, &Packet::Reject("şifre hatalı".into())).await?;
            anyhow::bail!("auth başarısız");
        }
        auth::Auth::Locked(secs) => {
            write_packet(&mut wr, &Packet::Reject(format!("çok fazla hatalı deneme; {secs} sn bekle"))).await?;
            anyhow::bail!("auth kilitli");
        }
    }
    if need_approval {
        write_packet(&mut wr, &Packet::WaitingForApproval).await?;
        let prompt_peer = peer.clone();
        let decision = tokio::task::spawn_blocking(move || approval::ask(&prompt_peer, false))
            .await
            .unwrap_or(approval::Decision::Deny);
        if decision == approval::Decision::Deny {
            write_packet(&mut wr, &Packet::Reject("host bağlantıyı reddetti".into())).await?;
            anyhow::bail!("operatör reddetti");
        }
    }
    write_packet(&mut wr, &Packet::Accept).await?;
    tracing::info!("{peer}: oturum başladı");
    #[cfg(target_os = "linux")]
    wayland::ensure_started();

    // Okuyucu: paketleri kanala aktarır (select! içinde kısmi okuma kaybolmasın).
    let (in_tx, mut in_rx) = mpsc::channel::<Packet>(256);
    let reader = tokio::spawn(async move {
        loop {
            match read_packet_limited(&mut rd, 1_000_000).await {
                Ok(p) => {
                    if in_tx.send(p).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    // Yazıcı: video kanalı küçük; ağ yavaşsa kareler kuyrukta değil kaynakta atlanır.
    let (video_tx, mut video_rx) = mpsc::channel::<Packet>(2);
    let writer = tokio::spawn(async move {
        while let Some(p) = video_rx.recv().await {
            if write_packet(&mut wr, &p).await.is_err() {
                break;
            }
        }
    });

    let mut frames = video::h264().subscribe();
    let mut flow = web::Flow::new(false);
    let result: Result<()> = loop {
        tokio::select! {
            frame = frames.recv() => match frame {
                Ok(f) => {
                    if !flow.admit(&f) {
                        continue;
                    }
                    let packet = Packet::Video(VideoFrame {
                        seq: f.seq,
                        width: f.w,
                        height: f.h,
                        codec: VideoCodec::H264,
                        data: f.data.clone(),
                        timestamp_ms: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                    });
                    match video_tx.try_send(packet) {
                        Ok(()) => flow.sent(&f),
                        Err(mpsc::error::TrySendError::Full(_)) => flow.on_lagged(),
                        Err(mpsc::error::TrySendError::Closed(_)) => break Ok(()),
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => flow.on_lagged(),
                Err(broadcast::error::RecvError::Closed) => break Ok(()),
            },
            pkt = in_rx.recv() => match pkt {
                None => break Ok(()),
                Some(Packet::Ack { seq }) => flow.on_ack(seq),
                Some(Packet::Input(ev)) => {
                    let ev = match ev {
                        remote_friend_common::InputEvent::MouseMove { x, y } => match flow.map_point(x, y) {
                            Some((x, y)) => remote_friend_common::InputEvent::MouseMove { x, y },
                            None => continue,
                        },
                        other => other,
                    };
                    if let Err(e) = input::apply(ev) {
                        tracing::debug!("girdi hatası: {e:#}");
                    }
                }
                Some(Packet::File(chunk)) => {
                    if let Err(e) = files::save_chunk(chunk) {
                        tracing::warn!("dosya hatası: {e:#}");
                    }
                }
                Some(_) => {}
            },
        }
    };
    reader.abort();
    writer.abort();
    input::release_all();
    tracing::info!("{peer}: oturum kapandı");
    result
}

// ---- internet uplink (VPS rendezvous) ----

type BoxRd = Box<dyn tokio::io::AsyncRead + Unpin + Send>;
type BoxWr = Box<dyn tokio::io::AsyncWrite + Unpin + Send>;

#[derive(Clone)]
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
        .map(|s| s.trim().to_string())
        .or_else(|| if cfg.server.trim().is_empty() { None } else { Some(cfg.server.clone()) })?;
    let addr = if addr.contains(':') { addr } else { format!("{addr}:{}", remote_friend_common::RENDEZVOUS_PORT) };
    let fp = std::env::var("RF_RV_FP")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| if cfg.fp.trim().is_empty() || cfg.server != addr { None } else { Some(cfg.fp.clone()) });
    let sni = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or("rv").to_string();
    Some(RvTarget { addr, fp, sni })
}

async fn rv_connect(t: &RvTarget) -> Result<(BoxRd, BoxWr)> {
    if let Some(fp) = &t.fp {
        let s = remote_friend_common::tls::tls_connect(&t.addr, &t.sni, Some(fp.clone())).await?;
        let (r, w) = tokio::io::split(s);
        return Ok((Box::new(r), Box::new(w)));
    }
    // Parmak izi yoksa sistem kök sertifikalarıyla dene (domain + LE kuruluysa sorunsuz).
    match remote_friend_common::tls::tls_connect(&t.addr, &t.sni, None).await {
        Ok(s) => {
            let (r, w) = tokio::io::split(s);
            Ok((Box::new(r), Box::new(w)))
        }
        Err(e) => {
            if std::env::var("RF_PLAIN_OK").map(|v| v == "1").unwrap_or(false) {
                tracing::warn!("!!! rendezvous DÜZ bağlanıyor (test modu)");
                let s = tokio::net::TcpStream::connect(&t.addr).await?;
                let (r, w) = s.into_split();
                return Ok((Box::new(r), Box::new(w)));
            }
            tracing::debug!("sistem sertifikasıyla doğrulanamadı ({e:#}); TOFU");
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

/// TOFU: ilk bağlanışta sunucu parmak izini sor, onaylanırsa kalıcı kaydet.
static TOFU_FP: OnceLock<std::sync::Mutex<Option<String>>> = OnceLock::new();
static TOFU_DECLINED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

async fn tofu_approve_server(addr: &str, sni: &str) -> Result<Option<String>> {
    let cache = TOFU_FP.get_or_init(|| std::sync::Mutex::new(None));
    if let Some(fp) = cache.lock().unwrap().clone() {
        return Ok(Some(fp));
    }
    if TOFU_DECLINED.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(None);
    }
    let fp = remote_friend_common::tls::fetch_server_fingerprint(addr, sni).await?;
    let question = format!(
        "*** Bu sunucuya ilk kez bağlanılıyor: {addr}\n*** Sertifika parmak izi: {fp}\n*** VPS kurulumunun sonunda yazan parmak iziyle aynı mı? Güvenip kaydedeyim mi? (E/H, 120 sn)"
    );
    let ok = tokio::task::spawn_blocking(move || approval::prompt(&question, Duration::from_secs(120)))
        .await
        .unwrap_or(false);
    if !ok {
        TOFU_DECLINED.store(true, std::sync::atomic::Ordering::Relaxed);
        return Ok(None);
    }
    let mut cfg = remote_friend_common::identity::load_rv_config();
    cfg.server = addr.to_string();
    cfg.fp = fp.clone();
    remote_friend_common::identity::save_rv_config(&cfg);
    *cache.lock().unwrap() = Some(fp.clone());
    println!("*** parmak izi kaydedildi, bir daha sorulmayacak");
    Ok(Some(fp))
}

async fn uplink_loop(target: RvTarget, id: String, secret: String, pc_name: String) {
    let mut delay = 2u64;
    let mut was_ok = false;
    loop {
        let started = std::time::Instant::now();
        match uplink_once(&target, &id, &secret, &pc_name, &mut was_ok).await {
            Ok(()) => tracing::warn!("sunucu bağlantısı kapandı; yeniden bağlanılıyor"),
            Err(e) => {
                if was_ok {
                    println!("!!! İnternet sunucusu bağlantısı koptu: {e:#}");
                } else {
                    tracing::warn!("sunucuya bağlanılamadı ({}): {e:#}", target.addr);
                }
                was_ok = false;
            }
        }
        if started.elapsed() > Duration::from_secs(60) {
            delay = 2;
        }
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(30);
    }
}

async fn uplink_once(target: &RvTarget, id: &str, secret: &str, pc_name: &str, was_ok: &mut bool) -> Result<()> {
    use remote_friend_common::io::{read_rv, write_rv};
    use remote_friend_common::RvMsg;

    let (mut rd, wr) = rv_connect(target).await?;
    let wr = Arc::new(Mutex::new(wr));
    {
        let mut g = wr.lock().await;
        write_rv(&mut *g, &RvMsg::Register { id: id.to_string(), name: pc_name.to_string(), secret: secret.to_string() })
            .await?;
        match tokio::time::timeout(Duration::from_secs(15), read_rv(&mut rd)).await.context("kayıt zaman aşımı")?? {
            RvMsg::RegisteredOk => {
                tracing::info!("rendezvous kaydı OK (ID: {})", remote_friend_common::format_id(id));
                if !*was_ok {
                    println!(">>> İnternet sunucusuna bağlandı ({}). Kod: {}", target.addr, remote_friend_common::format_id(id));
                }
                *was_ok = true;
            }
            RvMsg::RegisterError(m) => anyhow::bail!("kayıt reddedildi: {m}"),
            _ => anyhow::bail!("kayıt sırasında beklenmeyen yanıt"),
        }
    }
    let (out_tx, mut out_rx) = mpsc::channel::<RvMsg>(64);
    let wr2 = wr.clone();
    let heartbeat = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(20));
        loop {
            let msg = tokio::select! {
                _ = tick.tick() => RvMsg::Heartbeat,
                m = out_rx.recv() => match m { Some(m) => m, None => break },
            };
            let mut g = wr2.lock().await;
            if write_rv(&mut *g, &msg).await.is_err() {
                break;
            }
        }
    });
    let result = loop {
        let msg = match read_rv(&mut rd).await {
            Ok(m) => m,
            Err(e) => break Err(e),
        };
        if let RvMsg::ApprovalRequest { client, addr, kind, token, auth: candidate } = msg {
            let peer = format!("internet ({addr}, {kind})");
            let relay_auth = approval::split_relay_auth(candidate.as_deref().unwrap_or(""));
            let verdict = auth::check_password(relay_auth.password, &addr);
            let password_ok = matches!(verdict, auth::Auth::Ok);
            let resumed = password_ok && relay_auth.resume.is_some_and(approval::consume_resume);
            let trusted = password_ok && relay_auth.device.is_some_and(approval::is_trusted);
            let can_remember = kind.starts_with("web");
            if !matches!(verdict, auth::Auth::Ok) {
                tracing::warn!("{peer}: parola doğrulaması başarısız/kilitli; onay sorulmadı");
                let _ = out_tx.try_send(RvMsg::ApprovalAnswer { client, allow: false });
                continue;
            }
            let out_tx2 = out_tx.clone();
            let target2 = target.clone();
            let id = id.to_string();
            let secret = secret.to_string();
            tokio::spawn(async move {
                use approval::Decision;
                let decision = if resumed {
                    println!("*** {peer}: oturum yeniden bağlandı");
                    Decision::Once
                } else if trusted {
                    println!("*** {peer}: güvenilir cihaz, onaysız bağlandı");
                    Decision::Once
                } else {
                    let prompt_peer = peer.clone();
                    tokio::task::spawn_blocking(move || approval::ask(&prompt_peer, can_remember))
                        .await
                        .unwrap_or(Decision::Deny)
                };
                if decision != Decision::Deny {
                    let issued = (decision == Decision::Always).then(|| approval::trust_device(&peer));
                    dial_back(&target2, token, &kind, &id, &secret, issued).await;
                } else {
                    let _ = out_tx2.try_send(RvMsg::ApprovalAnswer { client, allow: false });
                }
            });
        }
    };
    heartbeat.abort();
    result
}

/// Onaylanan istemci için sunucuya geri bağlan, oturumu bu hat üzerinden yürüt.
async fn dial_back(target: &RvTarget, token: u128, kind: &str, id: &str, secret: &str, issued: Option<String>) {
    use remote_friend_common::io::write_rv;
    use remote_friend_common::RvMsg;
    let peer = format!("internet:{:08x}", token as u32);
    match rv_connect(target).await {
        Ok((rd, mut wr)) => {
            if write_rv(&mut wr, &RvMsg::ConnectBack { token, id: id.to_string(), secret: secret.to_string() })
                .await
                .is_err()
            {
                tracing::warn!("{peer}: ConnectBack yazılamadı");
                return;
            }
            tracing::info!("{peer}: dial-back kuruldu ({kind})");
            match kind {
                "web-jpeg" => web::session_kmsg(rd, wr, true, issued).await,
                "web" => web::session_kmsg(rd, wr, false, issued).await,
                _ => {
                    if let Err(e) = session_native(rd, wr, peer.clone(), false).await {
                        tracing::warn!("{peer}: {e:#}");
                    }
                }
            }
        }
        Err(e) => tracing::warn!("{peer}: dial-back bağlanamadı: {e:#}"),
    }
}
