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
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};

use crate::video::{self, EncodedFrame};

const PAGE: &str = remote_friend_common::webapp::WEBAPP;
use crate::core::{AuthMsg, Core, Login, MAX_TEXT};
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
/// A trusted device may answer the first hello with {t:"e2e2d", dev} and log in with its
/// device key instead of the password (see e2e.rs).
async fn web_e2e_handshake(io: &mut impl TextIo, source: &str) -> Result<Option<(remote_friend_common::e2e::Channel, Login)>> {
    use remote_friend_common::e2e;
    let (password, normalize) = crate::auth::e2e_secret().context("password not initialized")?;
    let hello = |hs: &e2e::HostHandshake, mode: &str| {
        serde_json::json!({
            "t": "e2e1",
            "v": 1,
            "salt": b64(&hs.salt),
            "pub": b64(&hs.host_pub),
            "norm": hs.normalize,
            "iter": hs.iterations,
            "dev": true,
            "mode": mode,
        })
        .to_string()
    };
    let mut hs = e2e::HostHandshake::new(normalize);
    io.send_text(hello(&hs, "password")).await?;
    let Some(text) = io.recv_text(Duration::from_secs(90)).await else { return Ok(None) };
    let mut v: serde_json::Value = serde_json::from_str(&text).context("bad handshake message")?;
    let mut device = None;
    if v.get("t").and_then(|t| t.as_str()) == Some("e2e2d") {
        if let Some(secs) = crate::auth::locked(source) {
            io.send_text(reject_json(&format!("too many wrong attempts; try again in {secs} s"))).await?;
            return Ok(None);
        }
        let id = v.get("dev").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let Some(key) = crate::approval::device_login_key(&id) else {
            crate::auth::record(source, false);
            io.send_text(serde_json::json!({"t": "reject", "code": "unknown_device",
                "msg": "this device is no longer trusted by the remote computer; enter the password"}).to_string()).await?;
            return Ok(None);
        };
        hs = e2e::HostHandshake::for_device(key.salt);
        io.send_text(hello(&hs, "device")).await?;
        let Some(text) = io.recv_text(Duration::from_secs(60)).await else { return Ok(None) };
        v = serde_json::from_str(&text).context("bad handshake message")?;
        device = Some((id, key));
    }
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
    let result = match &device {
        Some((_, key)) => {
            let k = key.key;
            tokio::task::spawn_blocking(move || hs.finish_with_key(&k, &reply)).await?
        }
        None => tokio::task::spawn_blocking(move || hs.finish(&password, &reply)).await?,
    };
    match result {
        Ok((channel, mac_h)) => {
            crate::auth::record(source, true);
            io.send_text(serde_json::json!({"t": "e2e3", "mac": b64(&mac_h)}).to_string()).await?;
            let login = match device {
                Some((id, key)) => Login::Device { id, label: key.label, temporary: key.temporary },
                None => {
                    crate::auth::password_login();
                    Login::Password
                }
            };
            Ok(Some((channel, login)))
        }
        Err(_) => {
            crate::auth::record(source, false);
            tokio::time::sleep(Duration::from_millis(400)).await;
            io.send_text(reject_json(if device.is_some() { "device key rejected" } else { "wrong password" })).await?;
            Ok(None)
        }
    }
}

/// Plaintext of an encrypted browser message: [kind] + payload (0 = video, 1 = JSON, 2 = sound).
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

/// Outgoing queues of a browser session; the writer encrypts and sends them.
struct Outbox {
    ctrl: mpsc::Sender<String>,
    video: mpsc::Sender<Vec<u8>>,
    audio: mpsc::Sender<Vec<u8>>,
    /// Switches the writer to the direct (peer-to-peer) path.
    direct: mpsc::Sender<crate::p2p::DirectOut>,
    /// Downloads (JSON), sent only when nothing else is waiting.
    files: mpsc::Sender<String>,
}

struct OutboxRx {
    ctrl: mpsc::Receiver<String>,
    video: mpsc::Receiver<Vec<u8>>,
    audio: mpsc::Receiver<Vec<u8>>,
    files: mpsc::Receiver<String>,
    direct_rx: mpsc::Receiver<crate::p2p::DirectOut>,
    direct: Option<crate::p2p::DirectOut>,
}

