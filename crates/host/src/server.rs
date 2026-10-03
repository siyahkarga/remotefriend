//! Host engine: shares this computer's screen and accepts remote input.
//! Serves native TCP + a browser page (HTTP/WebSocket) and optionally registers with a relay
//! (VPS) so it can be reached from anywhere by its 9-digit ID.
//!
//! Platforms: Linux Wayland (portal + PipeWire), Linux X11, Windows, macOS (xcap + enigo).

use anyhow::{Context, Result};
use remote_friend_common::{Packet, VideoCodec, VideoFrame};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, Mutex, Notify};

use crate::{approval, auth, files, input, status, video, web};
#[cfg(target_os = "linux")]
use crate::wayland;

static HOST_NAME: OnceLock<String> = OnceLock::new();

pub(crate) fn host_name() -> &'static str {
    HOST_NAME.get().map(String::as_str).unwrap_or("computer")
}

/// Windows: DPI awareness is required for correct capture size and mouse scaling.
fn enable_dpi_awareness() {
    #[cfg(windows)]
    {
        use windows::Win32::UI::HiDpi::*;
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }
}

fn env_port(name: &str, default: u16) -> u16 {
    std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

/// Address phones/browsers open: RF_WEB_URL, then the saved setting, else derived from the server.
pub(crate) fn relay_web_url(server: &str) -> String {
    if let Ok(u) = std::env::var("RF_WEB_URL") {
        if !u.trim().is_empty() {
            return u.trim().to_string();
        }
    }
    let cfg = remote_friend_common::identity::load_rv_config();
    if !cfg.web_url.trim().is_empty() {
        return cfg.web_url.trim().to_string();
    }
    let host = server.rsplit_once(':').map(|(h, _)| h).unwrap_or(server);
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        format!("https://{}.sslip.io", host.replace('.', "-"))
    } else {
        format!("https://{host}")
    }
}

/// Accept direct connections from the local network? (settings file or RF_LAN=1)
pub fn lan_enabled() -> bool {
    match std::env::var("RF_LAN").ok().as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => remote_friend_common::identity::load_host_settings().lan,
    }
}

/// Options for [`run`].
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    /// Replace the saved password with a new one.
    pub new_password: bool,
    /// Print the banner and answer prompts on the terminal (false for the desktop app).
    pub terminal: bool,
}

