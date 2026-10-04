//! Networking for the viewer: connect to a host (LAN or via the relay), decode video,
//! send input. Runs on its own thread with a small tokio runtime.

use anyhow::{Context, Result};
use remote_friend_common::{Handshake, Packet, PROTOCOL_VERSION};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc::{channel, Receiver};

/// A computer found on the local network
#[derive(Clone)]
pub(crate) struct LanEntry {
    pub ip: String,
    pub name: String,
    pub port: u16,
    pub last_seen: Instant,
}

pub(crate) struct Shared {
    pub texture: Option<(egui::ColorImage, u32, u32)>, // latest frame
    pub status: String,
    pub host_w: u32,
    pub host_h: u32,
    pub fps: f32,
    /// Trust on first use: (server, fingerprint) waiting for the user's answer.
    pub tofu_pending: Option<(String, String)>,
    pub tofu_answer: Option<bool>,
    /// Connection failed or was rejected (viewer shows an error screen).
    pub failed: bool,
    /// Play the remote computer's sound (sent to the host when the session starts).
    pub sound: bool,
    /// Trusted-device token to log in with (no password needed).
    pub device: Option<String>,
    /// The computer chose "Always allow": save this token for next time.
    pub issued_device: Option<String>,
    /// The saved token was refused (the computer forgot this device).
    pub device_rejected: bool,
    /// From the welcome message: "windows", "macos" or "linux".
    pub host_os: String,
    pub perms: Perms,
    /// Text copied on the remote computer, to put on this computer's clipboard.
    pub clip_in: Option<String>,
    /// Short message for the toolbar (permission changes, files...).
    pub banner: Option<(String, Instant)>,
    /// File being sent: (name, bytes confirmed by the computer, total).
    pub upload: Option<(String, u64, u64)>,
    /// The user canceled the upload (the sending thread stops and tells the computer).
    pub cancel_upload: bool,
    /// Last folder listing of the remote computer (file browser).
    pub remote_dir: Option<RemoteDir>,
    /// File being downloaded from the remote computer.
    pub download: Option<Transfer>,
    /// Where the last download was saved on this computer.
    pub downloaded: Option<String>,
    /// The computer's user asks to control this computer (switch sides).
    pub switch_req: bool,
    /// The computer accepted switching sides: close this viewer.
    pub switch_ok: bool,
    pub quality: String,
    /// Round trip time (ms) measured with pings.
    pub rtt: u32,
    /// Screens of the remote computer: (index, label), and the one shown.
    pub monitors: Vec<(usize, String)>,
    pub monitor: usize,
    /// Traffic flows directly (peer-to-peer), not through the relay server.
    pub direct: bool,
    /// The computer accepted the login (the typed password can be remembered).
    pub login_ok: bool,
    /// The password was wrong (forget a saved one).
    pub wrong_password: bool,
    /// The computer draws its pointer into the picture (hide ours over it).
    pub cursor_in_video: bool,
}

/// A folder on the remote computer.
#[derive(Clone, Debug, Default)]
pub(crate) struct RemoteDir {
    pub path: String,
    pub parent: Option<String>,
    /// (name, is folder, size)
    pub entries: Vec<(String, bool, u64)>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Transfer {
    pub id: u64,
    pub name: String,
    pub got: u64,
    pub total: u64,
}

/// What the computer's user currently allows this viewer to do.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Perms {
    pub control: bool,
    pub sound: bool,
    pub files: bool,
    pub clipboard: bool,
}

impl Default for Perms {
    fn default() -> Self {
        Self { control: true, sound: true, files: true, clipboard: true }
    }
}

impl Shared {
    pub(crate) fn new(sound: bool, device: Option<String>) -> Self {
        Self {
            texture: None,
            status: "Connecting...".into(),
            host_w: 0,
            host_h: 0,
            fps: 0.0,
            tofu_pending: None,
            tofu_answer: None,
            failed: false,
            sound,
            device,
            issued_device: None,
            device_rejected: false,
            host_os: String::new(),
            perms: Perms::default(),
            clip_in: None,
            banner: None,
            upload: None,
            cancel_upload: false,
            remote_dir: None,
            download: None,
            downloaded: None,
            switch_req: false,
            switch_ok: false,
            quality: String::new(),
            rtt: 0,
            monitors: Vec::new(),
            monitor: 0,
            direct: false,
            login_ok: false,
            wrong_password: false,
            cursor_in_video: false,
        }
    }