fn outbox() -> (Outbox, OutboxRx) {
    let (ctrl, ctrl_rx) = mpsc::channel(64);
    // Small video queue: on a slow network frames are skipped at the source, not queued.
    let (video, video_rx) = mpsc::channel(2);
    let (audio, audio_rx) = mpsc::channel(16);
    let (direct, direct_rx) = mpsc::channel(2);
    let (files, files_rx) = mpsc::channel(4);
    (
        Outbox { ctrl, video, audio, direct, files },
        OutboxRx { ctrl: ctrl_rx, video: video_rx, audio: audio_rx, files: files_rx, direct_rx, direct: None },
    )
}

impl OutboxRx {
    /// Next message as (kind, payload): control first, then sound, then video.
    /// Kinds: 0 = video, 1 = JSON, 2 = sound. None when the session ended.
    async fn next(&mut self) -> Option<(u8, Vec<u8>)> {
        loop {
            tokio::select! {
                biased;
                Some(d) = self.direct_rx.recv() => self.direct = Some(d),
                m = self.ctrl.recv() => return m.map(|t| (1u8, t.into_bytes())),
                m = self.audio.recv() => return m.map(|b| (2u8, b)),
                m = self.video.recv() => return m.map(|b| (0u8, b)),
                Some(m) = self.files.recv() => return Some((1u8, m.into_bytes())),
            }
        }
    }

    /// Send through the direct path if one is open; false: use the relay.
    async fn send_direct(&mut self, kind: u8, payload: &[u8]) -> bool {
        let Some(d) = self.direct.as_mut() else { return false };
        let mut p = Vec::with_capacity(1 + payload.len());
        p.push(kind);
        p.extend_from_slice(payload);
        if d.send(&p).await {
            return true;
        }
        self.direct = None; // the direct path closed: back to the relay
        false
    }
}

