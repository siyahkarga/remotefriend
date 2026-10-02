//! Tarayıcı client için HTTP + WebSocket sunucusu.
//! http://HOST:33201 açılır, kurulum gerekmez. Video H264 (WebCodecs), input JSON.

use anyhow::{Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::{header::{HOST, ORIGIN}, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use remote_friend_common::{FileChunk, InputEvent, MouseButton, RemoteKey};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

const PAGE: &str = remote_friend_common::webapp::WEBAPP;

#[derive(Clone)]
struct WsState {
    password: String,
    slots: Arc<tokio::sync::Semaphore>,
}

pub async fn serve(http_addr: String, password: String) -> Result<()> {
    let max_sessions = std::env::var("RF_MAX_WEB_SESSIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n: &usize| *n > 0)
        .unwrap_or(4);
    let app = Router::new()
        .route("/", get(page))
        .route("/ws", get(ws_handler))
        .with_state(WsState {
            password,
            slots: Arc::new(tokio::sync::Semaphore::new(max_sessions)),
        });
    let listener = tokio::net::TcpListener::bind(&http_addr)
        .await
        .with_context(|| format!("web bind hatası: {http_addr}"))?;
    tracing::info!("web sunucusu: {http_addr}");
    axum::serve(listener, app)
        .await
        .context("web serve")?;
    Ok(())
}


async fn page() -> Response {
    use axum::http::{HeaderName, HeaderValue};
    let mut response = Html(PAGE).into_response();
    let h = response.headers_mut();
    h.insert(HeaderName::from_static("cache-control"), HeaderValue::from_static("no-store"));
    h.insert(HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff"));
    h.insert(HeaderName::from_static("referrer-policy"), HeaderValue::from_static("no-referrer"));
    h.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static("default-src 'self'; img-src 'self' blob: data:; connect-src 'self' ws: wss:; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"),
    );
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
    ws.on_upgrade(move |socket| handle_ws(socket, state, permit)).into_response()
}

async fn ws_send_text(
    tx: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    s: &str,
) -> Result<()> {
    tx.lock().await.send(Message::Text(s.to_string())).await?;
    Ok(())
}

async fn handle_ws(socket: WebSocket, state: WsState, _permit: tokio::sync::OwnedSemaphorePermit) {
    let peer = "tarayıcı";
    let (sink, mut stream) = socket.split();
    let tx = Arc::new(Mutex::new(sink));

    // 1. hello (15 sn). WebCodecs'siz tarayıcı (ya da Firefox) JPEG ister.
    let hello = tokio::time::timeout(std::time::Duration::from_secs(15), stream.next()).await;
    let (password_ok, want_jpeg) = match hello {
        Ok(Some(Ok(Message::Text(t)))) if t.len() <= 4096 => match serde_json::from_str::<serde_json::Value>(&t) {
            Ok(v) => (
                super::constant_time_eq(v.get("password").and_then(|p| p.as_str()).unwrap_or(""), &state.password),
                v.get("jpeg").and_then(|j| j.as_bool()).unwrap_or(false),
            ),
            Err(_) => (false, false),
        },
        _ => (false, false),
    };
    if !password_ok {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let _ = ws_send_text(&tx, r#"{"t":"reject","msg":"şifre hatalı"}"#).await;
        return;
    }

    // 2. operatör onayı (şart!)
    let _ = ws_send_text(&tx, r#"{"t":"wait"}"#).await;
    let approved = tokio::task::spawn_blocking(|| super::ask_approval(peer))
        .await
        .unwrap_or(false);
    if !approved {
        let _ = ws_send_text(&tx, r#"{"t":"reject","msg":"host bağlantıyı reddetti"}"#).await;
        return;
    }
    if want_jpeg {
        let welcome = format!(r#"{{"t":"welcome","jpeg":true,"fps":{}}}"#, jpeg_fps());
        let _ = ws_send_text(&tx, &welcome).await;
    } else {
        let welcome = format!(r#"{{"t":"welcome","codec":"h264","fps":{}}}"#, super::target_fps());
        let _ = ws_send_text(&tx, &welcome).await;
    }
    tracing::info!("tarayıcı client kabul edildi (jpeg={want_jpeg})");

    // 3. video gönderici (H264 WebCodecs ya da düz JPEG)
    let tx2 = tx.clone();
    let video_task = tokio::spawn(async move {
        if want_jpeg {
            jpeg_loop(tx2).await;
            return;
        }
        let mut rx = super::video_sender().subscribe();
        loop {
            let frame = match rx.recv().await {
                Ok(frame) => frame,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            // başlık: w(u32 LE) + h(u32 LE) + Annex-B
            let mut msg = Vec::with_capacity(frame.data.len() + 8);
            msg.extend_from_slice(&frame.width.to_le_bytes());
            msg.extend_from_slice(&frame.height.to_le_bytes());
            msg.extend_from_slice(frame.data.as_ref());
            let mut g = tx2.lock().await;
            if g.send(Message::Binary(msg)).await.is_err() {
                break;
            }
        }
    });

    // 4. input alıcı
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(Message::Text(t)) => {
                if t.len() > 400 * 1024 {
                    tracing::warn!("web input mesajı çok büyük; bağlantı kapatılıyor");
                    break;
                }
                if let Err(e) = handle_json_input(&t) {
                    tracing::warn!("web input hatası: {e:#}");
                }
            }
            Ok(Message::Close(_)) => break,
            Err(_) => break,
            _ => {}
        }
    }
    video_task.abort();
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    tracing::info!("tarayıcı client ayrıldı ({ts})");
}

/// JPEG karesi kodla: w(u32 LE) + h(u32 LE) + JPEG baytları.
/// Kalite RF_JPEG_Q ile verilir (30-95).
fn encode_jpeg_frame(w: u32, h: u32, rgba: &[u8], quality: u8) -> Option<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    use image::RgbImage;
    let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|px| [px[0], px[1], px[2]]).collect();
    let img = RgbImage::from_raw(w, h, rgb)?;
    let mut jpeg = Vec::new();
    let mut enc = JpegEncoder::new_with_quality(&mut jpeg, quality);
    enc.encode_image(&img).ok()?;
    let mut msg = Vec::with_capacity(jpeg.len() + 8);
    msg.extend_from_slice(&w.to_le_bytes());
    msg.extend_from_slice(&h.to_le_bytes());
    msg.extend_from_slice(&jpeg);
    Some(msg)
}

fn jpeg_fps() -> u32 {
    std::env::var("RF_JPEG_FPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|fps: &u32| (2..=15).contains(fps))
        .unwrap_or(10)
}

fn jpeg_quality() -> u8 {
    // Kalite: RF_JPEG_Q (30-95, varsayılan 68). Yüksek = net yazı, daha fazla CPU/ağ.
    std::env::var("RF_JPEG_Q")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&q| (30..=95).contains(&q))
        .unwrap_or(68)
}

static JPEG_TX: OnceLock<tokio::sync::broadcast::Sender<Arc<Vec<u8>>>> = OnceLock::new();

/// JPEG fallback için de tek capture/encode hattı kullanılır. Her izleyici aynı
/// sıkıştırılmış kareye abone olur; yavaş izleyici eski kareleri biriktirmez.
fn jpeg_sender() -> &'static tokio::sync::broadcast::Sender<Arc<Vec<u8>>> {
    JPEG_TX.get_or_init(|| {
        let (tx, _) = tokio::sync::broadcast::channel::<Arc<Vec<u8>>>(2);
        let worker = tx.clone();
        std::thread::Builder::new()
            .name("rf-capture-jpeg".into())
            .spawn(move || {
                let quality = jpeg_quality();
                let period = Duration::from_micros(1_000_000 / jpeg_fps() as u64);
                let mut next = Instant::now();
                let mut errors = 0u32;
                loop {
                    if worker.receiver_count() == 0 {
                        std::thread::sleep(Duration::from_millis(100));
                        next = Instant::now();
                        continue;
                    }
                    match super::capture_rgba() {
                        Ok((w, h, rgba)) => match encode_jpeg_frame(w, h, &rgba, quality) {
                            Some(msg) => {
                                errors = 0;
                                let _ = worker.send(Arc::new(msg));
                            }
                            None => errors = errors.saturating_add(1),
                        },
                        Err(e) => {
                            errors = errors.saturating_add(1);
                            if errors <= 3 || errors % 60 == 0 {
                                tracing::warn!("global JPEG capture/encode hatası ({errors}): {e:#}");
                            }
                        }
                    }
                    next += period;
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    } else {
                        next = now;
                    }
                }
            })
            .expect("JPEG capture/encode thread başlatılamadı");
        tx
    })
}

/// WebCodecs'siz tarayıcılar için düz JPEG akışı (her kare bağımsız resim).
/// Başlık H264 ile aynı: w(u32 LE) + h(u32 LE) + JPEG baytları.
async fn jpeg_loop(tx: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>) {
    let mut rx = jpeg_sender().subscribe();
    loop {
        let msg = match rx.recv().await {
            Ok(msg) => msg,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        };
        let mut g = tx.lock().await;
        if g.send(Message::Binary(msg.as_ref().clone())).await.is_err() {
            break;
        }
    }
}
/// Dial-back hattı üzerinden web oturumu (VPS internet yolu).
/// Mesajlar kmsg çerçeveli: tür 0 = binary video, 1 = text JSON.
pub(crate) async fn session_kmsg<R, W>(rd: R, wr: W)
where
    R: tokio::io::AsyncReadExt + Unpin + Send + 'static,
    W: tokio::io::AsyncWriteExt + Unpin + Send + 'static,
{
    use remote_friend_common::io::{read_kmsg, write_kmsg};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let wr = Arc::new(Mutex::new(wr));
    let wr2 = wr.clone();
    let video_task = tokio::spawn(async move {
        let mut rx = super::video_sender().subscribe();
        loop {
            let frame = match rx.recv().await {
                Ok(frame) => frame,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let mut msg = Vec::with_capacity(frame.data.len() + 8);
            msg.extend_from_slice(&frame.width.to_le_bytes());
            msg.extend_from_slice(&frame.height.to_le_bytes());
            msg.extend_from_slice(frame.data.as_ref());
            let mut g = wr2.lock().await;
            if write_kmsg(&mut *g, 0, &msg).await.is_err() {
                break;
            }
        }
    });

    let mut rd = rd;
    loop {
        match read_kmsg(&mut rd).await {
            Ok((1, payload)) if payload.len() <= 400 * 1024 => {
                if let Ok(t) = String::from_utf8(payload) {
                    if let Err(e) = handle_json_input(&t) {
                        tracing::warn!("web input hatası: {e:#}");
                    }
                }
            }
            Ok((1, _)) => {
                tracing::warn!("kmsg web input mesajı çok büyük; oturum kapatılıyor");
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    video_task.abort();
    tracing::info!("kmsg web oturumu kapandı");
}

/// VPS internet yolunda WebCodecs'siz tarayıcı: kmsg üzerinden JPEG.
/// (Rendezvous welcome'da {"jpeg":true} gönderir, sayfa createImageBitmap ile çizer.)
pub(crate) async fn session_kmsg_jpeg<R, W>(rd: R, wr: W)
where
    R: tokio::io::AsyncReadExt + Unpin + Send + 'static,
    W: tokio::io::AsyncWriteExt + Unpin + Send + 'static,
{
    use remote_friend_common::io::{read_kmsg, write_kmsg};
    let wr = Arc::new(Mutex::new(wr));
    let wr2 = wr.clone();
    let video_task = tokio::spawn(async move {
        let mut rx = jpeg_sender().subscribe();
        loop {
            let msg = match rx.recv().await {
                Ok(msg) => msg,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let mut g = wr2.lock().await;
            if write_kmsg(&mut *g, 0, msg.as_ref()).await.is_err() {
                break;
            }
        }
    });

    let mut rd = rd;
    loop {
        match read_kmsg(&mut rd).await {
            Ok((1, payload)) if payload.len() <= 400 * 1024 => {
                if let Ok(t) = String::from_utf8(payload) {
                    if let Err(e) = handle_json_input(&t) {
                        tracing::warn!("web input hatası: {e:#}");
                    }
                }
            }
            Ok((1, _)) => {
                tracing::warn!("kmsg web input mesajı çok büyük; oturum kapatılıyor");
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    video_task.abort();
    tracing::info!("kmsg jpeg oturumu kapandı");
}

fn handle_json_input(t: &str) -> Result<()> {
    let v: serde_json::Value = serde_json::from_str(t)?;
    match v.get("t").and_then(|x| x.as_str()) {
        Some("mouse") => {
            let x = v.get("x").and_then(|n| n.as_u64()).unwrap_or(0).min(u32::MAX as u64) as u32;
            let y = v.get("y").and_then(|n| n.as_u64()).unwrap_or(0).min(u32::MAX as u64) as u32;
            super::apply_input(InputEvent::MouseMove { x, y })?;
        }
        Some("down") => {
            super::apply_input(InputEvent::MouseDown { button: map_btn_str(v.get("b").and_then(|x| x.as_str()).unwrap_or("left")) })?;
        }
        Some("up") => {
            super::apply_input(InputEvent::MouseUp { button: map_btn_str(v.get("b").and_then(|x| x.as_str()).unwrap_or("left")) })?;
        }
        Some("scroll") => {
            let dx = v.get("dx").and_then(|n| n.as_i64()).unwrap_or(0)
                .clamp(i32::MIN as i64, i32::MAX as i64) as i32;
            let dy = v.get("dy").and_then(|n| n.as_i64()).unwrap_or(0)
                .clamp(i32::MIN as i64, i32::MAX as i64) as i32;
            super::apply_input(InputEvent::Scroll { dx, dy })?;
        }
        Some("key") => {
            let code = v.get("code").and_then(|x| x.as_str()).unwrap_or("");
            let down = v.get("down").and_then(|x| x.as_bool()).unwrap_or(true);
            if let Some(k) = map_key_str(code) {
                super::apply_input(InputEvent::Key { key: k, down })?;
            }
        }
        Some("file") => {
            use base64::Engine;
            let data_b64 = v.get("data").and_then(|x| x.as_str()).unwrap_or("");
            if data_b64.len() > 400 * 1024 {
                anyhow::bail!("dosya parçası çok büyük");
            }
            let data = base64::engine::general_purpose::STANDARD.decode(data_b64)?;
            let chunk = FileChunk {
                transfer_id: v.get("transfer_id").and_then(|n| n.as_u64()).unwrap_or(0),
                name: v.get("name").and_then(|x| x.as_str()).unwrap_or("dosya")
                    .chars().take(128).collect(),
                offset: v.get("offset").and_then(|n| n.as_u64()).unwrap_or(0),
                total: v.get("total").and_then(|n| n.as_u64()).unwrap_or(0),
                data,
                last: v.get("last").and_then(|x| x.as_bool()).unwrap_or(false),
            };
            super::save_chunk(chunk)?;
        }
        _ => {}
    }
    Ok(())
}

fn map_btn_str(b: &str) -> MouseButton {
    match b {
        "right" => MouseButton::Right,
        "middle" => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

/// JS e.key -> RemoteKey. Tek karakter harf, gerisi isim.
fn map_key_str(code: &str) -> Option<RemoteKey> {
    if code.chars().count() == 1 {
        return code.chars().next().map(RemoteKey::Char);
    }
    Some(match code {
        "Enter" => RemoteKey::Enter,
        "Tab" => RemoteKey::Tab,
        "Backspace" => RemoteKey::Backspace,
        "Escape" => RemoteKey::Escape,
        "Delete" => RemoteKey::Delete,
        "Insert" => RemoteKey::Insert,
        "Home" => RemoteKey::Home,
        "End" => RemoteKey::End,
        "PageUp" => RemoteKey::PageUp,
        "PageDown" => RemoteKey::PageDown,
        "ArrowUp" => RemoteKey::Up,
        "ArrowDown" => RemoteKey::Down,
        "ArrowLeft" => RemoteKey::Left,
        "ArrowRight" => RemoteKey::Right,
        "F1" => RemoteKey::F1,
        "F2" => RemoteKey::F2,
        "F3" => RemoteKey::F3,
        "F4" => RemoteKey::F4,
        "F5" => RemoteKey::F5,
        "F6" => RemoteKey::F6,
        "F7" => RemoteKey::F7,
        "F8" => RemoteKey::F8,
        "F9" => RemoteKey::F9,
        "F10" => RemoteKey::F10,
        "F11" => RemoteKey::F11,
        "F12" => RemoteKey::F12,
        "Shift" => RemoteKey::Shift,
        "Control" => RemoteKey::Ctrl,
        "Alt" => RemoteKey::Alt,
        "Meta" => RemoteKey::Meta,
        "CapsLock" => RemoteKey::CapsLock,
        "NumLock" => RemoteKey::NumLock,
        "PrintScreen" => RemoteKey::PrintScreen,
        "Pause" => RemoteKey::Pause,
        " " => RemoteKey::Char(' '),
        _ => return None,
    })
}