    pub(crate) fn banner(&mut self, text: impl Into<String>) {
        self.banner = Some((text.into(), Instant::now()));
    }
}


pub(crate) async fn net_loop(
    host: &str,
    password: &str,
    shared: Arc<Mutex<Shared>>,
    rx_out: Receiver<Packet>,
    disconnect: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let socket = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::TcpStream::connect(host),
    )
    .await
    .map_err(|_| anyhow::anyhow!("connection timed out (computer offline or wrong address: {host})"))?
    .with_context(|| format!("cannot connect to {host}"))?;
    remote_friend_common::tune_tcp(&socket);
    let (rd, wr) = socket.into_split();
    run_session(rd, wr, password, shared, rx_out, disconnect).await
}

/// Internet path via the relay: TLS -> Hello(ID) -> the computer dials back -> the same
/// end-to-end encrypted session (the relay only forwards ciphertext).
pub(crate) async fn net_loop_rv(
    server: &str,
    fp: Option<String>,
    id: &str,
    password: &str,
    shared: Arc<Mutex<Shared>>,
    rx_out: Receiver<Packet>,
    disconnect: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    use remote_friend_common::io::{read_rv, write_rv};
    use remote_friend_common::RvMsg;
    shared.lock().unwrap().status = format!("Connecting to the server ({server})...");
    let sni = server.split(':').next().unwrap_or("rv").to_string();
    // 1. Try the pinned fingerprint or the system roots
    let mut pinned: Option<String> = fp;
    let mut tls_stream = None;
    if pinned.is_none() {
        match remote_friend_common::tls::tls_connect(server, &sni, None).await {
            Ok(s) => tls_stream = Some(s),
            Err(_) => {} // falls through to trust-on-first-use / plain below
        }
    }
    // 2. Unknown server: show its fingerprint and save it if the user trusts it
    if tls_stream.is_none() && pinned.is_none() {
        if std::env::var("RF_PLAIN_OK").map(|v| v == "1").unwrap_or(false) {
            // test mode: unverified plain connection (below)
        } else if let Ok(fp_fetched) =
            remote_friend_common::tls::fetch_server_fingerprint(server, &sni).await
        {
            {
                let mut sh = shared.lock().unwrap();
                sh.status = "Waiting for you to confirm the server fingerprint...".into();
                sh.tofu_pending = Some((server.to_string(), fp_fetched.clone()));
                sh.tofu_answer = None;
            }
            // Wait for the user's decision (120 s)
            let mut answer = None;
            for _ in 0..600 {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                if disconnect.has_changed().unwrap_or(true) {
                    break;
                }
                if let Some(a) = shared.lock().unwrap().tofu_answer {
                    answer = Some(a);
                    break;
                }
            }
            {
                let mut sh = shared.lock().unwrap();
                sh.tofu_pending = None;
                sh.tofu_answer = None;
            }
            match answer {
                Some(true) => {
                    let mut cfg = remote_friend_common::identity::load_rv_config();
                    cfg.server = server.to_string();
                    cfg.fp = fp_fetched.clone();
                    remote_friend_common::identity::save_rv_config(&cfg);
                    pinned = Some(fp_fetched);
                }
                _ => {
                    shared.lock().unwrap().status = "The server was not trusted".into();
                    anyhow::bail!("server not trusted");
                }
            }
        }
    }
    let (rd, wr, mode): (BoxRd, BoxWr, &'static str) = if let Some(fp) = pinned {
        let s = remote_friend_common::tls::tls_connect(server, &sni, Some(fp)).await?;
        let (r, w) = tokio::io::split(s);
        (Box::new(r), Box::new(w), "TLS")
    } else if let Some(s) = tls_stream {
        let (r, w) = tokio::io::split(s);
        (Box::new(r), Box::new(w), "TLS-CA")
    } else if std::env::var("RF_PLAIN_OK").map(|v| v == "1").unwrap_or(false) {
        let s = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::net::TcpStream::connect(server),
        )
        .await
        .context("connection to the server timed out")??;
        remote_friend_common::tune_tcp(&s);
        let (r, w) = s.into_split();
        (Box::new(r), Box::new(w), "PLAIN")
    } else {
        anyhow::bail!("cannot connect securely to the server");
    };
    let _ = mode;
    let mut rd = rd;
    let mut wr = wr;
    // The password never goes to the server: it is proven end to end after the link is up.
    write_rv(
        &mut wr,
        &RvMsg::Hello { id: id.to_string(), auth: Some(remote_friend_common::e2e::RELAY_AUTH_MARKER.into()) },
    )
    .await?;
    loop {
        match read_rv(&mut rd).await? {
            RvMsg::Accepted => break,
            RvMsg::Rejected(m) => {
                let mut sh = shared.lock().unwrap();
                sh.status = format!("Rejected: {m}");
                sh.failed = true;
                drop(sh);
                anyhow::bail!("rejected: {m}");
            }
            RvMsg::WaitApproval => {
                shared.lock().unwrap().status = "Waiting for the remote computer to accept...".into();
            }
            _ => {}
        }
    }
    run_session(rd, wr, password, shared, rx_out, disconnect).await
}

