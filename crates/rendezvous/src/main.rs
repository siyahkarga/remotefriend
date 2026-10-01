//! Rendezvous + relay sunucusu (VPS'te çalışır).
//!
//! - Host'lar kalıcı uplink açar, ID ile kaydolur.
//! - Client (native TLS/plain veya tarayıcı WS) ID ile host ister.
//! - Host onaylarsa dial-back gelir, sunucu iki ucu BİRLEŞTİRİR (splice).
//! - Sunucu video/şifre içeriğini anlamaz, sadece taşır (kendi VPS'in).
//!
//! Env:
//!   RF_RV_PORT (33202) - native relay dinleyici
//!   RF_WEB_PORT (8080, 127.0.0.1) - tarayıcı arayüzü (nginx arkası önerilir)
//!   RF_TLS_CERT / RF_TLS_KEY - varsa native TLS, yoksa düz + UYARI
//!   RF_PLAIN_OK=1 olmadan düz modda çalışmayı reddeder (güvenlik).

use anyhow::{Context, Result};
use remote_friend_common::io::{read_blob, read_rv, write_blob, write_rv};
use remote_friend_common::{RvMsg, RENDEZVOUS_PORT};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

// ---- tipler ----

type BoxRd = Box<dyn tokio::io::AsyncRead + Unpin + Send>;
type BoxWr = Box<dyn tokio::io::AsyncWrite + Unpin + Send>;

struct HostEntry {
    name: String,
    cmd_tx: mpsc::UnboundedSender<RvMsg>,
    last_beat: std::time::Instant,
}

enum PendingKind {
    Native { rd: Option<BoxRd>, wr: Option<BoxWr> },
    Web { sink: Option<WsSink>, stream: Option<WsStream> },
}

struct Pending {
    host_id: String,
    kind: PendingKind,
    created: std::time::Instant,
}

struct State {
    hosts: Mutex<HashMap<String, HostEntry>>,
    pending: Mutex<HashMap<u64, Pending>>,
    token_ctr: AtomicU64,
}

// axum WS tipleri (splice için kutulanır)
type WsSink = futures_util::stream::SplitSink<axum::extract::ws::WebSocket, axum::extract::ws::Message>;
type WsStream = futures_util::stream::SplitStream<axum::extract::ws::WebSocket>;

// ---- splice ----