/// Runs the host until the process exits. Errors only on startup problems
/// (e.g. the native port is already used by another RemoteFriend).
pub async fn run(opts: RunOptions) -> Result<()> {
    enable_dpi_awareness();
    remote_friend_common::tls::init_crypto();

    let pc_name = hostname::get()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "computer".into());
    let _ = HOST_NAME.set(pc_name.clone());
    let (password, from_env) = auth::init(opts.new_password);

    // Wayland: ask for screen permission at startup (while the user is at the computer).
    #[cfg(target_os = "linux")]
    {
        wayland::init_runtime(tokio::runtime::Handle::current());
        if std::env::var("RF_TEST_PATTERN").map(|v| v != "1").unwrap_or(true) {
            wayland::ensure_started();
        }
    }

    // Local-network access is off by default: only the encrypted internet path is open.
    let lan = lan_enabled();
    let default_bind = if lan { "0.0.0.0" } else { "127.0.0.1" };
    let native_bind = std::env::var("RF_NATIVE_BIND").unwrap_or_else(|_| default_bind.into());
    let native_port = env_port("RF_NATIVE_PORT", remote_friend_common::DEFAULT_PORT);
    let addr = format!("{native_bind}:{native_port}");
    let listener = TcpListener::bind(&addr).await.with_context(|| {
        format!("cannot listen on {addr} (is RemoteFriend already running? set RF_NATIVE_PORT to use another port)")
    })?;

    let host_id = remote_friend_common::identity::load_or_create_host_id();
    let host_secret = remote_friend_common::identity::load_or_create_host_secret();
    let http_port = env_port("RF_HTTP_PORT", 33201);
    let lan_ip = local_ip_address::local_ip().map(|ip| ip.to_string()).unwrap_or_else(|_| "127.0.0.1".into());
    let web_bind = std::env::var("RF_WEB_BIND").unwrap_or_else(|_| default_bind.into());
    let rv = rv_target();
    let auto_accept = std::env::var("REMOTE_FRIEND_AUTO_ACCEPT").map(|v| v == "1").unwrap_or(false);

    status::update(|s| {
        s.name = pc_name.clone();
        s.id = host_id.clone();
        s.password = password.clone();
        s.password_from_env = from_env;
        s.server = rv.as_ref().map(|t| t.addr.clone());
        s.web_url = rv.as_ref().map(|t| relay_web_url(&t.addr));
        s.lan_url = if lan { format!("http://{lan_ip}:{http_port}") } else { String::new() };
        s.auto_accept = auto_accept;
    });

    if opts.terminal {
        print_banner(&pc_name, &host_id, &password, from_env, rv.as_ref(), &lan_ip, http_port, auto_accept, &native_bind);
    }

    {
        let (id_c, secret_c, pc_c) = (host_id.clone(), host_secret.clone(), pc_name.clone());
        tokio::spawn(async move { uplink_loop(id_c, secret_c, pc_c).await });
    }

    tokio::spawn({
        let name = pc_name.clone();
        async move {
            if let Err(e) = web::serve(format!("{web_bind}:{http_port}"), name).await {
                status::notice(format!("Local web page unavailable: {e:#}"));
            }
        }
    });

    if lan {
        std::thread::spawn({
            let pc_name = pc_name.clone();
            move || remote_friend_common::discovery::broadcast_loop(pc_name, native_port)
        });
    }

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
                tracing::warn!("native session limit reached; rejected {peer}");
                continue;
            }
        };
        tokio::spawn(async move {
            let _permit = permit;
            // The computer speaks first (encrypted handshake); anything else (e.g. a browser
            // hitting this port) fails the handshake and is closed.
            let _ = socket.set_nodelay(true);
            tracing::info!("native client: {peer}");
            if let Err(e) = handle_client(socket, peer.to_string()).await {
                tracing::warn!("native client {peer} closed: {e:#}");
            }
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn print_banner(
    pc_name: &str,
    host_id: &str,
    password: &str,
    from_env: bool,
    rv: Option<&RvTarget>,
    lan_ip: &str,
    http_port: u16,
    auto_accept: bool,
    native_bind: &str,
) {
    status::say("");
    status::say(&format!("  ┌────────────────────────────────────────────────────────┐"));
    status::say(&format!("    RemoteFriend  ·  {pc_name}"));
    status::say(&format!("  ├────────────────────────────────────────────────────────┤"));
    status::say(&format!("    Computer ID : {}", remote_friend_common::format_id(host_id)));
    if from_env {
        status::say(&format!("    Password    : (from REMOTE_FRIEND_PASS)"));
    } else {
        status::say(&format!("    Password    : {password}"));
    }
    match rv {
        Some(t) => status::say(&format!("    From anywhere: {}", relay_web_url(&t.addr))),
        None => status::say("    From anywhere: off (no relay server configured)"),
    }
    if native_bind == "127.0.0.1" || native_bind == "::1" {
        status::say("    Local network: off (enable in the app settings or with RF_LAN=1)");
    } else {
        status::say(&format!("    Local network: http://{lan_ip}:{http_port}"));
    }
    status::say(&format!("  └────────────────────────────────────────────────────────┘"));
    if auto_accept {
        status::say(&format!("  Approval is OFF (REMOTE_FRIEND_AUTO_ACCEPT=1): anyone with the password can connect."));
    } else {
        status::say(&format!("  New devices need approval here: A = allow once, P = allow permanently (device is remembered)."));
        let trusted = approval::trusted_count();
        if trusted > 0 {
            status::say(&format!("  Permanently allowed devices: {trusted}  (forget all: remote-friend-host --forget-devices)"));
        }
    }
    if !from_env {
        status::say(&format!("  The password is persistent; renew it with: remote-friend-host --new-password"));
    }
    if native_bind != "127.0.0.1" && native_bind != "::1" {
        status::say(&format!("  Note: the local network path is not encrypted; the internet path uses TLS/HTTPS."));
    }
    let prof = video::profile(video::preset());
    status::say(&format!(
        "  Video: {} ({} fps, up to {} px, {} kbit/s)",
        video::preset().name(),
        prof.fps,
        prof.max_w,
        prof.bitrate / 1000
    ));
    status::say("");
}

/// Settings changed (server address etc.): drop the relay connection and reconnect.
static RECONNECT: OnceLock<Notify> = OnceLock::new();

fn reconnect_signal() -> &'static Notify {
    RECONNECT.get_or_init(Notify::new)
}

pub fn reconnect_server() {
    TOFU_DECLINED.store(false, std::sync::atomic::Ordering::Relaxed);
    if let Some(c) = TOFU_FP.get() {
        *c.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    reconnect_signal().notify_waiters();
}

async fn handle_client(socket: TcpStream, peer: String) -> Result<()> {
    let (rd, wr) = socket.into_split();
    let source = peer.clone();
    session_native(rd, wr, peer, source).await
}

/// Encrypted handshake (host side) over framed blobs. Returns the channel, or None after
/// telling the viewer why it was refused (wrong password / locked).
pub(crate) async fn native_e2e_handshake<R, W>(
    rd: &mut R,
    wr: &mut W,
    source: &str,
) -> Result<Option<remote_friend_common::e2e::Channel>>
where
    R: tokio::io::AsyncReadExt + Unpin,
    W: tokio::io::AsyncWriteExt + Unpin,
{
    use remote_friend_common::e2e;
    use remote_friend_common::io::{read_blob_limited, write_blob};
    let (password, normalize) = auth::e2e_secret().context("password not initialized")?;
    let hs = e2e::HostHandshake::new(normalize);
    write_blob(wr, &hs.hello_bytes()).await?;
    let reply = tokio::time::timeout(Duration::from_secs(60), read_blob_limited(rd, 1024))
        .await
        .context("handshake timed out")??;
    let reply = e2e::ViewerReply::from_bytes(&reply)?;
    if let Some(secs) = auth::locked(source) {
        write_blob(wr, &e2e::host_result_err(2, &format!("too many wrong attempts; try again in {secs} s"))).await?;
        return Ok(None);
    }
    match tokio::task::spawn_blocking(move || hs.finish(&password, &reply)).await? {
        Ok((channel, mac_h)) => {
            auth::record(source, true);
            write_blob(wr, &e2e::host_result_ok(&mac_h)).await?;
            Ok(Some(channel))
        }
        Err(_) => {
            auth::record(source, false);
            tokio::time::sleep(Duration::from_millis(400)).await;
            write_blob(wr, &e2e::host_result_err(1, "wrong password")).await?;
            Ok(None)
        }
    }
}

/// Native (desktop app) session over LAN TCP or a relay dial-back. Everything after the
/// handshake is end-to-end encrypted; the relay only sees ciphertext.
async fn session_native<R, W>(mut rd: R, mut wr: W, peer: String, source: String) -> Result<()>
where
    R: tokio::io::AsyncReadExt + Unpin + Send + 'static,
    W: tokio::io::AsyncWriteExt + Unpin + Send + 'static,
{
    use remote_friend_common::io::{read_packet_secure, write_packet_secure};
    let Some(channel) = native_e2e_handshake(&mut rd, &mut wr, &source).await? else {
        anyhow::bail!("authentication failed");
    };
    let remote_friend_common::e2e::Channel { mut tx, mut rx } = channel;
    let pkt = tokio::time::timeout(Duration::from_secs(15), read_packet_secure(&mut rd, &mut rx, 64 * 1024))
        .await
        .context("handshake timed out")??;
    let Packet::Handshake(h) = pkt else {
        anyhow::bail!("unexpected first packet");
    };
    if h.version != remote_friend_common::PROTOCOL_VERSION {
        let msg = format!(
            "version mismatch (computer {}, app {}): update both",
            remote_friend_common::PROTOCOL_VERSION,
            h.version
        );
        write_packet_secure(&mut wr, &mut tx, &Packet::Reject(msg)).await?;
        anyhow::bail!("protocol version mismatch");
    }
    write_packet_secure(&mut wr, &mut tx, &Packet::WaitingForApproval).await?;
    let prompt_peer = peer.clone();
    let decision = tokio::task::spawn_blocking(move || approval::ask(&prompt_peer, false))
        .await
        .unwrap_or(approval::Decision::Deny);
    if decision == approval::Decision::Deny {
        write_packet_secure(&mut wr, &mut tx, &Packet::Reject("the remote computer denied the connection".into())).await?;
        anyhow::bail!("denied by operator");
    }
    write_packet_secure(&mut wr, &mut tx, &Packet::Accept).await?;
    tracing::info!("{peer}: session started");
    let _session = status::SessionGuard::new();
    #[cfg(target_os = "linux")]
    wayland::ensure_started();

    // Reader: forwards packets to a channel (partial reads must not be lost inside select!).
    let (in_tx, mut in_rx) = mpsc::channel::<Packet>(256);
    let reader = tokio::spawn(async move {
        while let Ok(p) = read_packet_secure(&mut rd, &mut rx, 1_000_000).await {
            if in_tx.send(p).await.is_err() {
                break;
            }
        }
    });
    // Writer: small video channel; on a slow network frames are skipped at the source, not queued.
    // Sound has its own small queue and goes first (late sound is dropped, not queued).
    let (video_tx, mut video_rx) = mpsc::channel::<Packet>(2);
    let (audio_tx, mut audio_rx) = mpsc::channel::<Packet>(16);
    let writer = tokio::spawn(async move {
        loop {
            let p = tokio::select! {
                biased;
                p = audio_rx.recv() => p,
                p = video_rx.recv() => p,
            };
            let Some(p) = p else { break };
            if write_packet_secure(&mut wr, &mut tx, &p).await.is_err() {
                break;
            }
        }
    });

    let mut frames = video::h264().subscribe();
    let mut flow = web::Flow::new(false);
    let mut sound: Option<tokio::sync::broadcast::Receiver<std::sync::Arc<crate::audio::AudioPacket>>> = None;
    let result: Result<()> = loop {
        tokio::select! {
            p = async {
                match sound.as_mut() {
                    Some(r) => r.recv().await,
                    None => std::future::pending().await,
                }
            } => match p {
                Ok(p) => {
                    let chunk = remote_friend_common::AudioChunk { seq: p.seq, data: p.opus.clone() };
                    let _ = audio_tx.try_send(Packet::Audio(chunk));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => sound = None,
            },
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
                Some(Packet::AudioOn(on)) => {
                    if !on {
                        sound = None;
                    } else if sound.is_none() {
                        sound = Some(crate::audio::subscribe());
                    }
                }
                Some(Packet::Input(ev)) => {
                    let ev = match ev {
                        remote_friend_common::InputEvent::MouseMove { x, y } => match flow.map_point(x, y) {
                            Some((x, y)) => remote_friend_common::InputEvent::MouseMove { x, y },
                            None => continue,
                        },
                        other => other,
                    };
                    if let Err(e) = input::apply(ev) {
                        tracing::debug!("input error: {e:#}");
                    }
                }
                Some(Packet::File(chunk)) => {
                    if let Err(e) = files::save_chunk(chunk) {
                        tracing::warn!("file transfer error: {e:#}");
                    }
                }
                Some(_) => {}
            },
        }
    };
    reader.abort();
    writer.abort();
    input::release_all();
    tracing::info!("{peer}: session closed");
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
    /// The server's registration key (empty if none).
    register_key: String,
}

/// None when no relay is configured (the host keeps working on the local network).
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
    let register_key = std::env::var("RF_REGISTER_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| cfg.register_key.clone())
        .trim()
        .to_string();
    Some(RvTarget { addr, fp, sni, register_key })
}

async fn rv_connect(t: &RvTarget) -> Result<(BoxRd, BoxWr)> {
    if let Some(fp) = &t.fp {
        let s = remote_friend_common::tls::tls_connect(&t.addr, &t.sni, Some(fp.clone())).await?;
        let (r, w) = tokio::io::split(s);
        return Ok((Box::new(r), Box::new(w)));
    }
    // No pinned fingerprint: try the system roots first (works with a domain + Let's Encrypt).
    match remote_friend_common::tls::tls_connect(&t.addr, &t.sni, None).await {
        Ok(s) => {
            let (r, w) = tokio::io::split(s);
            Ok((Box::new(r), Box::new(w)))
        }
        Err(e) => {
            if std::env::var("RF_PLAIN_OK").map(|v| v == "1").unwrap_or(false) {
                tracing::warn!("!!! connecting to the relay WITHOUT TLS (test mode)");
                let s = tokio::net::TcpStream::connect(&t.addr).await?;
                let (r, w) = s.into_split();
                return Ok((Box::new(r), Box::new(w)));
            }
            tracing::debug!("not verifiable with system roots ({e:#}); trust on first use");
            match tofu_approve_server(&t.addr, &t.sni).await? {
                Some(fp) => {
                    let s = remote_friend_common::tls::tls_connect(&t.addr, &t.sni, Some(fp)).await?;
                    let (r, w) = tokio::io::split(s);
                    Ok((Box::new(r), Box::new(w)))
                }
                None => anyhow::bail!("server certificate was not trusted"),
            }
        }
    }
}

/// Trust on first use: ask about the server fingerprint once and remember it.
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
    let (a, f) = (addr.to_string(), fp.clone());
    let ok = tokio::task::spawn_blocking(move || approval::trust_server(&a, &f)).await.unwrap_or(false);
    if !ok {
        TOFU_DECLINED.store(true, std::sync::atomic::Ordering::Relaxed);
        return Ok(None);
    }
    let mut cfg = remote_friend_common::identity::load_rv_config();
    cfg.server = addr.to_string();
    cfg.fp = fp.clone();
    remote_friend_common::identity::save_rv_config(&cfg);
    *cache.lock().unwrap() = Some(fp.clone());
    status::notice("Server fingerprint saved; you won't be asked again.");
    Ok(Some(fp))
}

async fn uplink_loop(id: String, secret: String, pc_name: String) {
    let mut delay = 2u64;
    let mut was_ok = false;
    loop {
        // Re-read settings each time: the desktop app may change the server.
        let Some(target) = rv_target() else {
            status::update(|s| {
                s.server = None;
                s.online = false;
                s.server_error = None;
                s.web_url = None;
            });
            tokio::select! {
                _ = reconnect_signal().notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
            continue;
        };
        status::update(|s| {
            s.server = Some(target.addr.clone());
            s.web_url = Some(relay_web_url(&target.addr));
        });
        let started = std::time::Instant::now();
        let result = tokio::select! {
            r = uplink_once(&target, &id, &secret, &pc_name, &mut was_ok) => r,
            _ = reconnect_signal().notified() => {
                delay = 0;
                Ok(())
            }
        };
        status::update(|s| s.online = false);
        match result {
            Ok(()) => tracing::info!("relay connection closed; reconnecting"),
            Err(e) => {
                let msg = format!("{e:#}");
                if was_ok {
                    status::notice(format!("Lost connection to the server: {msg}"));
                } else {
                    tracing::warn!("cannot reach the server ({}): {msg}", target.addr);
                }
                status::update(|s| s.server_error = Some(msg));
                was_ok = false;
            }
        }
        if started.elapsed() > Duration::from_secs(60) {
            delay = 2;
        }
        tokio::select! {
            _ = reconnect_signal().notified() => { delay = 2; }
            _ = tokio::time::sleep(Duration::from_secs(delay)) => {}
        }
        delay = (delay * 2).clamp(2, 30);
    }
}

async fn uplink_once(target: &RvTarget, id: &str, secret: &str, pc_name: &str, was_ok: &mut bool) -> Result<()> {
    use remote_friend_common::io::{read_rv, write_rv};
    use remote_friend_common::RvMsg;

    let (mut rd, wr) = rv_connect(target).await?;
    let wr = Arc::new(Mutex::new(wr));
    {
        let mut g = wr.lock().await;
        write_rv(
            &mut *g,
            &RvMsg::RegisterV2 {
                id: id.to_string(),
                name: pc_name.to_string(),
                secret: secret.to_string(),
                key: target.register_key.clone(),
            },
        )
            .await?;
        match tokio::time::timeout(Duration::from_secs(15), read_rv(&mut rd)).await.context("registration timed out")?? {
            RvMsg::RegisteredOk => {
                tracing::info!("registered with the relay (ID: {})", remote_friend_common::format_id(id));
                if !*was_ok {
                    status::notice(format!("Online: reachable from anywhere via {}", target.addr));
                }
                status::update(|s| {
                    s.online = true;
                    s.server_error = None;
                });
                *was_ok = true;
            }
            RvMsg::RegisterError(m) => anyhow::bail!("registration rejected: {m}"),
            _ => anyhow::bail!("unexpected reply during registration"),
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
            let peer = if kind.starts_with("web") {
                format!("{addr} (via internet)")
            } else {
                format!("app {addr} (via internet)")
            };
            // Only end-to-end encrypted requests are accepted: the password is never sent to
            // the relay; it is proven inside the encrypted handshake after the dial-back.
            if candidate.as_deref() != Some(remote_friend_common::e2e::RELAY_AUTH_MARKER) {
                tracing::warn!("{peer}: unencrypted request refused (outdated page or app)");
                let _ = out_tx.try_send(RvMsg::ApprovalAnswer { client, allow: false });
                continue;
            }
            if let Some(secs) = auth::locked(&addr) {
                tracing::warn!("{peer}: locked for {secs} s after wrong passwords");
                let _ = out_tx.try_send(RvMsg::ApprovalAnswer { client, allow: false });
                continue;
            }
            let target2 = target.clone();
            let id = id.to_string();
            let secret = secret.to_string();
            tokio::spawn(async move {
                dial_back(&target2, token, &kind, &id, &secret, peer, addr).await;
            });
        }
    };
    heartbeat.abort();
    result
}

/// Connect back to the relay for a client and run the (encrypted) session over that link.
/// The password check and the approval happen inside the session.
async fn dial_back(target: &RvTarget, token: u128, kind: &str, id: &str, secret: &str, peer: String, source: String) {
    use remote_friend_common::io::write_rv;
    use remote_friend_common::RvMsg;
    match rv_connect(target).await {
        Ok((rd, mut wr)) => {
            if write_rv(&mut wr, &RvMsg::ConnectBack { token, id: id.to_string(), secret: secret.to_string() })
                .await
                .is_err()
            {
                tracing::warn!("{peer}: could not send ConnectBack");
                return;
            }
            tracing::info!("{peer}: dial-back established ({kind})");
            match kind {
                "web-jpeg" => web::session_kmsg(rd, wr, true, peer, source).await,
                "web" => web::session_kmsg(rd, wr, false, peer, source).await,
                _ => {
                    if let Err(e) = session_native(rd, wr, peer.clone(), source).await {
                        tracing::warn!("{peer}: {e:#}");
                    }
                }
            }
        }
        Err(e) => tracing::warn!("{peer}: dial-back failed: {e:#}"),
    }
}
