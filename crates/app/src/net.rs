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
    let (rd, wr) = socket.into_split();
    run_session(rd, wr, password, shared, rx_out, disconnect).await
}

/// Internet path via the relay: TLS -> Hello(ID + password) -> approval -> same session.
/// Note: TLS ends at the relay; whoever runs the relay can technically see the traffic.
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
    // 1. End-to-end encrypted handshake: both sides prove they know the password.
    shared.lock().unwrap().status = "Checking the password (encrypted)...".into();
    let hello = tokio::time::timeout(std::time::Duration::from_secs(30), read_blob_limited(&mut rd, 1024))
        .await
        .context("the remote computer did not answer")??;
    let pw = password.to_string();
    let (reply, pending) = tokio::task::spawn_blocking(move || e2e::viewer_respond(&hello, &pw)).await??;
    write_blob(&mut wr, &reply.to_bytes()).await?;
    let result = read_blob_limited(&mut rd, 1024).await?;
    let secure = match e2e::viewer_result(&result).and_then(|mac| pending.finish(mac)) {
        Ok(c) => c,
        Err(e) => {
            let mut sh = shared.lock().unwrap();
            sh.status = format!("Rejected: {e}");
            sh.failed = true;
            drop(sh);
            anyhow::bail!("rejected: {e}");
        }
    };
    let e2e::Channel { mut tx, mut rx } = secure;
    let hs = Packet::Handshake(Handshake {
        version: PROTOCOL_VERSION,
        password: String::new(),
        want_video: true,
        want_input: true,
    });
    write_packet_secure(&mut wr, &mut tx, &hs).await?;
    // Accept / Reject / WaitingForApproval
    loop {
        let resp = read_packet_secure(&mut rd, &mut rx, 64 * 1024).await?;
        match resp {
            Packet::Accept => {
                shared.lock().unwrap().status = "Connected".into();
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

    // Writer: user input and frame acks (the host uses acks to bound latency).
    let (ack_tx, mut ack_rx) = channel::<u64>(64);
    let writer = tokio::spawn(async move {
        let mut wr = wr;
        loop {
            let p = tokio::select! {
                biased;
                p = rx_out.recv() => match p { Some(p) => p, None => break },
                s = ack_rx.recv() => match s { Some(seq) => Packet::Ack { seq }, None => break },
            };
            if write_packet_secure(&mut wr, &mut tx, &p).await.is_err() {
                break;
            }
        }
    });

    // Receiver: video (fresh decoder per connection; P-frames need the chain)
    let mut last = Instant::now();
    let mut n = 0u32;
    let mut total = 0u64;
    let mut h264 = H264Dec::new()?;
    let mut first_frame = false;
    loop {
        tokio::select! {
            _ = disconnect.changed() => {
                tracing::info!("disconnected by user");
                shared.lock().unwrap().status = "Disconnected".into();
                break;
            }
            res = read_packet_secure(&mut rd, &mut rx, remote_friend_common::io::MAX_MSG) => {
                let pkt = match res {
                    Ok(p) => p,
                    Err(e) => {
                        {
                            let mut sh = shared.lock().unwrap();
                            sh.status = format!("Connection lost: {e:#}");
                            sh.failed = true;
                        }
                        break;
                    }
                };
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
                tracing::info!("video akiyor: toplam {total} frame");
            }
        }
            }
    };
    }
    writer.abort();
    Ok(())
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
                Err(_) => {} // bozuk NAL atla, IDR'de toparlar
            }
        }
        let (w, h, rgba) = last.context("no decodable frame yet (waiting for a keyframe)")?;
        Ok(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
    }
}