/// Decrypts browser messages arriving on the direct path and hands them to the session.
fn web_direct_reader(to_session: mpsc::Sender<String>) -> crate::core::DirectHooksReader {
    Box::new(move |mut inc, mut rx| {
        tokio::spawn(async move {
            while let Some(ct) = inc.recv().await {
                match open_json(&mut rx, &ct) {
                    Ok(Some(t)) => {
                        if to_session.send(t).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        })
    })
}

/// WebSocket writer; encrypted when `tx` is set.
fn spawn_ws_writer(
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
    mut out: OutboxRx,
    mut tx: Option<remote_friend_common::e2e::Cipher>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some((kind, data)) = out.next().await {
            if tx.is_some() && out.send_direct(kind, &data).await {
                continue;
            }
            let msg = match tx.as_mut() {
                Some(c) => match seal(c, kind, &data) {
                    Ok(ct) => Message::Binary(ct),
                    Err(_) => break,
                },
                None if kind == 1 => Message::Text(String::from_utf8(data).unwrap_or_default()),
                None if kind == 2 => continue, // no sound on the unencrypted page
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
        let (channel, login) = match web_e2e_handshake(&mut io, &peer_ip).await {
            Ok(Some(c)) => c,
            Ok(None) => return,
            Err(e) => {
                tracing::debug!("{who}: handshake failed: {e:#}");
                return;
            }
        };
        let remote_friend_common::e2e::Channel { tx, rx, direct } = channel;
        let (inbox_tx, inbox) = mpsc::channel::<String>(256);
        let (out, out_rx) = outbox();
        let writer = spawn_ws_writer(io.sink, out_rx, Some(tx));
        let reader = spawn_ws_reader(io.stream, inbox_tx, Some(rx));
        secure_session(inbox, out, want_jpeg, &who, login, Some(direct)).await;
        reader.abort();
        let _ = writer.await;
        return;
    }

    // Legacy, unencrypted page (plain http on the local network, no WebCrypto).
    let field = |k: &str, n: usize| v.get(k).and_then(|x| x.as_str()).unwrap_or("").chars().take(n).collect::<String>();
    let (password, resume, device) = (field("password", 256), field("resume", 64), field("device", 64));
    match crate::auth::check_password(&password, &peer_ip) {
        crate::auth::Auth::Ok => crate::auth::password_login(),
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
    let auth = AuthMsg { device, resume, ..Default::default() };
    let decision = match crate::core::quick_decision(&Login::Password, &auth, &who) {
        Some(d) => d,
        None => {
            let _ = io.send_text(r#"{"t":"wait"}"#.into()).await;
            let w = who.clone();
            tokio::task::spawn_blocking(move || crate::approval::ask(&w, true)).await.unwrap_or(Decision::Deny)
        }
    };
    if decision == Decision::Deny {
        let _ = io.send_text(reject_json("the remote computer denied the connection")).await;
        crate::auth::session_ended(true, false);
        return;
    }
    let issued = (decision == Decision::Always).then(|| crate::approval::trust_device(&who));
    let (inbox_tx, inbox) = mpsc::channel::<String>(256);
    let (out, out_rx) = outbox();
    let writer = spawn_ws_writer(io.sink, out_rx, None);
    let reader = spawn_ws_reader(io.stream, inbox_tx, None);
    let handle = crate::sessions::register(&who, &who, crate::sessions::Via::Browser, false, true);
    run_session(inbox, out, want_jpeg, handle, issued, None).await;
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
    let (channel, login) = match web_e2e_handshake(&mut io, &source).await {
        Ok(Some(c)) => c,
        Ok(None) => return,
        Err(e) => {
            tracing::debug!("{peer}: handshake failed: {e:#}");
            return;
        }
    };
    let remote_friend_common::e2e::Channel { mut tx, mut rx, direct } = channel;
    let KmsgIo { mut rd, mut wr } = io;
    let (inbox_tx, inbox) = mpsc::channel::<String>(256);
    let (out, mut out_rx) = outbox();
    let writer = tokio::spawn(async move {
        while let Some((kind, data)) = out_rx.next().await {
            if out_rx.send_direct(kind, &data).await {
                continue;
            }
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
    secure_session(inbox, out, jpeg, &peer, login, Some(direct)).await;
    reader.abort();
    let _ = writer.await;
}

/// After the encrypted handshake: the browser sends {t:"auth", device?, resume?, name?};
/// decide about approval, then run the session.
async fn secure_session(
    mut inbox: mpsc::Receiver<String>,
    out: Outbox,
    jpeg: bool,
    peer: &str,
    login: Login,
    direct: Option<remote_friend_common::e2e::DirectPair>,
) {
    use crate::approval::Decision;
    let Ok(Some(first)) = tokio::time::timeout(Duration::from_secs(30), inbox.recv()).await else { return };
    let Some(auth) = AuthMsg::parse(&first) else { return };
    let label = crate::core::viewer_label(&login, &auth, peer);
    let decision = match crate::core::quick_decision(&login, &auth, &label) {
        Some(d) => d,
        None => {
            let _ = out.ctrl.send(r#"{"t":"wait"}"#.into()).await;
            let who = if label == peer { peer.to_string() } else { format!("{label} ({peer})") };
            tokio::task::spawn_blocking(move || crate::approval::ask(&who, true)).await.unwrap_or(Decision::Deny)
        }
    };
    if decision == Decision::Deny {
        let _ = out.ctrl.send(reject_json("the remote computer denied the connection")).await;
        crate::auth::session_ended(login.used_password(), false);
        tokio::time::sleep(Duration::from_millis(300)).await;
        return;
    }
    let issued = (decision == Decision::Always).then(|| crate::approval::trust_device(&label));
    let trusted = matches!(login, Login::Device { .. });
    let handle = crate::sessions::register(&label, peer, crate::sessions::Via::Browser, trusted, login.used_password());
    run_session(inbox, out, jpeg, handle, issued, direct).await;
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
    out: Outbox,
    jpeg: bool,
    handle: crate::sessions::SessionHandle,
    issued_device: Option<String>,
    direct: Option<remote_friend_common::e2e::DirectPair>,
) {
    #[cfg(target_os = "linux")]
    crate::wayland::ensure_started();
    let welcome = welcome_json(jpeg, issued_device, &handle);
    if out.ctrl.send(welcome.to_string()).await.is_err() {
        return;
    }
    let peer = handle.label();
    let mut frames = subscribe(jpeg);
    // Messages that arrive on the direct (peer-to-peer) path, once there is one.
    let (direct_in_tx, mut direct_in) = mpsc::channel::<String>(256);
    let hooks = direct.map(|keys| crate::core::DirectHooks {
        keys,
        set_out: out.direct.clone(),
        start_reader: web_direct_reader(direct_in_tx),
    });
    let mut core = Core::new(handle, Flow::new(jpeg), out.ctrl.clone(), hooks);
    // Downloads: file chunks become {t:"fdata"} messages on the lowest-priority lane.
    let (lane_tx, mut lane_rx) = mpsc::channel::<remote_friend_common::FileChunk>(4);
    core.set_file_lane(lane_tx);
    let files_out = out.files.clone();
    let lane = tokio::spawn(async move {
        use base64::Engine;
        while let Some(c) = lane_rx.recv().await {
            let msg = serde_json::json!({
                "t": "fdata", "id": c.transfer_id, "name": c.name, "offset": c.offset, "total": c.total,
                "data": base64::engine::general_purpose::STANDARD.encode(&c.data), "last": c.last,
            });
            if files_out.send(msg.to_string()).await.is_err() {
                break;
            }
        }
    });
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
                core.flow.jpeg_catch_up();
            }
            frame = frames.recv() => match frame {
                Ok(f) => {
                    if !core.flow.admit(&f) {
                        continue;
                    }
                    let bytes = remote_friend_common::web_frame(f.w, f.h, f.seq, f.key, f.jpeg, &f.data);
                    match out.video.try_send(bytes) {
                        Ok(()) => core.flow.sent(&f),
                        Err(mpsc::error::TrySendError::Full(_)) => core.flow.on_lagged(),
                        Err(mpsc::error::TrySendError::Closed(_)) => break,
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => core.flow.on_lagged(),
                Err(broadcast::error::RecvError::Closed) => break,
            },
            ev = core.next_event() => match ev {
                // A full queue means a slow link: late sound is useless, so it is dropped.
                crate::core::Event::Audio(p) => { let _ = out.audio.try_send(crate::audio::web_payload(&p, core.sound.pcm)); }
                crate::core::Event::Send(json) => { let _ = out.ctrl.try_send(json); }
                crate::core::Event::Kill => {
                    // Ended on purpose: the password changes right away.
                    core.handle.bye = true;
                    let ended = serde_json::json!({"t": "reject", "code": "ended", "msg": "the remote computer's user ended the session"});
                    let _ = out.ctrl.send(ended.to_string()).await;
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    break;
                }
                crate::core::Event::Nothing => {}
            },
            msg = inbox.recv() => {
                let Some(text) = msg else { break };
                last_rx = Instant::now();
                if let Err(e) = core.on_json(&text) {
                    tracing::debug!("{peer}: message error: {e:#}");
                }
            }
            Some(text) = direct_in.recv() => {
                last_rx = Instant::now();
                if let Err(e) = core.on_json(&text) {
                    tracing::debug!("{peer}: message error: {e:#}");
                }
            }
        }
    }
    lane.abort();
    crate::input::release_all();
    tracing::info!("{peer}: session closed");
}

/// First message of every session (browser and app).
pub(crate) fn welcome_json(jpeg: bool, issued_device: Option<String>, handle: &crate::sessions::SessionHandle) -> serde_json::Value {
    serde_json::json!({
        "t": "welcome",
        "v": remote_friend_common::PROTOCOL_VERSION,
        "codec": if jpeg { "jpeg" } else { "h264" },
        "jpeg": jpeg,
        "name": crate::host_name(),
        "os": crate::core::host_os(),
        "preset": video::preset().name(),
        "fps": video::profile(video::preset()).fps,
        // Lets the client reconnect shortly after a drop without asking the host for approval.
        "resume": crate::approval::grant_resume(),
        // If the operator chose "always": the device stores this and logs in with it from now on.
        "device": issued_device,
        // The computer can send its sound ({t:"audio", on, codec} turns it on).
        "audio": true,
        // The pointer is already part of the picture: the viewer doesn't draw its own.
        "cursor": video::cursor_in_video(),
        "perms": handle.perms().to_json(handle.blocked()),
        "monitors": video::monitors_json(),
        // Safe view: some windows (RemoteFriend, private apps) are hidden from viewers.
        "safe": !crate::privacy::full_view(),
    })
}
