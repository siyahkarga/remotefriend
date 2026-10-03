//! Browser sessions.
//!
//! - LAN: the host's own HTTP/WebSocket server (http://HOST:33201).
//! - Internet: dial-back link through the VPS relay (kmsg frames).
//!
//! Both transports share the same session core (`run_session`): welcome,
//! video stream + flow control (frame acks), input, quality, files.

use anyhow::{Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::{header::{HOST, ORIGIN}, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use remote_friend_common::{FileChunk, InputEvent, MouseButton};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};

use crate::video::{self, EncodedFrame};

const PAGE: &str = remote_friend_common::webapp::WEBAPP;
/// Size limit for a single text message from the browser (including base64 file chunks).
const MAX_TEXT: usize = 400 * 1024;
/// The session closes if no message arrives from the client for this long (including a phone
/// tab sent to the background; when it returns, the page reconnects without approval).
const SESSION_IDLE: Duration = Duration::from_secs(45);

#[derive(Clone)]
struct WsState {
    slots: Arc<tokio::sync::Semaphore>,
    name: String,
}

pub async fn serve(http_addr: String, name: String) -> Result<()> {
    let max_sessions = std::env::var("RF_MAX_WEB_SESSIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n: &usize| *n > 0)
        .unwrap_or(4);
    let app = Router::new()
        .route("/", get(page))
        .route("/info", get(info))
        .route("/ws", get(ws_handler))
        .with_state(WsState { slots: Arc::new(tokio::sync::Semaphore::new(max_sessions)), name });
    let listener = tokio::net::TcpListener::bind(&http_addr)
        .await
        .with_context(|| format!("cannot open web port {http_addr}"))?;
    tracing::info!("web server: {http_addr}");
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await
        .context("web server")?;
    Ok(())
}

fn security_headers(response: &mut Response) {
    let h = response.headers_mut();
    h.insert(HeaderName::from_static("cache-control"), HeaderValue::from_static("no-store"));
    h.insert(HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff"));
    h.insert(HeaderName::from_static("referrer-policy"), HeaderValue::from_static("no-referrer"));
    h.insert(HeaderName::from_static("x-frame-options"), HeaderValue::from_static("DENY"));
    h.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    h.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static("default-src 'none'; img-src 'self' blob: data:; connect-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; manifest-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"),
    );
}

async fn page() -> Response {
    let mut response = Html(PAGE).into_response();
    security_headers(&mut response);
    response
}

async fn info(State(state): State<WsState>) -> Response {
    let body = serde_json::json!({
        "role": "host",
        "name": state.name,
        "v": remote_friend_common::PROTOCOL_VERSION,
    });
    let mut response = axum::Json(body).into_response();
    security_headers(&mut response);
    response
}

fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(host) = headers.get(HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    if origin == format!("http://{host}") || origin == format!("https://{host}") {
        return true;
    }
    std::env::var("RF_ALLOWED_ORIGIN")
        .ok()
        .map(|list| list.split(',').any(|item| item.trim() == origin))
        .unwrap_or(false)
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<WsState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if !origin_allowed(&headers) {
        tracing::warn!("local WebSocket origin rejected");
        return StatusCode::FORBIDDEN.into_response();
    }
    let permit = match state.slots.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    ws.max_message_size(MAX_TEXT + 1024)
        .on_upgrade(move |socket| handle_ws(socket, permit, peer.ip().to_string()))
        .into_response()
}

fn reject_json(msg: &str) -> String {
    serde_json::json!({"t": "reject", "msg": msg}).to_string()
}

// ---- browser transports ----

/// Text messages before encryption is set up (hello / handshake).
trait TextIo {
    async fn send_text(&mut self, s: String) -> Result<()>;
    /// None when the connection closed or the timeout passed.
    async fn recv_text(&mut self, timeout: Duration) -> Option<String>;
}

struct WsIo {
    sink: futures_util::stream::SplitSink<WebSocket, Message>,
    stream: futures_util::stream::SplitStream<WebSocket>,
}

impl TextIo for WsIo {
    async fn send_text(&mut self, s: String) -> Result<()> {
        self.sink.send(Message::Text(s)).await?;
        Ok(())
    }

    async fn recv_text(&mut self, timeout: Duration) -> Option<String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, self.stream.next()).await {
                Ok(Some(Ok(Message::Text(t)))) if t.len() <= 64 * 1024 => return Some(t),
                Ok(Some(Ok(Message::Ping(_)))) | Ok(Some(Ok(Message::Pong(_)))) => continue,
                _ => return None,
            }
        }
    }
}

