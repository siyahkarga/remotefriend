//! Rendezvous + relay sunucusu (VPS'te çalışır).
//!
//! - Host'lar kalıcı uplink açar, ID ile kaydolur.
//! - Client (native TLS/plain veya tarayıcı WS) ID ile host ister.
//! - Host onaylarsa dial-back gelir, sunucu iki ucu BİRLEŞTİRİR (splice).
//! - TLS VPS'te sonlanır: sunucu operatörü trafiği teknik olarak görebilir; güvenilir VPS kullan.
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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

// ---- tipler ----

type BoxRd = Box<dyn tokio::io::AsyncRead + Unpin + Send>;
type BoxWr = Box<dyn tokio::io::AsyncWrite + Unpin + Send>;

struct HostEntry {
    name: String,
    secret: String,
    session_id: u128,
    cmd_tx: mpsc::Sender<RvMsg>,
    last_beat: std::time::Instant,
}

enum PendingKind {
    Native { rd: Option<BoxRd>, wr: Option<BoxWr> },
    Web {
        sink: Option<WsSink>,
        stream: Option<WsStream>,
        jpeg: bool,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
    },
}

struct Pending {
    host_id: String,
    kind: PendingKind,
    created: std::time::Instant,
    // Pending sayısını atomik olarak sınırlar; kayıt tablosundan çıkınca otomatik bırakılır.
    _pending_permit: tokio::sync::OwnedSemaphorePermit,
}

struct State {
    hosts: Mutex<HashMap<String, HostEntry>>,
    pending: Mutex<HashMap<u128, Pending>>,
    registry: Mutex<HashMap<String, String>>,
    registry_path: PathBuf,
    web_slots: Arc<tokio::sync::Semaphore>,
    pending_slots: Arc<tokio::sync::Semaphore>,
}

// axum WS tipleri (splice için kutulanır)
type WsSink = futures_util::stream::SplitSink<axum::extract::ws::WebSocket, axum::extract::ws::Message>;
type WsStream = futures_util::stream::SplitStream<axum::extract::ws::WebSocket>;

fn valid_secret(secret: &str) -> bool {
    secret.len() == 64 && secret.chars().all(|c| c.is_ascii_hexdigit())
}

fn registry_path() -> PathBuf {
    std::env::var("RF_HOST_REGISTRY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/remotefriend/hosts.json"))
}

fn load_registry(path: &Path) -> Result<HashMap<String, String>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let data = std::fs::read(path)
        .with_context(|| format!("host kayıt dosyası okunamadı: {}", path.display()))?;
    let map: HashMap<String, String> = serde_json::from_slice(&data)
        .with_context(|| format!("host kayıt dosyası bozuk: {}", path.display()))?;
    if map.iter().any(|(id, secret)| !valid_id(id) || !valid_secret(secret)) {
        anyhow::bail!("host kayıt dosyasında geçersiz ID/secret var");
    }
    Ok(map)
}