/// İki framed bacağın ham baytlarını iki yönlü kopyala.
async fn splice_framed(mut a_rd: BoxRd, mut a_wr: BoxWr, mut b_rd: BoxRd, mut b_wr: BoxWr) {
    let a2b = async {
        loop {
            let m = read_blob(&mut a_rd).await?;
            write_blob(&mut b_wr, &m).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    let b2a = async {
        loop {
            let m = read_blob(&mut b_rd).await?;
            write_blob(&mut a_wr, &m).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    let _ = tokio::join!(a2b, b2a);
}

/// WS bacağı <-> framed dial-back bacağı. Text<->kind1, Binary<->kind0.
async fn splice_ws(
    mut sink: WsSink,
    mut stream: WsStream,
    mut h_rd: BoxRd,
    mut h_wr: BoxWr,
) {
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};
    let c2h = async {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(Message::Text(t)) => {
                    let mut m = vec![1u8];
                    m.extend_from_slice(t.as_bytes());
                    // kmsg: kind + len + bytes
                    if remote_friend_common::io::write_kmsg_raw(&mut h_wr, &m).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Binary(b)) => {
                    let mut m = vec![0u8];
                    m.extend_from_slice(&b);
                    if remote_friend_common::io::write_kmsg_raw(&mut h_wr, &m).await.is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
    };
    let h2c = async {
        loop {
            let (kind, payload) = match remote_friend_common::io::read_kmsg(&mut h_rd).await {
                Ok(x) => x,
                Err(_) => break,
            };
            let msg = if kind == 1 {
                match String::from_utf8(payload) {
                    Ok(s) => Message::Text(s),
                    Err(_) => break,
                }
            } else {
                Message::Binary(payload)
            };
            if sink.send(msg).await.is_err() {
                break;
            }
        }
    };
    let _ = tokio::join!(c2h, h2c);
}

// ---- native dinleyici ----

async fn handle_native(
    tcp: tokio::net::TcpStream,
    peer: String,
    state: Arc<State>,
    tls: Option<tokio_rustls::TlsAcceptor>,
) -> Result<()> {
    // TLS varsa sar, yoksa düz (kurulumda uyarıldı)
    let (mut rd, mut wr): (BoxRd, BoxWr) = match tls {
        Some(acc) => {
            let s = acc.accept(tcp).await.context("TLS accept")?;
            let (r, w) = tokio::io::split(s);
            (Box::new(r), Box::new(w))
        }
        None => {
            let (r, w) = tcp.into_split();
            (Box::new(r), Box::new(w))
        }
    };

    let first = tokio::time::timeout(std::time::Duration::from_secs(15), read_rv(&mut rd)).await??;

    match first {
        RvMsg::Register { id, name } => {
            // host uplink
            if !valid_id(&id) {
                write_rv(&mut wr, &RvMsg::RegisterError("ID 9 hane olmalı".into())).await?;
                return Ok(());
            }
            let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<RvMsg>();
            {
                let mut hosts = state.hosts.lock().await;
                if hosts.contains_key(&id) {
                    tracing::warn!("{id}: eski kayıt değiştiriliyor");
                }
                hosts.insert(id.clone(), HostEntry { name: name.clone(), cmd_tx, last_beat: std::time::Instant::now() });
            }
            write_rv(&mut wr, &RvMsg::RegisteredOk).await?;
            tracing::info!("host kaydoldu: {id} ({name}) [{peer}]");
            // uplink döngüsü: komut yaz + heartbeat/cevap oku
            loop {
                tokio::select! {
                    cmd = cmd_rx.recv() => {
                        match cmd {
                            Some(m) => { write_rv(&mut wr, &m).await?; }
                            None => break,
                        }
                    }
                    res = read_rv(&mut rd) => {
                        match res? {
                            RvMsg::Heartbeat => {
                                if let Some(h) = state.hosts.lock().await.get_mut(&id) {
                                    h.last_beat = std::time::Instant::now();
                                }
                            }
                            RvMsg::ApprovalAnswer { client, allow } => {
                                // hızlı ret: bekleyen dial-back'i iptal et
                                if !allow {
                                    reject_pending(&state, &client, "host bağlantıyı reddetti").await;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            state.hosts.lock().await.remove(&id);
            tracing::info!("host ayrıldı: {id}");
            Ok(())
        }
        RvMsg::Hello { id } => {
            // client: host ara + onay bekle
            let host = state.hosts.lock().await.get(&id).map(|h| (h.name.clone(), h.cmd_tx.clone()));
            let Some((hname, cmd_tx)) = host else {
                write_rv(&mut wr, &RvMsg::Rejected("ID bulunamadı (host çevrimiçi değil)".into())).await?;
                return Ok(());
            };
            let token = state.token_ctr.fetch_add(1, Ordering::Relaxed);
            let client_tag = format!("c{token}");
            state.pending.lock().await.insert(
                token,
                Pending { host_id: id.clone(), kind: PendingKind::Native { rd: Some(rd), wr: Some(wr) }, created: std::time::Instant::now() },
            );
            let _ = cmd_tx.send(RvMsg::ApprovalRequest { client: client_tag.clone(), addr: peer.clone(), kind: "native".into(), token });
            tracing::info!("{client_tag} -> {id} ({hname}) onay bekleniyor");
            // bu task biter; dial-back / ret / timeout bekleyenleri halleder
            Ok(())
        }
        RvMsg::ConnectBack { token } => {
            // host dial-back: bekleyen client ile birleştir (native veya web)
            let pend = state.pending.lock().await.remove(&token);
            match pend {
                Some(Pending { kind: PendingKind::Native { rd: Some(c_rd), wr: Some(mut c_wr) }, .. }) => {
                    write_rv(&mut c_wr, &RvMsg::Accepted).await?;
                    splice_framed(c_rd, c_wr, rd, wr).await;
                    Ok(())
                }
                Some(Pending { kind: PendingKind::Web { sink: Some(mut sink), stream: Some(stream) }, .. }) => {
                    use axum::extract::ws::Message;
                    use futures_util::SinkExt;
                    sink.send(Message::Text(r#"{"t":"welcome"}"#.into())).await?;
                    splice_ws(sink, stream, rd, wr).await;
                    Ok(())
                }
                _ => {
                    tracing::warn!("geçersiz/expired dial-back token");
                    Ok(())
                }
            }
        }
        _ => Ok(()),
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 9 && id.chars().all(|c| c.is_ascii_digit())
}

async fn reject_pending(state: &Arc<State>, client_tag: &str, msg: &str) {
    // token tara (tag c{token})
    let token: Option<u64> = client_tag.strip_prefix('c').and_then(|s| s.parse().ok());
    if let Some(t) = token {
        if let Some(pend) = state.pending.lock().await.remove(&t) {
            match pend.kind {
                PendingKind::Native { wr: Some(mut w), .. } => {
                    let _ = write_rv(&mut w, &RvMsg::Rejected(msg.into())).await;
                }
                PendingKind::Web { sink: Some(mut s), .. } => {
                    use futures_util::SinkExt;
                    let _ = s.send(axum::extract::ws::Message::Text(format!(r#"{{"t":"reject","msg":"{msg}"}}"#))).await;
                }
                _ => {}
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let port: u16 = std::env::var("RF_RV_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(RENDEZVOUS_PORT);
    let web_port: u16 = std::env::var("RF_WEB_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(8080);

    let tls = match (std::env::var("RF_TLS_CERT").ok(), std::env::var("RF_TLS_KEY").ok()) {
        (Some(c), Some(k)) => {
            tracing::info!("TLS açık: {c}");
            Some(remote_friend_common::tls::tls_acceptor(&c, &k)?)
        }
        _ => {
            if std::env::var("RF_PLAIN_OK").map(|v| v == "1").unwrap_or(false) {
                tracing::warn!("!!! DÜZ (şifresiz) mod: sadece test için!");
                None
            } else {
                anyhow::bail!("TLS sertifikası yok. RF_TLS_CERT/RF_TLS_KEY ver ya da test için RF_PLAIN_OK=1");
            }
        }
    };

    let state = Arc::new(State {
        hosts: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
        token_ctr: AtomicU64::new(1),
    });

    // zaman aşımı süpürücü (60 sn onaysız bekleyenler)
    {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                let mut pend = st.pending.lock().await;
                let expired: Vec<u64> = pend.iter().filter(|(_, p)| p.created.elapsed().as_secs() > 60).map(|(t, _)| *t).collect();
                for t in expired {
                    if let Some(p) = pend.remove(&t) {
                        let tag = format!("c{t}");
                        drop(pend);
                        reject_pending(&st, &tag, "onay zaman aşımı").await;
                        pend = st.pending.lock().await;
                    }
                }
                // ölü hostları temizle (kalp atışı yoksa uplink zaten kopmuştur; ekstra güvence atla)
            }
        });
    }

    // web (tarayıcı) sunucusu
    {
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = web_serve(web_port, st).await {
                tracing::warn!("web kapandı: {e:#}");
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    tracing::info!("rendezvous dinliyor: 0.0.0.0:{port} (TLS: {})", tls.is_some());
    loop {
        let (tcp, peer) = listener.accept().await?;
        let st = state.clone();
        let tls = tls.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_native(tcp, peer.to_string(), st, tls).await {
                tracing::warn!("bağlantı hatası {peer}: {e:#}");
            }
        });
    }
}

// ---- web (tarayıcı clientlar): aynı sayfa, ID ile yönlendirme ----

async fn web_serve(port: u16, state: Arc<State>) -> Result<()> {
    use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
    use axum::extract::State as AxState;
    use axum::response::{Html, IntoResponse};
    use axum::routing::get;
    use futures_util::{SinkExt, StreamExt};

    async fn page() -> Html<&'static str> {
        Html(remote_friend_common::webapp::WEBAPP)
    }

    async fn ws_handler(
        ws: WebSocketUpgrade,
        AxState(state): AxState<Arc<State>>,
    ) -> impl IntoResponse {
        ws.on_upgrade(move |socket| handle_web(socket, state))
    }

    async fn handle_web(socket: WebSocket, state: Arc<State>) {
        let (mut sink, mut stream) = socket.split();
        // hello (15 sn): {t:"hello", id:"123456789"}
        let hello = tokio::time::timeout(std::time::Duration::from_secs(15), stream.next()).await;
        let id = match hello {
            Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<serde_json::Value>(&t)
                .ok()
                .and_then(|v| v.get("id").and_then(|x| x.as_str()).map(|s| s.replace(' ', "")))
                .unwrap_or_default(),
            _ => String::new(),
        };
        if !valid_id(&id) {
            let _ = sink.send(Message::Text(r#"{"t":"reject","msg":"9 haneli ID gerekli"}"#.into())).await;
            return;
        }
        let host = state.hosts.lock().await.get(&id).map(|h| (h.name.clone(), h.cmd_tx.clone()));
        let Some((hname, cmd_tx)) = host else {
            let _ = sink.send(Message::Text(r#"{"t":"reject","msg":"ID bulunamadı (host çevrimiçi değil)"}"#.into())).await;
            return;
        };
        let token = state.token_ctr.fetch_add(1, Ordering::Relaxed);
        let client_tag = format!("c{token}");
        state.pending.lock().await.insert(
            token,
            Pending { host_id: id.clone(), kind: PendingKind::Web { sink: Some(sink), stream: Some(stream) }, created: std::time::Instant::now() },
        );
        let _ = cmd_tx.send(RvMsg::ApprovalRequest { client: client_tag.clone(), addr: "tarayıcı".into(), kind: "web".into(), token });
        tracing::info!("{client_tag} (web) -> {id} ({hname}) onay bekleniyor");
        // sonrası: dial-back / ret / timeout halleder, bu task biter
    }

    let app = axum::Router::new()
        .route("/", get(page))
        .route("/ws", get(ws_handler))
        .with_state(state);
    let web_bind = std::env::var("RF_WEB_BIND").unwrap_or("127.0.0.1".into());
    let listener = tokio::net::TcpListener::bind(format!("{web_bind}:{port}")).await?;
    tracing::info!("web arayüzü: {web_bind}:{port}");
    axum::serve(listener, app).await?;
    Ok(())
}
