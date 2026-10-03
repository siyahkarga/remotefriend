//! Tarayıcı oturumları.
//!
//! - LAN: host'un kendi HTTP/WebSocket sunucusu (http://HOST:33201).
//! - İnternet: VPS rölesi üzerinden dial-back hattı (kmsg çerçeveleri).
//!
//! İki taşıma da aynı oturum çekirdeğini (`run_session`) kullanır: hoş geldin,
//! video akışı + akış kontrolü (kare onayı), girdi, kalite, dosya.

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
/// Tarayıcıdan gelen tek metin mesajı sınırı (dosya parçası base64 dahil).
const MAX_TEXT: usize = 400 * 1024;
/// Bu süre istemciden hiç mesaj gelmezse oturum kapanır (arka plana alınan telefon sekmesi
/// dahil; dönünce sayfa onaysız yeniden bağlanır).
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
        .with_context(|| format!("web bind hatası: {http_addr}"))?;
    tracing::info!("web sunucusu: {http_addr}");
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await
        .context("web serve")?;
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
        tracing::warn!("yerel WebSocket Origin reddedildi");
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

async fn handle_ws(socket: WebSocket, _permit: tokio::sync::OwnedSemaphorePermit, peer_ip: String) {
    let (mut sink, mut stream) = socket.split();

    // 1. hello (15 sn): {t:"hello", password, jpeg?, resume?}
    let hello = tokio::time::timeout(Duration::from_secs(15), stream.next()).await;
    let (password, want_jpeg, resume) = match hello {
        Ok(Some(Ok(Message::Text(t)))) if t.len() <= 4096 => match serde_json::from_str::<serde_json::Value>(&t) {
            Ok(v) => (
                v.get("password").and_then(|p| p.as_str()).unwrap_or("").chars().take(256).collect::<String>(),
                v.get("jpeg").and_then(|j| j.as_bool()).unwrap_or(false),
                v.get("resume").and_then(|r| r.as_str()).unwrap_or("").chars().take(64).collect::<String>(),
            ),
            Err(_) => (String::new(), false, String::new()),
        },
        _ => (String::new(), false, String::new()),
    };
    match crate::auth::check_password(&password, &peer_ip) {
        crate::auth::Auth::Ok => {}
        crate::auth::Auth::Bad => {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let _ = sink.send(Message::Text(reject_json("şifre hatalı"))).await;
            return;
        }
        crate::auth::Auth::Locked(secs) => {
            let _ = sink
                .send(Message::Text(reject_json(&format!("çok fazla hatalı deneme; {secs} sn sonra tekrar dene"))))
                .await;
            return;
        }
    }

    // 2. operatör onayı (kısa kopmadan dönen oturum için belirteç yeterli)
    let approved = if crate::approval::consume_resume(&resume) {
        println!("*** tarayıcı ({peer_ip}): oturum yeniden bağlandı");
        true
    } else {
        let _ = sink.send(Message::Text(r#"{"t":"wait"}"#.into())).await;
        let who = format!("tarayıcı (yerel ağ, {peer_ip})");
        tokio::task::spawn_blocking(move || crate::approval::ask(&who)).await.unwrap_or(false)
    };
    if !approved {
        let _ = sink.send(Message::Text(reject_json("host bağlantıyı reddetti"))).await;
        return;
    }
    tracing::info!("tarayıcı (LAN) kabul edildi (jpeg={want_jpeg})");

    // 3. taşıma görevleri
    let (inbox_tx, inbox) = mpsc::channel::<String>(256);
    let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<String>(64);
    let (video_tx, mut video_rx) = mpsc::channel::<Vec<u8>>(2);
    let writer = tokio::spawn(async move {
        loop {
            let msg = tokio::select! {
                biased;
                m = ctrl_rx.recv() => match m { Some(t) => Message::Text(t), None => break },
                m = video_rx.recv() => match m { Some(b) => Message::Binary(b), None => break },
            };
            if sink.send(msg).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let reader = tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(Message::Text(t)) => {
                    if t.len() > MAX_TEXT || inbox_tx.send(t).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Binary(_)) | Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {}
                Ok(Message::Close(_)) | Err(_) => break,
            }
        }
    });
    run_session(inbox, ctrl_tx, video_tx, want_jpeg, "tarayıcı (LAN)").await;
    reader.abort();
    let _ = writer.await;
}

/// VPS dial-back hattı üzerinden tarayıcı oturumu. kmsg tür 0 = video, 1 = JSON.
pub(crate) async fn session_kmsg<R, W>(mut rd: R, mut wr: W, jpeg: bool)
where
    R: tokio::io::AsyncReadExt + Unpin + Send + 'static,
    W: tokio::io::AsyncWriteExt + Unpin + Send + 'static,
{
    use remote_friend_common::io::{read_kmsg, write_kmsg};
    let (inbox_tx, inbox) = mpsc::channel::<String>(256);
    let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<String>(64);
    let (video_tx, mut video_rx) = mpsc::channel::<Vec<u8>>(2);
    let writer = tokio::spawn(async move {
        loop {
            let res = tokio::select! {
                biased;
                m = ctrl_rx.recv() => match m { Some(t) => write_kmsg(&mut wr, 1, t.as_bytes()).await, None => break },
                m = video_rx.recv() => match m { Some(b) => write_kmsg(&mut wr, 0, &b).await, None => break },
            };
            if res.is_err() {
                break;
            }
            let _ = wr.flush().await;
        }
        let _ = wr.shutdown().await;
    });
    let reader = tokio::spawn(async move {
        loop {
            match read_kmsg(&mut rd).await {
                Ok((1, payload)) if payload.len() <= MAX_TEXT => {
                    let Ok(t) = String::from_utf8(payload) else { break };
                    if inbox_tx.send(t).await.is_err() {
                        break;
                    }
                }
                Ok((1, _)) => {
                    tracing::warn!("röle: tarayıcı mesajı çok büyük; oturum kapatılıyor");
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    run_session(inbox, ctrl_tx, video_tx, jpeg, "tarayıcı (internet)").await;
    reader.abort();
    let _ = writer.await;
}

/// Akış kontrolü: istemcinin onaylamadığı kare sayısı/yaşı sınırlanır. Böylece
/// yavaş ağda kareler soket tamponlarında birikip saniyelerce gecikme oluşmaz;
/// fazlası atlanır, ardından temiz bir anahtar kare istenir.
pub(crate) struct Flow {
    inflight: VecDeque<(u64, Instant)>,
    ack_seen: bool,
    need_key: bool,
    jpeg: bool,
    last_key_req: Option<Instant>,
    /// En son gönderilen kare (girdi koordinatı dönüşümü için).
    pub last: Option<Arc<EncodedFrame>>,
}

const MAX_INFLIGHT_FRAMES: usize = 8;
const MAX_INFLIGHT_AGE: Duration = Duration::from_millis(700);
/// Bu kadar eski onaysız kare kayıp sayılır (istemci atmış olabilir); pencere kilitlenmez.
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

    /// İstemci çözücüsünü sıfırladı: onaylanmamış kareleri attı, yeni anahtar kare istiyor.
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

    /// Bu kare gönderilmeli mi?
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

    /// JPEG oturumunda atlanan kare varsa ekran durağanlaşınca da güncel kare gelsin.
    pub(crate) fn jpeg_catch_up(&mut self) {
        if self.jpeg && self.need_key && !self.window_full() {
            self.need_key = false;
            self.last_key_req = None;
            self.request_key();
        }
    }

    /// İstemci karesindeki noktayı yakalama uzayına çevir.
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
        // Bağlantı koparsa kısa süre içinde host onayı sormadan yeniden bağlanmak için.
        "resume": crate::approval::grant_resume(),
    });
    if ctrl.send(welcome.to_string()).await.is_err() {
        return;
    }
    let mut frames = subscribe(jpeg);
    let mut flow = Flow::new(jpeg);
    let mut files = FileTransfers::default();
    // İstemci 2 sn'de bir ping atar; uzun sessizlik = kopmuş bağlantı (yarım açık soket).
    let mut last_rx = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if last_rx.elapsed() > SESSION_IDLE {
                    tracing::info!("{peer}: {} sn yanıt yok, oturum kapatılıyor", SESSION_IDLE.as_secs());
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
                    tracing::debug!("{peer}: mesaj hatası: {e:#}");
                }
            }
        }
    }
    crate::input::release_all();
    tracing::info!("{peer}: oturum kapandı");
}

/// Tamamlanan/başarısız dosyaları istemciye bildirmek için isim takibi.
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
                // Başlıkta seq'in düşük 32 biti gider; tam değeri son gönderilenden kur.
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
                anyhow::bail!("dosya parçası çok büyük");
            }
            let id = v.get("transfer_id").and_then(|n| n.as_u64()).unwrap_or(0);
            let name: String = v.get("name").and_then(|x| x.as_str()).unwrap_or("dosya").chars().take(128).collect();
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
