//! Rendezvous + relay server (runs on the VPS).
//!
//! - Hosts open a persistent uplink and register with their ID.
//! - A client (native TLS/plain or browser WS) asks for a host by ID.
//! - If the host approves, it dials back and the server SPLICES the two ends together.
//! - TLS terminates on the VPS: the server operator can technically see the traffic; use a trusted VPS.
//!
//! Env:
//!   RF_RV_PORT (33202) - native relay listener
//!   RF_WEB_PORT (33203, 127.0.0.1) - browser UI (behind nginx recommended)
//!   RF_TLS_CERT / RF_TLS_KEY - native TLS certificate. When provided via systemd
//!     `LoadCredential=` ($CREDENTIALS_DIRECTORY/cert.pem, key.pem), file permissions
//!     don't matter: systemd reads the file as root and hands it to the service.
//!   Refuses to run in plaintext mode unless RF_PLAIN_OK=1 (security).

use anyhow::{Context, Result};
use remote_friend_common::io::{read_blob, read_rv, write_blob, write_rv};
use remote_friend_common::{RvMsg, RENDEZVOUS_PORT};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

// ---- types ----

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
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
    },
}

struct Pending {
    host_id: String,
    kind: PendingKind,
    created: std::time::Instant,
    // Atomically caps the number of pending entries; released automatically when removed from the table.
    _pending_permit: tokio::sync::OwnedSemaphorePermit,
}

struct State {
    hosts: Mutex<HashMap<String, HostEntry>>,
    pending: Mutex<HashMap<u128, Pending>>,
    registry: Mutex<HashMap<String, String>>,
    registry_path: PathBuf,
    web_slots: Arc<tokio::sync::Semaphore>,
    pending_slots: Arc<tokio::sync::Semaphore>,
    /// Per-IP connection request limit (first line of defense against password-guessing spam;
    /// the real lockout is on the host).
    hello_rate: std::sync::Mutex<HashMap<std::net::IpAddr, (u32, std::time::Instant)>>,
    /// Registration key; when set, only computers that know it can register new IDs.
    register_key: Option<String>,
    /// Open native connections per IP (caps how many one address can hold open).
    conns_per_ip: std::sync::Mutex<HashMap<std::net::IpAddr, u32>>,
}

const MAX_CONNS_PER_IP: u32 = 16;

/// Releases a per-IP connection slot when dropped.
struct IpSlot {
    state: Arc<State>,
    ip: std::net::IpAddr,
}

impl Drop for IpSlot {
    fn drop(&mut self) {
        let mut map = self.state.conns_per_ip.lock().unwrap();
        if let Some(n) = map.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                map.remove(&self.ip);
            }
        }
    }
}

fn take_ip_slot(state: &Arc<State>, ip: std::net::IpAddr) -> Option<IpSlot> {
    let mut map = state.conns_per_ip.lock().unwrap();
    let n = map.entry(ip).or_insert(0);
    if *n >= MAX_CONNS_PER_IP {
        return None;
    }
    *n += 1;
    Some(IpSlot { state: state.clone(), ip })
}

const HELLO_PER_MINUTE: u32 = 12;

impl State {
    fn hello_allowed(&self, ip: std::net::IpAddr) -> bool {
        let now = std::time::Instant::now();
        let mut map = self.hello_rate.lock().unwrap();
        let entry = map.entry(ip).or_insert((0, now));
        if now.duration_since(entry.1).as_secs() >= 60 {
            *entry = (0, now);
        }
        entry.0 += 1;
        entry.0 <= HELLO_PER_MINUTE
    }

    fn sweep_rate(&self) {
        let now = std::time::Instant::now();
        self.hello_rate.lock().unwrap().retain(|_, (_, t)| now.duration_since(*t).as_secs() < 60);
    }
}

// axum WS types (boxed for splicing)
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
        .with_context(|| format!("could not read host registry file: {}", path.display()))?;
    let map: HashMap<String, String> = serde_json::from_slice(&data)
        .with_context(|| format!("host registry file is corrupt: {}", path.display()))?;
    if map.iter().any(|(id, secret)| !valid_id(id) || !valid_secret(secret)) {
        anyhow::bail!("host registry file contains an invalid ID/secret");
    }
    Ok(map)
}