fn save_registry(path: &Path, map: &HashMap<String, String>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("host kayıt dizini oluşturulamadı: {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(map)?;
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        use std::io::Write as _;
        let mut f = opts.open(&tmp)?;
        f.write_all(&data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn fresh_token(pending: &HashMap<u128, Pending>) -> u128 {
    loop {
        let token = rand::random::<u128>();
        if token != 0 && !pending.contains_key(&token) {
            return token;
        }
    }
}

fn client_tag(token: u128) -> String {
    format!("c{token:032x}")
}

fn origin_allowed(headers: &axum::http::HeaderMap) -> bool {
    use axum::http::header::{HOST, ORIGIN};
    let Some(origin) = headers.get(ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(host) = headers.get(HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    if origin == format!("https://{host}") || origin == format!("http://{host}") {
        return true;
    }
    std::env::var("RF_ALLOWED_ORIGIN")
        .ok()
        .map(|list| list.split(',').any(|item| item.trim() == origin))
        .unwrap_or(false)
}

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
    // Bir yön kapandığında diğer kopyalama gelecekte sonsuza kadar beklememeli.
    // select! kaybeden future'ı düşürür; ilgili reader/writer handle'ları da kapanır.
    tokio::select! {
        _ = a2b => {},
        _ = b2a => {},
    }
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
                    if t.len() > 512 * 1024 { break; }
                    let mut m = vec![1u8];
                    m.extend_from_slice(t.as_bytes());
                    // kmsg: kind + len + bytes
                    if remote_friend_common::io::write_kmsg_raw(&mut h_wr, &m).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Binary(b)) => {
                    if b.len() > 24_000_000 { break; }
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
                if payload.len() > 512 * 1024 { break; }
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
    // Tarayıcı veya host tarafı kapandığında kalan yönü iptal ederek yarım-açık
    // relay oturumlarının kaynak tüketmesini önle.
    tokio::select! {
        _ = c2h => {},
        _ = h2c => {},
    }
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
            let s = tokio::time::timeout(std::time::Duration::from_secs(15), acc.accept(tcp))
                .await
                .context("TLS handshake zaman aşımı")??;
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
        RvMsg::Register { id, name, secret } => {
            // Host ID artık kalıcı 256-bit sır ile sahiplenilir. Aynı ID'yi başka biri kaydedemez.
            if !valid_id(&id) || !valid_secret(&secret) || !valid_name(&name) {
                write_rv(&mut wr, &RvMsg::RegisterError("geçersiz ID veya host secret".into())).await?;
                return Ok(());
            }
            {
                let mut registry = state.registry.lock().await;
                match registry.get(&id) {
                    Some(known) if known != &secret => {
                        write_rv(&mut wr, &RvMsg::RegisterError(
                            "bu ID başka bir host anahtarına kayıtlı".into(),
                        )).await?;
                        tracing::warn!("{id}: yanlış host secret ile kayıt denemesi [{peer}]");
                        return Ok(());
                    }
                    Some(_) => {}
                    None => {
                        registry.insert(id.clone(), secret.clone());
                        if let Err(e) = save_registry(&state.registry_path, &registry) {
                            registry.remove(&id);
                            write_rv(&mut wr, &RvMsg::RegisterError(
                                "host registry yazılamadı; sunucu yöneticisine bildir".into(),
                            )).await?;
                            return Err(e);
                        }
                    }
                }
            }

            let session_id = rand::random::<u128>();
            let (cmd_tx, mut cmd_rx) = mpsc::channel::<RvMsg>(64);
            {
                let mut hosts = state.hosts.lock().await;
                if hosts.contains_key(&id) {
                    tracing::warn!("{id}: aynı anahtarlı eski oturum değiştiriliyor");
                }
                hosts.insert(id.clone(), HostEntry {
                    name: name.clone(),
                    secret: secret.clone(),
                    session_id,
                    cmd_tx,
                    last_beat: std::time::Instant::now(),
                });
            }
            write_rv(&mut wr, &RvMsg::RegisteredOk).await?;
            tracing::info!("host kaydoldu: {id} ({name}) [{peer}]");
            // uplink döngüsü: komut yaz + heartbeat/cevap oku.
            // 65 saniye heartbeat gelmezse yarım-açık oturumu temizle.
            let mut liveness = tokio::time::interval(std::time::Duration::from_secs(10));
            liveness.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    cmd = cmd_rx.recv() => {
                        match cmd {
                            Some(m) => { write_rv(&mut wr, &m).await?; }
                            None => break,
                        }
                    }
                    _ = liveness.tick() => {
                        let stale = state.hosts.lock().await.get(&id)
                            .map_or(true, |h| h.session_id != session_id || h.last_beat.elapsed().as_secs() > 65);
                        if stale {
                            tracing::warn!("{id}: heartbeat zaman aşımı");
                            break;
                        }
                    }
                    res = read_rv(&mut rd) => {
                        match res? {
                            RvMsg::Heartbeat => {
                                if let Some(h) = state.hosts.lock().await.get_mut(&id) {
                                    if h.session_id == session_id {
                                        h.last_beat = std::time::Instant::now();
                                    }
                                }
                            }
                            RvMsg::ApprovalAnswer { client, allow } => {
                                if !allow {
                                    reject_pending(&state, &client, "kimlik doğrulama başarısız veya host reddetti").await;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            {
                let mut hosts = state.hosts.lock().await;
                if hosts.get(&id).is_some_and(|h| h.session_id == session_id) {
                    hosts.remove(&id);
                }
            }
            tracing::info!("host ayrıldı: {id}");
            Ok(())
        }
        RvMsg::Hello { id, auth } => {
            // client: host ara + onay bekle
            if !valid_id(&id) {
                write_rv(&mut wr, &RvMsg::Rejected("9 haneli ID gerekli".into())).await?;
                return Ok(());
            }
            let auth = match auth {
                Some(value) if value.len() <= 256 => Some(value),
                None => None,
                Some(_) => {
                    write_rv(&mut wr, &RvMsg::Rejected("kimlik doğrulama verisi çok büyük".into())).await?;
                    return Ok(());
                }
            };
            let host = state.hosts.lock().await.get(&id).map(|h| (h.name.clone(), h.cmd_tx.clone()));
            let Some((hname, cmd_tx)) = host else {
                write_rv(&mut wr, &RvMsg::Rejected("ID bulunamadı (host çevrimiçi değil)".into())).await?;
                return Ok(());
            };
            let pending_permit = match state.pending_slots.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    write_rv(&mut wr, &RvMsg::Rejected("sunucuda çok fazla bekleyen bağlantı var".into())).await?;
                    return Ok(());
                }
            };
            write_rv(&mut wr, &RvMsg::WaitApproval).await?;
            let (token, client_tag) = {
                let mut pending = state.pending.lock().await;
                let token = fresh_token(&pending);
                let tag = client_tag(token);
                pending.insert(
                    token,
                    Pending {
                        host_id: id.clone(),
                        kind: PendingKind::Native { rd: Some(rd), wr: Some(wr) },
                        created: std::time::Instant::now(),
                        _pending_permit: pending_permit,
                    },
                );
                (token, tag)
            };
            if cmd_tx.try_send(RvMsg::ApprovalRequest {
                client: client_tag.clone(),
                addr: peer.clone(),
                kind: "native".into(),
                token,
                auth,
            }).is_err() {
                reject_pending(&state, &client_tag, "host komut kuyruğu dolu veya kapalı").await;
                return Ok(());
            }
            tracing::info!("{client_tag} -> {id} ({hname}) onay bekleniyor");
            // bu task biter; dial-back / ret / timeout bekleyenleri halleder
            Ok(())
        }
        RvMsg::ConnectBack { token, id, secret } => {
            // Dial-back ancak kayıtlı host kimliği + CSPRNG token birlikte doğruysa kabul edilir.
            let registered = state.hosts.lock().await.get(&id)
                .is_some_and(|h| h.secret.as_str() == secret.as_str());
            if !registered {
                tracing::warn!("yetkisiz dial-back: host kimliği doğrulanamadı");
                return Ok(());
            }
            let pend = {
                let mut pending = state.pending.lock().await;
                if pending.get(&token).is_some_and(|p| p.host_id == id) {
                    pending.remove(&token)
                } else {
                    None
                }
            };
            match pend {
                Some(Pending { kind: PendingKind::Native { rd: Some(c_rd), wr: Some(mut c_wr) }, .. }) => {
                    write_rv(&mut c_wr, &RvMsg::Accepted).await?;
                    splice_framed(c_rd, c_wr, rd, wr).await;
                    Ok(())
                }
                Some(Pending { kind: PendingKind::Web { sink: Some(mut sink), stream: Some(stream), jpeg, permit: Some(_permit) }, .. }) => {
                    use axum::extract::ws::Message;
                    use futures_util::SinkExt;
                    if jpeg {
                        sink.send(Message::Text(r#"{"t":"welcome","jpeg":true,"fps":10}"#.into())).await?;
                    } else {
                        sink.send(Message::Text(r#"{"t":"welcome","codec":"h264","fps":30}"#.into())).await?;
                    }
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

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.chars().any(|c| c.is_control())
}

async fn reject_pending(state: &Arc<State>, client_tag: &str, msg: &str) {
    // token tara (tag c + 32 hex karakter)
    let token: Option<u128> = client_tag
        .strip_prefix('c')
        .and_then(|s| u128::from_str_radix(s, 16).ok());
    if let Some(t) = token {
        if let Some(pend) = state.pending.lock().await.remove(&t) {
            match pend.kind {
                PendingKind::Native { wr: Some(mut w), .. } => {
                    let _ = write_rv(&mut w, &RvMsg::Rejected(msg.into())).await;
                }
                PendingKind::Web { sink: Some(mut s), .. } => {
                    use futures_util::SinkExt;
                    let payload = serde_json::json!({"t": "reject", "msg": msg}).to_string();
                    let _ = s.send(axum::extract::ws::Message::Text(payload)).await;
                }
                _ => {}
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    remote_friend_common::tls::init_crypto();
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

    let registry_path = registry_path();
    let registry = load_registry(&registry_path)?;
    tracing::info!("host registry: {} ({} kayıt)", registry_path.display(), registry.len());
    let web_limit = std::env::var("RF_MAX_WEB_SESSIONS")
        .ok().and_then(|s| s.parse().ok()).filter(|n: &usize| *n > 0).unwrap_or(64);
    let pending_limit = std::env::var("RF_MAX_PENDING")
        .ok().and_then(|s| s.parse().ok()).filter(|n: &usize| *n > 0).unwrap_or(256);
    let state = Arc::new(State {
        hosts: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
        registry: Mutex::new(registry),
        registry_path,
        web_slots: Arc::new(tokio::sync::Semaphore::new(web_limit)),
        pending_slots: Arc::new(tokio::sync::Semaphore::new(pending_limit)),
    });

    // zaman aşımı süpürücü (60 sn onaysız bekleyenler)
    {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                let expired: Vec<u128> = {
                    let pend = st.pending.lock().await;
                    pend.iter()
                        .filter(|(_, p)| p.created.elapsed().as_secs() > 60)
                        .map(|(t, _)| *t)
                        .collect()
                };
                for t in expired {
                    reject_pending(&st, &client_tag(t), "onay zaman aşımı").await;
                }
                // Ölü host bağlantısı read_rv ile kapanır; session_id eski oturumun yenisini silmesini engeller.
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
    let native_limit = std::env::var("RF_MAX_NATIVE_CONNECTIONS")
        .ok().and_then(|s| s.parse().ok()).filter(|n: &usize| *n > 0).unwrap_or(128);
    let native_slots = Arc::new(tokio::sync::Semaphore::new(native_limit));
    tracing::info!("rendezvous dinliyor: 0.0.0.0:{port} (TLS: {}, limit: {native_limit})", tls.is_some());
    loop {
        let (tcp, peer) = listener.accept().await?;
        let permit = match native_slots.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("native bağlantı limiti dolu; {peer} reddedildi");
                drop(tcp);
                continue;
            }
        };
        let st = state.clone();
        let tls = tls.clone();
        tokio::spawn(async move {
            let _permit = permit;
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
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{Html, IntoResponse, Response};
    use axum::routing::get;
    use futures_util::{SinkExt, StreamExt};

    async fn page() -> Response {
        use axum::http::{HeaderName, HeaderValue};
        let mut response = Html(remote_friend_common::webapp::WEBAPP).into_response();
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

    async fn ws_handler(
        ws: WebSocketUpgrade,
        AxState(state): AxState<Arc<State>>,
        headers: HeaderMap,
    ) -> Response {
        if !origin_allowed(&headers) {
            tracing::warn!("WebSocket Origin reddedildi");
            return StatusCode::FORBIDDEN.into_response();
        }
        let permit = match state.web_slots.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        ws.on_upgrade(move |socket| handle_web(socket, state, permit)).into_response()
    }

    async fn handle_web(
        socket: WebSocket,
        state: Arc<State>,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) {
        let (mut sink, mut stream) = socket.split();
        // hello (15 sn): {t:"hello", id:"123456789", password:"...", jpeg:true?}
        let hello = tokio::time::timeout(std::time::Duration::from_secs(15), stream.next()).await;
        let (id, password, jpeg) = match hello {
            Ok(Some(Ok(Message::Text(t)))) if t.len() <= 4096 => {
                serde_json::from_str::<serde_json::Value>(&t)
                    .ok()
                    .map(|v| (
                        v.get("id").and_then(|x| x.as_str())
                            .map(|s| s.replace(' ', "")).unwrap_or_default(),
                        v.get("password").and_then(|x| x.as_str())
                            .unwrap_or("").chars().take(256).collect::<String>(),
                        v.get("jpeg").and_then(|x| x.as_bool()).unwrap_or(false),
                    ))
                    .unwrap_or_default()
            }
            _ => (String::new(), String::new(), false),
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
        let pending_permit = match state.pending_slots.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                let _ = sink.send(Message::Text(r#"{"t":"reject","msg":"sunucuda çok fazla bekleyen bağlantı var"}"#.into())).await;
                return;
            }
        };
        let _ = sink.send(Message::Text(r#"{"t":"wait"}"#.into())).await;
        let (token, tag) = {
            let mut pending = state.pending.lock().await;
            let token = fresh_token(&pending);
            let tag = client_tag(token);
            pending.insert(
                token,
                Pending {
                    host_id: id.clone(),
                    kind: PendingKind::Web {
                        sink: Some(sink),
                        stream: Some(stream),
                        jpeg,
                        permit: Some(permit),
                    },
                    created: std::time::Instant::now(),
                    _pending_permit: pending_permit,
                },
            );
            (token, tag)
        };
        let kind = if jpeg { "web-jpeg" } else { "web" };
        if cmd_tx.try_send(RvMsg::ApprovalRequest {
            client: tag.clone(),
            addr: "tarayıcı".into(),
            kind: kind.into(),
            token,
            auth: Some(password),
        }).is_err() {
            reject_pending(&state, &tag, "host bağlantısı kapandı").await;
            return;
        }
        tracing::info!("{tag} (web) -> {id} ({hname}) kimlik doğrulama/onay bekleniyor");
        // sonrası: host dial-back / ret / timeout halleder
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