type BoxRd = Box<dyn tokio::io::AsyncRead + Unpin + Send>;
type BoxWr = Box<dyn tokio::io::AsyncWrite + Unpin + Send>;

/// Handshake + approval + video/input loops (shared by all transports).
async fn run_session<R, W>(
    mut rd: R,
    mut wr: W,
    password: &str,
    shared: Arc<Mutex<Shared>>,
    mut rx_out: Receiver<Packet>,
    mut disconnect: tokio::sync::watch::Receiver<bool>,
) -> Result<()>
where
    R: tokio::io::AsyncReadExt + Unpin + Send + 'static,
    W: tokio::io::AsyncWriteExt + Unpin + Send + 'static,
{
    use remote_friend_common::e2e;
    use remote_friend_common::io::{read_blob_limited, read_packet_secure, write_blob, write_packet_secure};
    // 1. End-to-end encrypted handshake: both sides prove they know the password, or this
    //    app logs in as a trusted device with its token (no password needed).
    shared.lock().unwrap().status = "Checking the password (encrypted)...".into();
    let hello = tokio::time::timeout(std::time::Duration::from_secs(30), read_blob_limited(&mut rd, 1024))
        .await
        .context("the remote computer did not answer")??;
    let device = shared.lock().unwrap().device.clone().map(|t| t.to_ascii_lowercase());
    let device_login = device.is_some() && password.trim().is_empty() && e2e::parse_hello(&hello)?.device_login();
    let (hello, secret) = match (&device, device_login) {
        (Some(token), true) => {
            shared.lock().unwrap().status = "Logging in as a trusted device (encrypted)...".into();
            write_blob(&mut wr, &e2e::device_request(token)).await?;
            let second = read_blob_limited(&mut rd, 1024).await?;
            if !second.starts_with(b"RFE2") {
                let reason = e2e::viewer_result(&second).err().map(|e| e.to_string()).unwrap_or_default();
                let mut sh = shared.lock().unwrap();
                sh.device_rejected = true;
                sh.status = format!("Rejected: {reason}");
                sh.failed = true;
                drop(sh);
                anyhow::bail!("device not trusted");
            }
            (second, token.clone())
        }
        _ => (hello, password.to_string()),
    };
    let (reply, pending) = tokio::task::spawn_blocking(move || e2e::viewer_respond(&hello, &secret)).await??;
    write_blob(&mut wr, &reply.to_bytes()).await?;
    let result = read_blob_limited(&mut rd, 1024).await?;
    let secure = match e2e::viewer_result(&result).and_then(|mac| pending.finish(mac)) {
        Ok(c) => c,
        Err(e) => {
            let mut sh = shared.lock().unwrap();
            if device_login {
                sh.device_rejected = true;
            } else if e.to_string().contains("wrong password") {
                sh.wrong_password = true;
            }
            sh.status = format!("Rejected: {e}");
            sh.failed = true;
            drop(sh);
            anyhow::bail!("rejected: {e}");
        }
    };
    let e2e::Channel { mut tx, mut rx, direct } = secure;
    let hs = Packet::Handshake(Handshake {
        version: PROTOCOL_VERSION,
        password: String::new(),
        want_video: true,
        want_input: true,
    });
    write_packet_secure(&mut wr, &mut tx, &hs).await?;
    // 2. Login message: who we are, and whether the computer may switch sides with us.
    let me = remote_friend_host::snapshot();
    let auth = serde_json::json!({
        "t": "auth",
        "name": me.name,
        "switch": me.online && !me.id.is_empty(),
        // With a typed password, a saved token still skips the approval question.
        "device": if device_login { None } else { device.clone() },
    });
    write_packet_secure(&mut wr, &mut tx, &Packet::Control(auth.to_string())).await?;
    // Accept / Reject / WaitingForApproval
    loop {
        let resp = read_packet_secure(&mut rd, &mut rx, 64 * 1024).await?;
        match resp {
            Packet::Accept => {
                let mut sh = shared.lock().unwrap();
                sh.status = "Connected".into();
                sh.login_ok = true;
                break;
            }
            Packet::Reject(m) => {
                let mut sh = shared.lock().unwrap();
                sh.status = format!("Rejected: {m}");
                sh.failed = true;
                drop(sh);
                anyhow::bail!("rejected: {m}");
            }
            Packet::WaitingForApproval => {
                shared.lock().unwrap().status =
                    "Waiting for the remote computer to accept...".into();
            }
            _ => anyhow::bail!("unexpected reply"),
        }
    }
    let want_sound = shared.lock().unwrap().sound;
    let audio = serde_json::json!({"t": "audio", "on": want_sound});
    write_packet_secure(&mut wr, &mut tx, &Packet::Control(audio.to_string())).await?;

    // Writer: user input, frame acks (the host uses acks to bound latency) and a ping every
    // 2 s (keeps the session alive on a still screen and measures the round trip). Once a
    // direct (peer-to-peer) path is open, everything goes there; a ping still goes through the
    // relay every 10 s so the relay keeps the session.
    use remote_friend_host::p2p;
    let (ack_tx, mut ack_rx) = channel::<u64>(64);
    let (own_tx, mut own_rx) = channel::<Packet>(16);
    let (direct_set, mut direct_set_rx) = channel::<p2p::DirectOut>(2);
    let started = Instant::now();
    let writer = tokio::spawn(async move {
        let mut wr = wr;
        let mut ping = tokio::time::interval(std::time::Duration::from_secs(2));
        let mut relay_ping = tokio::time::interval(std::time::Duration::from_secs(10));
        let mut direct: Option<p2p::DirectOut> = None;
        let ping_packet = || {
            Packet::Control(serde_json::json!({"t": "ping", "ts": started.elapsed().as_millis() as u64}).to_string())
        };
        loop {
            let (p, relay_only) = tokio::select! {
                biased;
                Some(d) = direct_set_rx.recv() => {
                    direct = Some(d);
                    continue;
                }
                p = own_rx.recv() => match p { Some(p) => (p, false), None => break },
                p = rx_out.recv() => match p { Some(p) => (p, false), None => break },
                s = ack_rx.recv() => match s { Some(seq) => (Packet::Ack { seq }, false), None => break },
                _ = ping.tick() => (ping_packet(), false),
                _ = relay_ping.tick() => (ping_packet(), true),
            };
            if !relay_only {
                if let Some(d) = direct.as_mut() {
                    if let Ok(plain) = remote_friend_common::encode(&p) {
                        if d.send(&plain).await {
                            continue;
                        }
                    }
                    direct = None; // the direct path closed: back to the relay
                }
            }
            if write_packet_secure(&mut wr, &mut tx, &p).await.is_err() {
                break;
            }
        }
    });

    // Relay reader in its own task: a read cut short inside select! would lose bytes.
    let (relay_tx, mut relay_rx) = channel::<Result<Packet, String>>(64);
    let reader = tokio::spawn(async move {
        loop {
            let r = read_packet_secure(&mut rd, &mut rx, remote_friend_common::io::MAX_MSG).await;
            let failed = r.is_err();
            if relay_tx.send(r.map_err(|e| format!("{e:#}"))).await.is_err() || failed {
                break;
            }
        }
    });

    // Direct path: offer a data channel to the computer (inside this encrypted session).
    let (direct_in_tx, mut direct_in) = channel::<Packet>(256);
    let mut direct_keys = Some(direct);
    type OfferResult = Result<(String, tokio::sync::oneshot::Sender<String>, tokio::sync::oneshot::Receiver<p2p::Link>)>;
    let mut p2p_setup: Option<std::pin::Pin<Box<dyn std::future::Future<Output = OfferResult> + Send>>> =
        p2p::enabled().then(|| Box::pin(async { p2p::offer(p2p::stun_server().await).await }) as _);
    let mut p2p_answer: Option<tokio::sync::oneshot::Sender<String>> = None;
    let mut p2p_open: Option<tokio::sync::oneshot::Receiver<p2p::Link>> = None;

    // Receiver: video (fresh decoder per connection; P-frames need the chain)
    let mut last = Instant::now();
    let mut n = 0u32;
    let mut total = 0u64;
    let mut h264 = H264Dec::new()?;
    let mut first_frame = false;
    let mut player: Option<crate::audio::Player> = None;
    let mut player_failed = false;
    loop {
        let pkt = tokio::select! {
            _ = disconnect.changed() => {
                tracing::info!("disconnected by user");
                shared.lock().unwrap().status = "Disconnected".into();
                break;
            }
            r = relay_rx.recv() => match r {
                Some(Ok(p)) => p,
                Some(Err(e)) => {
                    let mut sh = shared.lock().unwrap();
                    sh.status = format!("Connection lost: {e}");
                    sh.failed = true;
                    break;
                }
                None => break,
            },
            Some(p) = direct_in.recv() => p,
            r = opt_future(&mut p2p_setup) => {
                p2p_setup = None;
                match r {
                    Ok((sdp, answer_tx, open)) => {
                        let offer = serde_json::json!({"t": "p2p_offer", "sdp": sdp});
                        let _ = own_tx.send(Packet::Control(offer.to_string())).await;
                        p2p_answer = Some(answer_tx);
                        p2p_open = Some(open);
                    }
                    Err(e) => tracing::info!("no direct connection: {e:#}"),
                }
                continue;
            }
            link = opt_future(&mut p2p_open) => {
                p2p_open = None;
                let (Ok(link), Some(keys)) = (link, direct_keys.take()) else {
                    tracing::info!("no direct path (staying on the relay)");
                    continue;
                };
                let p2p::Link { out, mut inc } = link;
                let e2e::DirectPair { tx: dtx, rx: mut drx } = keys;
                let (to_loop, sh) = (direct_in_tx.clone(), shared.clone());
                tokio::spawn(async move {
                    while let Some(ct) = inc.recv().await {
                        let Ok(plain) = drx.decrypt(&ct) else { break };
                        let Ok(p) = remote_friend_common::decode(&plain) else { break };
                        if to_loop.send(p).await.is_err() {
                            break;
                        }
                    }
                    sh.lock().unwrap().direct = false;
                });
                if direct_set.send(p2p::DirectOut { cipher: dtx, out }).await.is_ok() {
                    shared.lock().unwrap().direct = true;
                    tracing::info!("direct connection (peer-to-peer)");
                }
                continue;
            }
        };
        if let Packet::Control(text) = &pkt {
            if text.contains("\"p2p_answer\"") {
                if let (Some(tx), Ok(v)) = (p2p_answer.take(), serde_json::from_str::<serde_json::Value>(text)) {
                    let _ = tx.send(v.get("sdp").and_then(|x| x.as_str()).unwrap_or("").to_string());
                }
                continue;
            }
            if text.contains("\"p2p_off\"") {
                p2p_answer = None;
                p2p_open = None;
                continue;
            }
            if !on_control(text, &shared, started) {
                break;
            }
            continue;
        }
        if let Packet::File(chunk) = pkt {
            // A download from the remote computer (only while one is in progress).
            let wanted = shared.lock().unwrap().download.as_ref().is_some_and(|d| d.id == chunk.transfer_id);
            if !wanted {
                continue;
            }
            let got = chunk.offset + chunk.data.len() as u64;
            match tokio::task::spawn_blocking(move || remote_friend_host::save_download_chunk(chunk)).await {
                Ok(Ok(Some(path))) => {
                    let mut sh = shared.lock().unwrap();
                    let name = sh.download.take().map(|d| d.name).unwrap_or_default();
                    sh.banner(format!("✓ Downloaded {name} to {path}"));
                    sh.downloaded = Some(path);
                }
                Ok(Ok(None)) => {
                    if let Some(d) = shared.lock().unwrap().download.as_mut() {
                        d.got = got;
                    }
                }
                Ok(Err(e)) => {
                    let mut sh = shared.lock().unwrap();
                    sh.download = None;
                    sh.banner(format!("Download failed: {e:#}"));
                }
                Err(_) => {}
            }
            continue;
        }
        if let Packet::Audio(a) = &pkt {
            if player.is_none() && !player_failed {
                player = crate::audio::Player::start();
                player_failed = player.is_none();
            }
            if let Some(p) = player.as_mut() {
                p.push_opus(&a.data);
            }
            continue;
        }
        if let Packet::Video(f) = pkt {
            n += 1;
            total += 1;
            // Ack on receipt (even if decoding fails, the host's window must not stall).
            let _ = ack_tx.try_send(f.seq);
            let img = match f.codec {
                remote_friend_common::VideoCodec::H264 => match h264.decode_frame(&f.data) {
                    Ok(img) => img,
                    Err(e) => {
                        if total < 10 || total % 100 == 0 {
                            tracing::warn!("h264 frame skipped: {e:#}");
                        }
                        continue;
                    }
                },
                remote_friend_common::VideoCodec::Jpeg => decode_jpeg(&f.data)?,
                remote_friend_common::VideoCodec::RawRgba => continue, // unused
            };
            let mut s = shared.lock().unwrap();
            s.host_w = f.width;
            s.host_h = f.height;
            // Only the newest frame is kept; if the UI hasn't shown the previous one yet it is
            // dropped here, so no queue builds up.
            s.texture = Some((img, f.width, f.height));
            let el = last.elapsed().as_secs_f32();
            if el >= 1.0 {
                s.fps = n as f32 / el;
                n = 0;
                last = Instant::now();
            }
            if !first_frame {
                first_frame = true;
                // Don't lock the same mutex again here (deadlock on the first frame).
                s.status = "Connected".into();
            }
            drop(s);
            if total % 50 == 0 {
                tracing::info!("video: {total} frames received");
            }
        }
    }
    writer.abort();
    reader.abort();
    Ok(())
}