fn save_registry(path: &Path, map: &HashMap<String, String>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create host registry directory: {}", parent.display()))?;
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

/// Copy raw bytes between two framed legs in both directions.
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
    // When one direction closes, the other copy must not wait forever.
    // select! drops the losing future, which also closes its reader/writer handles.
    tokio::select! {
        _ = a2b => {},
        _ = b2a => {},
    }
}

/// WS leg <-> framed dial-back leg. Text<->kind1, Binary<->kind0.
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
    // When the browser or host side closes, cancel the remaining direction so
    // half-open relay sessions don't keep consuming resources.
    tokio::select! {
        _ = c2h => {},
        _ = h2c => {},
    }
}

// ---- native listener ----

async fn handle_native(
    tcp: tokio::net::TcpStream,
    peer_addr: std::net::SocketAddr,
    state: Arc<State>,
    tls: Option<tokio_rustls::TlsAcceptor>,
) -> Result<()> {
    let peer = peer_addr.to_string();
    let _ = tcp.set_nodelay(true);
    // Wrap in TLS if configured, otherwise plaintext (warned at startup)
    let (mut rd, mut wr): (BoxRd, BoxWr) = match tls {
        Some(acc) => {
            let s = tokio::time::timeout(std::time::Duration::from_secs(15), acc.accept(tcp))
                .await
                .context("TLS handshake timed out")??;
            let (r, w) = tokio::io::split(s);
            (Box::new(r), Box::new(w))
        }
        None => {
            let (r, w) = tcp.into_split();
            (Box::new(r), Box::new(w))
        }
    };

    let first = tokio::time::timeout(std::time::Duration::from_secs(15), read_rv(&mut rd)).await??;

    // Older hosts send Register without a key.
    let first = match first {
        RvMsg::Register { id, name, secret } => RvMsg::RegisterV2 { id, name, secret, key: String::new() },
        other => other,
    };
    match first {
        RvMsg::RegisterV2 { id, name, secret, key } => {
            // A host ID is owned via a persistent 256-bit secret. Nobody else can register the same ID.
            if !valid_id(&id) || !valid_secret(&secret) || !valid_name(&name) {
                write_rv(&mut wr, &RvMsg::RegisterError("invalid ID or host secret".into())).await?;
                return Ok(());
            }
            {
                let mut registry = state.registry.lock().await;
                // With a registration key, only computers that know it can register NEW IDs
                // (already registered computers keep working with their host secret).
                let key_ok = state.register_key.as_deref().is_none_or(|k| {
                    remote_friend_common::constant_time_eq(k.as_bytes(), key.trim().as_bytes())
                });
                if !key_ok && registry.get(&id).is_none_or(|known| known != &secret) {
                    write_rv(&mut wr, &RvMsg::RegisterError(
                        "this server only accepts computers with its server key (RemoteFriend: Settings -> Server key)".into(),
                    )).await?;
                    tracing::warn!("{id}: registration refused (missing or wrong server key) [{peer}]");
                    return Ok(());
                }
                match registry.get(&id) {
                    Some(known) if known != &secret => {
                        write_rv(&mut wr, &RvMsg::RegisterError(
                            "this ID is registered to a different host key".into(),
                        )).await?;
                        tracing::warn!("{id}: registration attempt with wrong host secret [{peer}]");
                        return Ok(());
                    }
                    Some(_) => {}
                    None => {
                        registry.insert(id.clone(), secret.clone());
                        if let Err(e) = save_registry(&state.registry_path, &registry) {
                            registry.remove(&id);
                            write_rv(&mut wr, &RvMsg::RegisterError(
                                "could not write host registry; tell the server administrator".into(),
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
                    tracing::warn!("{id}: replacing previous session with the same key");
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
            tracing::info!("host registered: {id} ({name}) [{peer}]");
            // uplink loop: write commands + read heartbeats/answers.
            // Clean up a half-open session if no heartbeat arrives for 65 seconds.
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
                            tracing::warn!("{id}: heartbeat timed out");
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
                                    reject_pending(&state, &client, "wrong password, connection denied, or too many failed attempts (wait 1 minute)").await;
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
            tracing::info!("host disconnected: {id}");
            Ok(())
        }
        RvMsg::Hello { id, auth } => {
            // client: look up host + wait for approval
            if !state.hello_allowed(peer_addr.ip()) {
                write_rv(&mut wr, &RvMsg::Rejected("too many attempts; try again in 1 minute".into())).await?;
                return Ok(());
            }
            if !valid_id(&id) {
                write_rv(&mut wr, &RvMsg::Rejected("a 9-digit ID is required".into())).await?;
                return Ok(());
            }
            let auth = match auth {
                Some(value) if value.len() <= 256 => Some(value),
                None => None,
                Some(_) => {
                    write_rv(&mut wr, &RvMsg::Rejected("authentication data too large".into())).await?;
                    return Ok(());
                }
            };
            let host = state.hosts.lock().await.get(&id).map(|h| (h.name.clone(), h.cmd_tx.clone()));
            let Some((hname, cmd_tx)) = host else {
                write_rv(&mut wr, &RvMsg::Rejected("ID not found (host is not online)".into())).await?;
                return Ok(());
            };
            let pending_permit = match state.pending_slots.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    write_rv(&mut wr, &RvMsg::Rejected("too many pending connections on the server".into())).await?;
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
                reject_pending(&state, &client_tag, "host command queue is full or closed").await;
                return Ok(());
            }
            tracing::info!("{client_tag} -> {id} ({hname}) awaiting approval");
            // this task ends here; dial-back / reject / timeout handle the pending entry
            Ok(())
        }
        RvMsg::ConnectBack { token, id, secret } => {
            // A dial-back is accepted only if both the registered host identity and the CSPRNG token match.
            let registered = state.hosts.lock().await.get(&id)
                .is_some_and(|h| h.secret.as_str() == secret.as_str());
            if !registered {
                tracing::warn!("unauthorized dial-back: host identity could not be verified");
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
                Some(Pending { kind: PendingKind::Web { sink: Some(sink), stream: Some(stream), permit: Some(_permit) }, .. }) => {
                    // The host sends the welcome message (it knows the codec/name/quality).
                    splice_ws(sink, stream, rd, wr).await;
                    Ok(())
                }
                _ => {
                    tracing::warn!("invalid/expired dial-back token");
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
    // parse the token (tag is 'c' + 32 hex characters)
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

/// Registration key: systemd credential "register.key", RF_REGISTER_KEY_FILE or RF_REGISTER_KEY.
fn load_register_key() -> Option<String> {
    let from_file = |p: std::path::PathBuf| std::fs::read_to_string(p).ok();
    let key = std::env::var("CREDENTIALS_DIRECTORY")
        .ok()
        .and_then(|d| from_file(Path::new(&d).join("register.key")))
        .or_else(|| std::env::var("RF_REGISTER_KEY_FILE").ok().and_then(|p| from_file(p.into())))
        .or_else(|| std::env::var("RF_REGISTER_KEY").ok())?;
    let key = key.trim().to_string();
    (!key.is_empty()).then_some(key)
}

/// Certificate paths: systemd credentials directory first, then RF_TLS_CERT/RF_TLS_KEY.
fn tls_paths() -> (Option<String>, Option<String>) {
    if let Ok(dir) = std::env::var("CREDENTIALS_DIRECTORY") {
        let c = Path::new(&dir).join("cert.pem");
        let k = Path::new(&dir).join("key.pem");
        if c.exists() && k.exists() {
            return (Some(c.display().to_string()), Some(k.display().to_string()));
        }
    }
    (std::env::var("RF_TLS_CERT").ok(), std::env::var("RF_TLS_KEY").ok())
}

/// On file errors such as "Permission denied", print clear instructions for fixing them.
fn explain_file_error(e: &anyhow::Error, paths: &[&str]) {
    let denied = e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
    });
    if denied {
        let user = std::env::var("USER").unwrap_or_else(|_| "remotefriend".into());
        eprintln!("ERROR: could not read/write file (permission denied): {}", paths.join(", "));
        eprintln!("Fix: the current service file passes the certificate via LoadCredential (independent of file permissions).");
        eprintln!("  Re-run the setup script, or manually: sudo chown root:{user} <file> && sudo chmod 640 <file>");
        eprintln!("  For the registry directory: sudo chown -R {user}:{user} /var/lib/remotefriend && sudo chmod 700 /var/lib/remotefriend");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    remote_friend_common::tls::init_crypto();
    tracing_subscriber::fmt::init();
    let port: u16 = std::env::var("RF_RV_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(RENDEZVOUS_PORT);
    let web_port: u16 = std::env::var("RF_WEB_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(33203);

    let tls = match tls_paths() {
        (Some(c), Some(k)) => {
            tracing::info!("TLS enabled: {c}");
            match remote_friend_common::tls::tls_acceptor(&c, &k) {
                Ok(acc) => Some(acc),
                Err(e) => {
                    explain_file_error(&e, &[&c, &k]);
                    return Err(e);
                }
            }
        }
        _ => {
            if std::env::var("RF_PLAIN_OK").map(|v| v == "1").unwrap_or(false) {
                tracing::warn!("!!! PLAINTEXT (unencrypted) mode: for testing only!");
                None
            } else {
                anyhow::bail!("No TLS certificate. Set RF_TLS_CERT/RF_TLS_KEY, or RF_PLAIN_OK=1 for testing");
            }
        }
    };

    let registry_path = registry_path();
    let registry = match load_registry(&registry_path) {
        Ok(r) => r,
        Err(e) => {
            let p = registry_path.display().to_string();
            explain_file_error(&e, &[&p]);
            return Err(e);
        }
    };
    tracing::info!("host registry: {} ({} entries)", registry_path.display(), registry.len());
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
        hello_rate: std::sync::Mutex::new(HashMap::new()),
        register_key: load_register_key(),
        conns_per_ip: std::sync::Mutex::new(HashMap::new()),
    });
    match &state.register_key {
        Some(_) => tracing::info!("server key set: only computers with the key can register"),
        None => tracing::warn!("no server key: ANY computer can register (set one with the setup script)"),
    }

    // timeout sweeper (entries waiting >60 s without approval)
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
                    reject_pending(&st, &client_tag(t), "approval timed out").await;
                }
                st.sweep_rate();
                // A dead host connection closes via read_rv; session_id prevents an old session from removing a newer one.
            }
        });
    }

    // web (browser) server: bind the port first; if it's taken, stop instead of silently continuing.
    {
        let web_bind = std::env::var("RF_WEB_BIND").unwrap_or("127.0.0.1".into());
        let web_listener = tokio::net::TcpListener::bind(format!("{web_bind}:{web_port}"))
            .await
            .with_context(|| {
                format!("could not open web port {web_bind}:{web_port} (another application may be using it; change it with RF_WEB_PORT)")
            })?;
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = web_serve(web_listener, st).await {
                tracing::error!("web server stopped: {e:#}");
                std::process::exit(1);
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    let native_limit = std::env::var("RF_MAX_NATIVE_CONNECTIONS")
        .ok().and_then(|s| s.parse().ok()).filter(|n: &usize| *n > 0).unwrap_or(128);
    let native_slots = Arc::new(tokio::sync::Semaphore::new(native_limit));
    tracing::info!("rendezvous listening: 0.0.0.0:{port} (TLS: {}, limit: {native_limit})", tls.is_some());
    loop {
        let (tcp, peer) = listener.accept().await?;
        let permit = match native_slots.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("native connection limit reached; rejected {peer}");
                drop(tcp);
                continue;
            }
        };
        let Some(ip_slot) = take_ip_slot(&state, peer.ip()) else {
            tracing::warn!("too many open connections from {}; refused", peer.ip());
            continue;
        };
        let st = state.clone();
        let tls = tls.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ip_slot = ip_slot;
            if let Err(e) = handle_native(tcp, peer, st, tls).await {
                tracing::warn!("connection error {peer}: {e:#}");
            }
        });
    }
}

// ---- web (browser clients): one page, routed by ID ----

async fn web_serve(listener: tokio::net::TcpListener, state: Arc<State>) -> Result<()> {
    use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
    use axum::extract::State as AxState;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{Html, IntoResponse, Response};
    use axum::routing::get;
    use futures_util::{SinkExt, StreamExt};

    fn security_headers(response: &mut Response) {
        use axum::http::{HeaderName, HeaderValue};
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
        let mut response = Html(remote_friend_common::webapp::WEBAPP).into_response();
        security_headers(&mut response);
        response
    }

    async fn info() -> Response {
        let body = serde_json::json!({"role": "relay", "v": remote_friend_common::PROTOCOL_VERSION});
        let mut response = axum::Json(body).into_response();
        security_headers(&mut response);
        response
    }

    /// Real client IP behind nginx (the header is trusted only from a local proxy).
    fn client_ip(peer: std::net::SocketAddr, headers: &HeaderMap) -> std::net::IpAddr {
        if peer.ip().is_loopback() {
            if let Some(ip) = headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse().ok())
            {
                return ip;
            }
        }
        peer.ip()
    }

    async fn ws_handler(
        ws: WebSocketUpgrade,
        AxState(state): AxState<Arc<State>>,
        axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
        headers: HeaderMap,
    ) -> Response {
        if !origin_allowed(&headers) {
            tracing::warn!("WebSocket Origin rejected");
            return StatusCode::FORBIDDEN.into_response();
        }
        if !state.hello_allowed(client_ip(peer, &headers)) {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        let permit = match state.web_slots.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        let ip = client_ip(peer, &headers);
        ws.max_message_size(600 * 1024)
            .on_upgrade(move |socket| handle_web(socket, state, permit, ip))
            .into_response()
    }

    async fn handle_web(
        socket: WebSocket,
        state: Arc<State>,
        permit: tokio::sync::OwnedSemaphorePermit,
        ip: std::net::IpAddr,
    ) {
        let (mut sink, mut stream) = socket.split();
        // hello (15 s): {t:"hello", id:"123456789", password:"...", jpeg:true?, resume?}
        let hello = tokio::time::timeout(std::time::Duration::from_secs(15), stream.next()).await;
        let (id, password, jpeg) = match hello {
            Ok(Some(Ok(Message::Text(t)))) if t.len() <= 4096 => {
                serde_json::from_str::<serde_json::Value>(&t)
                    .ok()
                    .map(|v| {
                        let pw: String = v.get("password").and_then(|x| x.as_str())
                            .unwrap_or("").chars().take(200).collect();
                        // Reconnect / trusted-device tokens are forwarded to the host together with
                        // the password: [rf-resume:<32hex>:][rf-dev:<64hex>:]<password>
                        let hex = |k: &str, n: usize| {
                            v.get(k).and_then(|x| x.as_str())
                                .filter(|t| t.len() == n && t.bytes().all(|b| b.is_ascii_hexdigit()))
                                .map(str::to_string)
                        };
                        // Encrypted pages never send the password to the server; the computer
                        // checks it inside the end-to-end encrypted handshake.
                        let auth = if v.get("e2e").and_then(|x| x.as_bool()).unwrap_or(false) {
                            remote_friend_common::e2e::RELAY_AUTH_MARKER.to_string()
                        } else {
                            let _ = (hex("device", 64), hex("resume", 32));
                            pw
                        };
                        (
                            v.get("id").and_then(|x| x.as_str())
                                .map(|s| s.replace(' ', "")).unwrap_or_default(),
                            auth,
                            v.get("jpeg").and_then(|x| x.as_bool()).unwrap_or(false),
                        )
                    })
                    .unwrap_or_default()
            }
            _ => (String::new(), String::new(), false),
        };
        if !valid_id(&id) {
            let _ = sink.send(Message::Text(r#"{"t":"reject","msg":"a 9-digit ID is required"}"#.into())).await;
            return;
        }
        let host = state.hosts.lock().await.get(&id).map(|h| (h.name.clone(), h.cmd_tx.clone()));
        let Some((hname, cmd_tx)) = host else {
            let _ = sink.send(Message::Text(r#"{"t":"reject","msg":"ID not found (host is not online)"}"#.into())).await;
            return;
        };
        let pending_permit = match state.pending_slots.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                let _ = sink.send(Message::Text(r#"{"t":"reject","msg":"too many pending connections on the server"}"#.into())).await;
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
            addr: format!("browser {ip}"),
            kind: kind.into(),
            token,
            auth: Some(password),
        }).is_err() {
            reject_pending(&state, &tag, "host connection closed").await;
            return;
        }
        tracing::info!("{tag} (web) -> {id} ({hname}) awaiting authentication/approval");
        // from here on: host dial-back / reject / timeout take over
    }

    let app = axum::Router::new()
        .route("/", get(page))
        .route("/info", get(info))
        .route("/ws", get(ws_handler))
        .with_state(state);
    tracing::info!("web UI: {}", listener.local_addr()?);
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await?;
    Ok(())
}
