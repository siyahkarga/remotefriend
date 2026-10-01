//! Tarayıcı client için HTTP + WebSocket sunucusu.
//! http://HOST:8080 açılır, kurulum gerekmez. Video H264 (WebCodecs), input JSON.

use anyhow::{Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use remote_friend_common::{FileChunk, InputEvent, MouseButton, RemoteKey};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

const PAGE: &str = remote_friend_common::webapp::WEBAPP;

#[derive(Clone)]
struct WsState {
    password: String,
}

pub async fn serve(http_addr: String, password: String) -> Result<()> {
    let app = Router::new()
        .route("/", get(|| async { Html(PAGE) }))
        .route("/ws", get(ws_handler))
        .with_state(WsState { password });
    let listener = tokio::net::TcpListener::bind(&http_addr)
        .await
        .with_context(|| format!("web bind hatası: {http_addr}"))?;
    tracing::info!("web sunucusu: {http_addr}");
    axum::serve(listener, app)
        .await
        .context("web serve")?;
    Ok(())
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<WsState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn ws_send_text(
    tx: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    s: &str,
) -> Result<()> {
    tx.lock().await.send(Message::Text(s.to_string())).await?;
    Ok(())
}

async fn handle_ws(socket: WebSocket, state: WsState) {
    let peer = "tarayıcı";
    let (sink, mut stream) = socket.split();
    let tx = Arc::new(Mutex::new(sink));

    // 1. hello (15 sn). WebCodecs'siz tarayıcı (ya da Firefox) JPEG ister.
    let hello = tokio::time::timeout(std::time::Duration::from_secs(15), stream.next()).await;
    let (password_ok, want_jpeg) = match hello {
        Ok(Some(Ok(Message::Text(t)))) => match serde_json::from_str::<serde_json::Value>(&t) {
            Ok(v) => (
                v.get("password").and_then(|p| p.as_str()).unwrap_or("") == state.password,
                v.get("jpeg").and_then(|j| j.as_bool()).unwrap_or(false),
            ),
            Err(_) => (false, false),
        },
        _ => (false, false),
    };
    if !password_ok {
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
        let _ = ws_send_text(&tx, r#"{"t":"welcome","jpeg":true}"#).await;
    } else {
        let _ = ws_send_text(&tx, r#"{"t":"welcome"}"#).await;
    }
    tracing::info!("tarayıcı client kabul edildi (jpeg={want_jpeg})");

    // 3. video gönderici (H264 WebCodecs ya da düz JPEG)
    let tx2 = tx.clone();
    let video_task = tokio::spawn(async move {
        if want_jpeg {
            jpeg_loop(tx2).await;
            return;
        }
        let mut h264 = super::H264Enc::new();
        let mut err_n = 0u32;
        loop {
            match super::capture_rgba() {
                Ok((w, h, rgba)) => match h264.encode_frame(&rgba, w, h) {
                    Ok(nal) => {
                        err_n = 0;
                        // başlık: w(u32 LE) + h(u32 LE) + Annex-B
                        let mut msg = Vec::with_capacity(nal.len() + 8);
                        msg.extend_from_slice(&w.to_le_bytes());
                        msg.extend_from_slice(&h.to_le_bytes());
                        msg.extend_from_slice(&nal);
                        let mut g = tx2.lock().await;
                        if g.send(Message::Binary(msg)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        err_n += 1;
                        tracing::warn!("h264 encode hatası ({err_n}): {e:#}");
                        if err_n > 30 {
                            h264 = super::H264Enc::new();
                            err_n = 0;
                        }
                    }
                },
                Err(e) => {
                    tracing::warn!("capture hatası: {e:#}");
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(super::FPS_MS)).await;
        }
    });

    // 4. input alıcı
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(Message::Text(t)) => {
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

/// WebCodecs'siz tarayıcılar için düz JPEG akışı (her kare bağımsız resim).
/// Başlık H264 ile aynı: w(u32 LE) + h(u32 LE) + JPEG baytları.
async fn jpeg_loop(tx: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>) {
    use image::codecs::jpeg::JpegEncoder;
    loop {
        match super::capture_rgba() {
            Ok((w, h, rgba)) => {
                let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|px| [px[0], px[1], px[2]]).collect();
                let mut jpeg = Vec::new();
                let mut enc = JpegEncoder::new_with_quality(&mut jpeg, 60);
                use image::RgbImage;
                match RgbImage::from_raw(w, h, rgb) {
                    Some(img) => {
                        let _ = enc.encode_image(&img);
                        let mut msg = Vec::with_capacity(jpeg.len() + 8);
                        msg.extend_from_slice(&w.to_le_bytes());
                        msg.extend_from_slice(&h.to_le_bytes());
                        msg.extend_from_slice(&jpeg);
                        let mut g = tx.lock().await;
                        if g.send(Message::Binary(msg)).await.is_err() {
                            break;
                        }
                    }
                    None => {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                }
            }
            Err(e) => {
                tracing::warn!("capture hatası: {e:#}");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(super::FPS_MS)).await;
    }
}
/// Mesajlar kmsg çerçeveli: tür 0 = binary video, 1 = text JSON.
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
        let mut h264 = super::H264Enc::new();
        let mut err_n = 0u32;
        loop {
            match super::capture_rgba() {
                Ok((w, h, rgba)) => match h264.encode_frame(&rgba, w, h) {
                    Ok(nal) => {
                        err_n = 0;
                        let mut msg = Vec::with_capacity(nal.len() + 8);
                        msg.extend_from_slice(&w.to_le_bytes());
                        msg.extend_from_slice(&h.to_le_bytes());
                        msg.extend_from_slice(&nal);
                        let mut g = wr2.lock().await;
                        if write_kmsg(&mut *g, 0, &msg).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        err_n += 1;
                        if err_n > 30 {
                            h264 = super::H264Enc::new();
                            err_n = 0;
                        } else {
                            tracing::warn!("h264 encode hatası: {e:#}");
                        }
                    }
                },
                Err(e) => {
                    tracing::warn!("capture hatası: {e:#}");
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(super::FPS_MS)).await;
        }
    });

    let mut rd = rd;
    loop {
        match read_kmsg(&mut rd).await {
            Ok((1, payload)) => {
                if let Ok(t) = String::from_utf8(payload) {
                    if let Err(e) = handle_json_input(&t) {
                        tracing::warn!("web input hatası: {e:#}");
                    }
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    video_task.abort();
    tracing::info!("kmsg web oturumu kapandı");
}

fn handle_json_input(t: &str) -> Result<()> {
    let v: serde_json::Value = serde_json::from_str(t)?;
    match v.get("t").and_then(|x| x.as_str()) {
        Some("mouse") => {
            let x = v.get("x").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
            let y = v.get("y").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
            super::apply_input(InputEvent::MouseMove { x, y })?;
        }
        Some("down") => {
            super::apply_input(InputEvent::MouseDown { button: map_btn_str(v.get("b").and_then(|x| x.as_str()).unwrap_or("left")) })?;
        }
        Some("up") => {
            super::apply_input(InputEvent::MouseUp { button: map_btn_str(v.get("b").and_then(|x| x.as_str()).unwrap_or("left")) })?;
        }
        Some("scroll") => {
            let dx = v.get("dx").and_then(|n| n.as_i64()).unwrap_or(0) as i32;
            let dy = v.get("dy").and_then(|n| n.as_i64()).unwrap_or(0) as i32;
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
            let data = base64::engine::general_purpose::STANDARD.decode(data_b64)?;
            let chunk = FileChunk {
                transfer_id: v.get("transfer_id").and_then(|n| n.as_u64()).unwrap_or(0),
                name: v.get("name").and_then(|x| x.as_str()).unwrap_or("dosya").to_string(),
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