/// Awaits an optional future; pending forever when there is none.
async fn opt_future<F: std::future::Future + Unpin>(f: &mut Option<F>) -> F::Output {
    match f {
        Some(f) => f.await,
        None => std::future::pending().await,
    }
}

fn decode_jpeg(data: &[u8]) -> Result<egui::ColorImage> {
    let dynimg = image::load_from_memory(data).context("jpeg decode")?;
    let rgb = dynimg.to_rgba8();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let pixels = rgb.into_raw();
    Ok(egui::ColorImage::from_rgba_unmultiplied([w, h], &pixels))
}

/// One decoder per connection (P-frames depend on decoder state).
struct H264Dec {
    dec: openh264::decoder::Decoder,
}

impl H264Dec {
    fn new() -> Result<Self> {
        Ok(Self { dec: openh264::decoder::Decoder::new().context("cannot open the H.264 decoder")? })
    }

    fn decode_frame(&mut self, data: &[u8]) -> Result<egui::ColorImage> {
        use openh264::formats::YUVSource;
        let mut last: Option<(usize, usize, Vec<u8>)> = None;
        for nal in openh264::nal_units(data) {
            match self.dec.decode(nal) {
                Ok(Some(yuv)) => {
                    let (w, h) = yuv.dimensions();
                    let mut rgba = vec![0u8; w * h * 4];
                    yuv.write_rgba8(&mut rgba);
                    last = Some((w, h, rgba));
                }
                Ok(None) => {}
                Err(_) => {} // skip a broken NAL; the next IDR frame recovers
            }
        }
        let (w, h, rgba) = last.context("no decodable frame yet (waiting for a keyframe)")?;
        Ok(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
    }
}

/// A control message from the computer. Returns false when the session is over.
fn on_control(text: &str, shared: &Arc<Mutex<Shared>>, started: Instant) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return true };
    let str_of = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let mut sh = shared.lock().unwrap();
    match v.get("t").and_then(|x| x.as_str()) {
        Some("welcome") => {
            sh.host_os = str_of("os");
            sh.cursor_in_video = v.get("cursor").and_then(|x| x.as_bool()).unwrap_or(false);
            sh.quality = str_of("preset");
            let token = str_of("device");
            if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
                sh.issued_device = Some(token);
                sh.banner("This computer now trusts this device: next time no password is needed.");
            }
            if let Some(p) = v.get("perms") {
                sh.perms = parse_perms(p);
            }
            if let Some(m) = v.get("monitors") {
                parse_monitors(m, &mut sh);
            }
        }
        Some("monitors") => parse_monitors(&v, &mut sh),
        Some("perms") => {
            let new = parse_perms(&v);
            let old = sh.perms;
            sh.perms = new;
            let change = |allowed: bool, was: bool, what: &str| -> Option<String> {
                (allowed != was).then(|| {
                    format!("{what} {}", if allowed { "allowed again" } else { "turned off by the computer's user" })
                })
            };
            let msgs: Vec<String> = [
                change(new.control, old.control, "Mouse and keyboard"),
                change(new.sound, old.sound, "Sound"),
                change(new.files, old.files, "File transfer"),
                change(new.clipboard, old.clipboard, "Clipboard sync"),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !msgs.is_empty() {
                sh.banner(msgs.join(" · "));
            }
        }
        Some("clip") => sh.clip_in = Some(str_of("text")),
        Some("quality") => sh.quality = str_of("p"),
        Some("pong") => {
            if let Some(ts) = v.get("ts").and_then(|x| x.as_u64()) {
                sh.rtt = (started.elapsed().as_millis() as u64).saturating_sub(ts) as u32;
            }
        }
        Some("file_progress") => {
            let got = v.get("got").and_then(|x| x.as_u64());
            if let (Some(u), Some(got)) = (sh.upload.as_mut(), got) {
                u.1 = got;
            }
        }
        Some("file_done") => {
            sh.upload = None;
            let msg = format!("✓ Sent {} — saved on the remote computer as {}", str_of("name"), str_of("path"));
            sh.banner(msg);
        }
        Some("ls") => {
            let entries = v
                .get("entries")
                .and_then(|e| e.as_array())
                .map(|list| {
                    list.iter()
                        .map(|e| {
                            let name = e.get("n").and_then(|x| x.as_str()).unwrap_or("").to_string();
                            let dir = e.get("d").and_then(|x| x.as_bool()).unwrap_or(false);
                            (name, dir, e.get("s").and_then(|x| x.as_u64()).unwrap_or(0))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let error = v.get("error").and_then(|x| x.as_str()).map(String::from);
            let parent = v.get("parent").and_then(|x| x.as_str()).map(String::from);
            sh.remote_dir = Some(RemoteDir { path: str_of("path"), parent, entries, error });
        }
        Some("get_err") => {
            sh.download = None;
            let msg = format!("Download failed: {}", str_of("msg"));
            sh.banner(msg);
        }
        Some("file_err") => {
            sh.upload = None;
            let msg = format!("File not sent: {}", str_of("msg"));
            sh.banner(msg);
        }
        Some("switch_req") => sh.switch_req = true,
        Some("switch_ok") => sh.switch_ok = true,
        Some("switch_no") => sh.banner("The other side did not switch."),
        Some("ended") => {
            sh.status = str_of("msg");
            sh.failed = true;
            return false;
        }
        _ => {}
    }
    true
}

fn parse_monitors(v: &serde_json::Value, sh: &mut Shared) {
    let list = v.get("list").and_then(|l| l.as_array()).cloned().unwrap_or_default();
    sh.monitors = list
        .iter()
        .filter_map(|m| {
            let i = m.get("i")?.as_u64()? as usize;
            let name = m.get("name").and_then(|x| x.as_str()).unwrap_or("Screen");
            let (w, h) = (m.get("w").and_then(|x| x.as_u64()).unwrap_or(0), m.get("h").and_then(|x| x.as_u64()).unwrap_or(0));
            Some((i, format!("{} ({w}×{h})", name)))
        })
        .collect();
    sh.monitor = v.get("current").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
}

fn parse_perms(v: &serde_json::Value) -> Perms {
    let b = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(true);
    Perms { control: b("control"), sound: b("sound"), files: b("files"), clipboard: b("clipboard") }
}