struct KmsgIo<R, W> {
    rd: R,
    wr: W,
}

impl<R, W> TextIo for KmsgIo<R, W>
where
    R: tokio::io::AsyncReadExt + Unpin + Send,
    W: tokio::io::AsyncWriteExt + Unpin + Send,
{
    async fn send_text(&mut self, s: String) -> Result<()> {
        remote_friend_common::io::write_kmsg(&mut self.wr, 1, s.as_bytes()).await?;
        self.wr.flush().await?;
        Ok(())
    }

    async fn recv_text(&mut self, timeout: Duration) -> Option<String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, remote_friend_common::io::read_kmsg(&mut self.rd)).await {
                Ok(Ok((1, payload))) if payload.len() <= 64 * 1024 => return String::from_utf8(payload).ok(),
                Ok(Ok((1, _))) => return None,
                Ok(Ok(_)) => continue,
                _ => return None,
            }
        }
    }
}

fn b64(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn unb64(v: &serde_json::Value, key: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let s = v.get(key)?.as_str()?;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// Encrypted handshake with a browser. Ok(None): refused (the browser was told why).
async fn web_e2e_handshake(io: &mut impl TextIo, source: &str) -> Result<Option<remote_friend_common::e2e::Channel>> {
    use remote_friend_common::e2e;
    let (password, normalize) = crate::auth::e2e_secret().context("password not initialized")?;
    let hs = e2e::HostHandshake::new(normalize);
    let hello = serde_json::json!({
        "t": "e2e1",
        "v": e2e::VERSION,
        "salt": b64(&hs.salt),
        "pub": b64(&hs.host_pub),
        "norm": hs.normalize,
        "iter": hs.iterations,
    });
    io.send_text(hello.to_string()).await?;
    let Some(text) = io.recv_text(Duration::from_secs(90)).await else { return Ok(None) };
    let v: serde_json::Value = serde_json::from_str(&text).context("bad handshake message")?;
    if v.get("t").and_then(|t| t.as_str()) != Some("e2e2") {
        anyhow::bail!("unexpected handshake message");
    }
    let reply = e2e::ViewerReply {
        viewer_pub: unb64(&v, "pub").context("missing key")?,
        mac: unb64(&v, "mac").context("missing proof")?,
    };
    if let Some(secs) = crate::auth::locked(source) {
        io.send_text(reject_json(&format!("too many wrong attempts; try again in {secs} s"))).await?;
        return Ok(None);
    }
    match tokio::task::spawn_blocking(move || hs.finish(&password, &reply)).await? {
        Ok((channel, mac_h)) => {
            crate::auth::record(source, true);
            io.send_text(serde_json::json!({"t": "e2e3", "mac": b64(&mac_h)}).to_string()).await?;
            Ok(Some(channel))
        }
        Err(_) => {
            crate::auth::record(source, false);
            tokio::time::sleep(Duration::from_millis(400)).await;
            io.send_text(reject_json("wrong password")).await?;
            Ok(None)
        }
    }
}

/// Plaintext of an encrypted browser message: [kind] + payload (0 = video, 1 = JSON).
fn seal(tx: &mut remote_friend_common::e2e::Cipher, kind: u8, payload: &[u8]) -> Result<Vec<u8>> {
    let mut p = Vec::with_capacity(1 + payload.len());
    p.push(kind);
    p.extend_from_slice(payload);
    tx.encrypt(&p)
}

fn open_json(rx: &mut remote_friend_common::e2e::Cipher, data: &[u8]) -> Result<Option<String>> {
    let p = rx.decrypt(data)?;
    match p.split_first() {
        Some((1, json)) if json.len() <= MAX_TEXT => Ok(Some(String::from_utf8(json.to_vec())?)),
        Some((1, _)) => anyhow::bail!("message too large"),
        _ => Ok(None),
    }
}

/// WebSocket writer: control messages first, then video; encrypted when `tx` is set.
fn spawn_ws_writer(
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
    mut ctrl_rx: mpsc::Receiver<String>,
    mut video_rx: mpsc::Receiver<Vec<u8>>,
    mut tx: Option<remote_friend_common::e2e::Cipher>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let (kind, data) = tokio::select! {
                biased;
                m = ctrl_rx.recv() => match m { Some(t) => (1u8, t.into_bytes()), None => break },
                m = video_rx.recv() => match m { Some(b) => (0u8, b), None => break },
            };
            let msg = match tx.as_mut() {
                Some(c) => match seal(c, kind, &data) {
                    Ok(ct) => Message::Binary(ct),
                    Err(_) => break,
                },
                None if kind == 1 => Message::Text(String::from_utf8(data).unwrap_or_default()),
                None => Message::Binary(data),
            };
            if sink.send(msg).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    })
}

fn spawn_ws_reader(
    mut stream: futures_util::stream::SplitStream<WebSocket>,
    inbox_tx: mpsc::Sender<String>,
    mut rx: Option<remote_friend_common::e2e::Cipher>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            let text = match (msg, rx.as_mut()) {
                (Ok(Message::Binary(b)), Some(c)) => match open_json(c, &b) {
                    Ok(Some(t)) => t,
                    Ok(None) => continue,
                    Err(_) => break, // tampered / out of order: drop the session
                },
                (Ok(Message::Text(t)), None) if t.len() <= MAX_TEXT => t,
                (Ok(Message::Close(_)), _) | (Err(_), _) => break,
                _ => continue,
            };
            if inbox_tx.send(text).await.is_err() {
                break;
            }
        }
    })
}

async fn handle_ws(socket: WebSocket, _permit: tokio::sync::OwnedSemaphorePermit, peer_ip: String) {
    let (sink, stream) = socket.split();
    let mut io = WsIo { sink, stream };

    // 1. hello (15 s): {t:"hello", e2e:true, jpeg?}  (legacy plaintext pages send the password)
    let Some(hello) = io.recv_text(Duration::from_secs(15)).await else { return };
    let v: serde_json::Value = serde_json::from_str(&hello).unwrap_or_default();
    let want_jpeg = v.get("jpeg").and_then(|j| j.as_bool()).unwrap_or(false);
    let who = format!("browser {peer_ip} (local network)");

    if v.get("e2e").and_then(|x| x.as_bool()).unwrap_or(false) {
        let channel = match web_e2e_handshake(&mut io, &peer_ip).await {
            Ok(Some(c)) => c,
            Ok(None) => return,
            Err(e) => {
                tracing::debug!("{who}: handshake failed: {e:#}");
                return;
            }
        };
        let remote_friend_common::e2e::Channel { tx, rx } = channel;
        let (inbox_tx, inbox) = mpsc::channel::<String>(256);
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<String>(64);
        let (video_tx, video_rx) = mpsc::channel::<Vec<u8>>(2);
        let writer = spawn_ws_writer(io.sink, ctrl_rx, video_rx, Some(tx));
        let reader = spawn_ws_reader(io.stream, inbox_tx, Some(rx));
        secure_session(inbox, ctrl_tx, video_tx, want_jpeg, &who).await;
        reader.abort();
        let _ = writer.await;
        return;
    }

    // Legacy, unencrypted page (plain http on the local network, no WebCrypto).
    let field = |k: &str, n: usize| v.get(k).and_then(|x| x.as_str()).unwrap_or("").chars().take(n).collect::<String>();
    let (password, resume, device) = (field("password", 256), field("resume", 64), field("device", 64));
    match crate::auth::check_password(&password, &peer_ip) {
        crate::auth::Auth::Ok => {}
        crate::auth::Auth::Bad => {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let _ = io.send_text(reject_json("wrong password")).await;
            return;
        }
        crate::auth::Auth::Locked(secs) => {
            let _ = io.send_text(reject_json(&format!("too many wrong attempts; try again in {secs} s"))).await;
            return;
        }
    }
    use crate::approval::Decision;
    let decision = if crate::approval::consume_resume(&resume) {
        Decision::Once
    } else if crate::approval::is_trusted(&device) {
        crate::status::notice(format!("{who}: trusted device connected"));
        Decision::Once
    } else {
        let _ = io.send_text(r#"{"t":"wait"}"#.into()).await;
        let w = who.clone();
        tokio::task::spawn_blocking(move || crate::approval::ask(&w, true)).await.unwrap_or(Decision::Deny)
    };
    if decision == Decision::Deny {
        let _ = io.send_text(reject_json("the remote computer denied the connection")).await;
        return;
    }
    let issued = (decision == Decision::Always).then(|| crate::approval::trust_device(&who));
    let (inbox_tx, inbox) = mpsc::channel::<String>(256);
    let (ctrl_tx, ctrl_rx) = mpsc::channel::<String>(64);
    let (video_tx, video_rx) = mpsc::channel::<Vec<u8>>(2);
    let writer = spawn_ws_writer(io.sink, ctrl_rx, video_rx, None);
    let reader = spawn_ws_reader(io.stream, inbox_tx, None);
    run_session(inbox, ctrl_tx, video_tx, want_jpeg, "browser (LAN, unencrypted)", issued).await;
    reader.abort();
    let _ = writer.await;
}

/// Browser session over the relay dial-back link (always end-to-end encrypted).
/// kmsg type 1 = handshake JSON, type 0 = encrypted messages.
pub(crate) async fn session_kmsg<R, W>(rd: R, wr: W, jpeg: bool, peer: String, source: String)
where
    R: tokio::io::AsyncReadExt + Unpin + Send + 'static,
    W: tokio::io::AsyncWriteExt + Unpin + Send + 'static,
{
    use remote_friend_common::io::{read_kmsg, write_kmsg};
    let mut io = KmsgIo { rd, wr };
    let channel = match web_e2e_handshake(&mut io, &source).await {
        Ok(Some(c)) => c,
        Ok(None) => return,
        Err(e) => {
            tracing::debug!("{peer}: handshake failed: {e:#}");
            return;
        }
    };
    let remote_friend_common::e2e::Channel { mut tx, mut rx } = channel;
    let KmsgIo { mut rd, mut wr } = io;
    let (inbox_tx, inbox) = mpsc::channel::<String>(256);
    let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<String>(64);
    let (video_tx, mut video_rx) = mpsc::channel::<Vec<u8>>(2);
    let writer = tokio::spawn(async move {
        loop {
            let (kind, data) = tokio::select! {
                biased;
                m = ctrl_rx.recv() => match m { Some(t) => (1u8, t.into_bytes()), None => break },
                m = video_rx.recv() => match m { Some(b) => (0u8, b), None => break },
            };
            let Ok(ct) = seal(&mut tx, kind, &data) else { break };
            if write_kmsg(&mut wr, 0, &ct).await.is_err() {
                break;
            }
            let _ = wr.flush().await;
        }
        let _ = wr.shutdown().await;
    });
    let reader = tokio::spawn(async move {
        loop {
            match read_kmsg(&mut rd).await {
                Ok((0, data)) => match open_json(&mut rx, &data) {
                    Ok(Some(t)) => {
                        if inbox_tx.send(t).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(_) => break,
                },
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    secure_session(inbox, ctrl_tx, video_tx, jpeg, &peer).await;
    reader.abort();
    let _ = writer.await;
}

/// After the encrypted handshake: the browser sends {t:"auth", device?, resume?};
/// decide about approval, then run the session.
async fn secure_session(
    mut inbox: mpsc::Receiver<String>,
    ctrl: mpsc::Sender<String>,
    video_out: mpsc::Sender<Vec<u8>>,
    jpeg: bool,
    peer: &str,
) {
    use crate::approval::Decision;
    let Ok(Some(first)) = tokio::time::timeout(Duration::from_secs(30), inbox.recv()).await else { return };
    let v: serde_json::Value = serde_json::from_str(&first).unwrap_or_default();
    if v.get("t").and_then(|t| t.as_str()) != Some("auth") {
        return;
    }
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let decision = if crate::approval::consume_resume(&field("resume")) {
        crate::status::notice(format!("{peer}: session resumed"));
        Decision::Once
    } else if crate::approval::is_trusted(&field("device")) {
        crate::status::notice(format!("{peer}: trusted device connected"));
        Decision::Once
    } else {
        let _ = ctrl.send(r#"{"t":"wait"}"#.into()).await;
        let p = peer.to_string();
        tokio::task::spawn_blocking(move || crate::approval::ask(&p, true)).await.unwrap_or(Decision::Deny)
    };
    if decision == Decision::Deny {
        let _ = ctrl.send(reject_json("the remote computer denied the connection")).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        return;
    }
    let issued = (decision == Decision::Always).then(|| crate::approval::trust_device(peer));
    run_session(inbox, ctrl, video_out, jpeg, peer, issued).await;
}

/// Flow control: the number/age of frames not yet acked by the client is capped, so
/// on a slow network frames don't pile up in socket buffers and add seconds of delay;
/// the excess is skipped and then a clean keyframe is requested.
pub(crate) struct Flow {
    inflight: VecDeque<(u64, Instant)>,
    ack_seen: bool,
    need_key: bool,
    jpeg: bool,
    last_key_req: Option<Instant>,
    /// Most recently sent frame (for input coordinate mapping).
    pub last: Option<Arc<EncodedFrame>>,
}

const MAX_INFLIGHT_FRAMES: usize = 8;
const MAX_INFLIGHT_AGE: Duration = Duration::from_millis(700);
/// An unacked frame this old counts as lost (the client may have dropped it), so the window never locks up.
const INFLIGHT_EXPIRE: Duration = Duration::from_millis(2000);

impl Flow {
    pub(crate) fn new(jpeg: bool) -> Self {
        let mut f = Self { inflight: VecDeque::new(), ack_seen: false, need_key: true, jpeg, last_key_req: None, last: None };
        f.request_key();
        f
    }

    fn request_key(&mut self) {
        if self.last_key_req.is_none_or(|t| t.elapsed() > Duration::from_millis(1000)) {
            self.last_key_req = Some(Instant::now());
            if self.jpeg {
                video::request_jpeg_refresh();
            } else {
                video::request_keyframe();
            }
        }
    }

    pub(crate) fn on_lagged(&mut self) {
        self.need_key = true;
        if !self.jpeg {
            video::report_congestion();
        }
        self.request_key();
    }

    pub(crate) fn on_ack(&mut self, seq: u64) {
        self.ack_seen = true;
        while self.inflight.front().is_some_and(|(s, _)| *s <= seq) {
            self.inflight.pop_front();
        }
    }

    /// The client reset its decoder: it dropped unacked frames and wants a new keyframe.
    pub(crate) fn want_key(&mut self) {
        self.inflight.clear();
        self.need_key = true;
        self.request_key();
    }

    fn window_full(&mut self) -> bool {
        while self.inflight.front().is_some_and(|(_, t)| t.elapsed() > INFLIGHT_EXPIRE) {
            self.inflight.pop_front();
        }
        self.ack_seen
            && (self.inflight.len() >= MAX_INFLIGHT_FRAMES
                || self.inflight.front().is_some_and(|(_, t)| t.elapsed() > MAX_INFLIGHT_AGE))
    }

    /// Should this frame be sent?
    pub(crate) fn admit(&mut self, f: &EncodedFrame) -> bool {
        if f.jpeg {
            if self.window_full() {
                self.need_key = true;
                return false;
            }
            return true;
        }
        if self.need_key && !f.key {
            self.request_key();
            return false;
        }
        if self.window_full() {
            self.need_key = true;
            video::report_congestion();
            self.request_key();
            return false;
        }
        true
    }

    pub(crate) fn sent(&mut self, f: &Arc<EncodedFrame>) {
        if f.key {
            self.need_key = false;
        }
        self.inflight.push_back((f.seq, Instant::now()));
        if self.inflight.len() > 120 {
            self.inflight.pop_front();
        }
        self.last = Some(f.clone());
    }

    /// If a JPEG session skipped frames, still deliver a fresh frame once the screen goes static.
    pub(crate) fn jpeg_catch_up(&mut self) {
        if self.jpeg && self.need_key && !self.window_full() {
            self.need_key = false;
            self.last_key_req = None;
            self.request_key();
        }
    }

    /// Map a point in the client's frame to capture space.
    pub(crate) fn map_point(&self, x: u32, y: u32) -> Option<(u32, u32)> {
        self.last.as_ref().map(|f| f.to_capture(x, y))
    }
}

fn subscribe(jpeg: bool) -> broadcast::Receiver<Arc<EncodedFrame>> {
    if jpeg { video::jpeg().subscribe() } else { video::h264().subscribe() }
}

async fn run_session(
    mut inbox: mpsc::Receiver<String>,
    ctrl: mpsc::Sender<String>,
    video_out: mpsc::Sender<Vec<u8>>,
    jpeg: bool,
    peer: &str,
    issued_device: Option<String>,
) {
    #[cfg(target_os = "linux")]
    crate::wayland::ensure_started();
    let welcome = serde_json::json!({
        "t": "welcome",
        "v": remote_friend_common::PROTOCOL_VERSION,
        "codec": if jpeg { "jpeg" } else { "h264" },
        "jpeg": jpeg,
        "name": crate::host_name(),
        "preset": video::preset().name(),
        "fps": video::profile(video::preset()).fps,
        // Lets the client reconnect shortly after a drop without asking the host for approval.
        "resume": crate::approval::grant_resume(),
        // If the operator chose "always": the browser stores this and later connections skip approval.
        "device": issued_device,
    });
    if ctrl.send(welcome.to_string()).await.is_err() {
        return;
    }
    let _session = crate::status::SessionGuard::new();
    let mut frames = subscribe(jpeg);
    let mut flow = Flow::new(jpeg);
    let mut files = FileTransfers::default();
    // The client pings every 2 s; a long silence means a dead connection (half-open socket).
    let mut last_rx = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if last_rx.elapsed() > SESSION_IDLE {
                    tracing::info!("{peer}: no reply for {} s, closing session", SESSION_IDLE.as_secs());
                    break;
                }
                flow.jpeg_catch_up();
            }
            frame = frames.recv() => match frame {
                Ok(f) => {
                    if !flow.admit(&f) {
                        continue;
                    }
                    let bytes = remote_friend_common::web_frame(f.w, f.h, f.seq, f.key, f.jpeg, &f.data);
                    match video_out.try_send(bytes) {
                        Ok(()) => flow.sent(&f),
                        Err(mpsc::error::TrySendError::Full(_)) => flow.on_lagged(),
                        Err(mpsc::error::TrySendError::Closed(_)) => break,
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => flow.on_lagged(),
                Err(broadcast::error::RecvError::Closed) => break,
            },
            msg = inbox.recv() => {
                let Some(text) = msg else { break };
                last_rx = Instant::now();
                if let Err(e) = handle_message(&text, &mut flow, &ctrl, &mut files) {
                    tracing::debug!("{peer}: message error: {e:#}");
                }
            }
        }
    }
    crate::input::release_all();
    tracing::info!("{peer}: session closed");
}

/// Tracks file names so completed/failed files can be reported to the client.
#[derive(Default)]
struct FileTransfers {
    names: std::collections::HashMap<u64, String>,
}

fn btn(v: &serde_json::Value) -> MouseButton {
    match v.get("b").and_then(|x| x.as_str()).unwrap_or("left") {
        "right" => MouseButton::Right,
        "middle" => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn num_u32(v: &serde_json::Value, k: &str) -> u32 {
    v.get(k).and_then(|n| n.as_f64()).unwrap_or(0.0).clamp(0.0, 1e6) as u32
}

fn num_i32(v: &serde_json::Value, k: &str) -> i32 {
    v.get(k).and_then(|n| n.as_f64()).unwrap_or(0.0).clamp(-1000.0, 1000.0) as i32
}

fn handle_message(t: &str, flow: &mut Flow, ctrl: &mpsc::Sender<String>, files: &mut FileTransfers) -> Result<()> {
    let v: serde_json::Value = serde_json::from_str(t)?;
    let input = |ev: InputEvent| crate::input::apply(ev);
    match v.get("t").and_then(|x| x.as_str()) {
        Some("mouse") => {
            if let Some((x, y)) = flow.map_point(num_u32(&v, "x"), num_u32(&v, "y")) {
                input(InputEvent::MouseMove { x, y })?;
            }
        }
        Some("down") => input(InputEvent::MouseDown { button: btn(&v) })?,
        Some("up") => input(InputEvent::MouseUp { button: btn(&v) })?,
        Some("scroll") => {
            let (dx, dy) = (num_i32(&v, "dx"), num_i32(&v, "dy"));
            if dx != 0 || dy != 0 {
                input(InputEvent::Scroll { dx, dy })?;
            }
        }
        Some("key") => {
            let code = v.get("code").and_then(|x| x.as_str()).unwrap_or("");
            let down = v.get("down").and_then(|x| x.as_bool()).unwrap_or(true);
            if let Some(key) = crate::input::key_from_web(code) {
                input(InputEvent::Key { key, down })?;
            }
        }
        Some("text") => {
            let s: String = v.get("s").and_then(|x| x.as_str()).unwrap_or("").chars().take(4096).collect();
            if !s.is_empty() {
                input(InputEvent::Text(s))?;
            }
        }
        Some("ack") => {
            if let Some(s) = v.get("s").and_then(|n| n.as_u64()) {
                // The header carries only the low 32 bits of seq; rebuild the full value from the last sent frame.
                let full = flow.last.as_ref().map_or(s, |f| (f.seq & !0xffff_ffff) | s);
                flow.on_ack(full);
            }
        }
        Some("kf") => flow.want_key(),
        Some("quality") => {
            if let Some(p) = v.get("p").and_then(|x| x.as_str()).and_then(video::Preset::from_name) {
                video::set_preset(p);
                let _ = ctrl.try_send(serde_json::json!({"t": "quality", "p": p.name()}).to_string());
            }
        }
        Some("ping") => {
            let ts = v.get("ts").cloned().unwrap_or(serde_json::Value::Null);
            let _ = ctrl.try_send(serde_json::json!({"t": "pong", "ts": ts}).to_string());
        }
        Some("file") => {
            use base64::Engine;
            let data_b64 = v.get("data").and_then(|x| x.as_str()).unwrap_or("");
            if data_b64.len() > MAX_TEXT {
                anyhow::bail!("file chunk too large");
            }
            let id = v.get("transfer_id").and_then(|n| n.as_u64()).unwrap_or(0);
            let name: String = v.get("name").and_then(|x| x.as_str()).unwrap_or("file").chars().take(128).collect();
            let chunk = FileChunk {
                transfer_id: id,
                name: name.clone(),
                offset: v.get("offset").and_then(|n| n.as_u64()).unwrap_or(0),
                total: v.get("total").and_then(|n| n.as_u64()).unwrap_or(0),
                data: base64::engine::general_purpose::STANDARD.decode(data_b64)?,
                last: v.get("last").and_then(|x| x.as_bool()).unwrap_or(false),
            };
            files.names.insert(id, name.clone());
            match crate::files::save_chunk(chunk) {
                Ok(Some(path)) => {
                    files.names.remove(&id);
                    let _ = ctrl.try_send(serde_json::json!({"t": "file_done", "id": id, "name": name, "path": path}).to_string());
                }
                Ok(None) => {}
                Err(e) => {
                    files.names.remove(&id);
                    let _ = ctrl.try_send(serde_json::json!({"t": "file_err", "id": id, "msg": format!("{e:#}")}).to_string());
                    return Err(e);
                }
            }
        }
        _ => {}
    }
    Ok(())
}
