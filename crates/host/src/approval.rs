//! Connection approval. In the desktop app the UI shows Allow / Always allow / Deny;
//! on the terminal the operator types A / P / N. A single stdin reader thread is used so
//! a timed-out question can never "steal" the answer to the next one.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::status::{self, Answer, PromptKind};

static LINES: OnceLock<Mutex<Receiver<String>>> = OnceLock::new();
static ASK_LOCK: Mutex<()> = Mutex::new(());
const ASK_TIMEOUT: Duration = Duration::from_secs(30);

fn lines() -> &'static Mutex<Receiver<String>> {
    LINES.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("rf-stdin".into())
            .spawn(move || {
                let stdin = std::io::stdin();
                let mut line = String::new();
                loop {
                    line.clear();
                    match stdin.read_line(&mut line) {
                        Ok(0) | Err(_) => break, // no stdin (service / desktop app)
                        Ok(_) => {
                            if tx.send(line.trim().to_string()).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .expect("failed to start stdin thread");
        Mutex::new(rx)
    })
}

fn yes(s: &str) -> bool {
    matches!(s.to_lowercase().as_str(), "y" | "yes" | "a" | "allow")
}

fn notify_desktop(peer: &str) {
    // The text ends up on a command line / in AppleScript: strip quotes and backslashes.
    let peer: String = peer.chars().filter(|c| !matches!(c, '"' | '\\' | '\'' | '`' | '$')).collect();
    let msg = if status::ui_prompts() {
        format!("{peer} wants to connect. Open RemoteFriend to allow or deny.")
    } else {
        format!("{peer} wants to connect. Answer in the RemoteFriend terminal.")
    };
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("notify-send")
        .args(["-u", "critical", "-a", "RemoteFriend", "RemoteFriend: connection request", &msg])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("osascript")
        .args(["-e", &format!("display notification \"{msg}\" with title \"RemoteFriend\"")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = msg;
}

/// The operator's decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Deny,
    /// This connection only.
    Once,
    /// This device may connect from now on without approval and without the password.
    Always,
}

/// Ask the operator about a connection request (the password was already verified).
/// `can_remember`: the client can store a device token (browser), so "always" is offered.
pub(crate) fn ask(peer: &str, can_remember: bool) -> Decision {
    if std::env::var("REMOTE_FRIEND_AUTO_ACCEPT").map(|v| v == "1").unwrap_or(false) {
        status::notice(format!("{peer}: accepted automatically (REMOTE_FRIEND_AUTO_ACCEPT=1)"));
        return Decision::Once;
    }
    notify_desktop(peer);
    let decision = if status::ui_prompts() {
        match status::ask_ui(PromptKind::Connection { peer: peer.to_string(), can_remember }, ASK_TIMEOUT) {
            Some(Answer::Connection(d)) => d,
            _ => Decision::Deny,
        }
    } else {
        let options = if can_remember {
            "A = allow once / P = allow permanently (this device won't be asked again) / N = deny"
        } else {
            "A = allow / N = deny"
        };
        let answer = prompt_line(
            &format!("*** Connection request: {peer}\n*** {options}  (30 s, default: deny)"),
            ASK_TIMEOUT,
        );
        match answer.as_deref().map(|s| s.to_lowercase()) {
            Some(a) if can_remember && matches!(a.as_str(), "p" | "permanent" | "always") => Decision::Always,
            Some(a) if yes(&a) || a == "p" => Decision::Once,
            _ => Decision::Deny,
        }
    };
    status::notice(format!(
        "{peer}: {}",
        match decision {
            Decision::Deny => "denied",
            Decision::Once => "allowed (this time)",
            Decision::Always => "allowed permanently (this device won't be asked again)",
        }
    ));
    decision
}

/// First connection to a relay server: trust its certificate fingerprint?
pub(crate) fn trust_server(addr: &str, fingerprint: &str) -> bool {
    if status::ui_prompts() {
        let kind = PromptKind::TrustServer { addr: addr.to_string(), fingerprint: fingerprint.to_string() };
        return matches!(status::ask_ui(kind, Duration::from_secs(300)), Some(Answer::Trust(true)));
    }
    let question = format!(
        "*** First connection to server {addr}\n*** Certificate fingerprint: {fingerprint}\n*** Does it match the fingerprint printed at the end of the VPS setup? Trust and remember it? (y/n, 120 s)"
    );
    prompt_line(&question, Duration::from_secs(120)).is_some_and(|l| yes(&l))
}

/// Ask a terminal question and return the answer line (timeout / no stdin -> None).
fn prompt_line(question: &str, timeout: Duration) -> Option<String> {
    let _guard = ASK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let rx = lines().lock().unwrap_or_else(|e| e.into_inner());
    while rx.try_recv().is_ok() {}
    status::say(&format!("\x07{question}"));
    match rx.recv_timeout(timeout) {
        Ok(line) => Some(line),
        Err(RecvTimeoutError::Timeout) => {
            status::say("*** timed out: treated as NO");
            None
        }
        Err(RecvTimeoutError::Disconnected) => {
            status::say("*** no terminal input: cannot ask (use the desktop app, trusted devices or REMOTE_FRIEND_AUTO_ACCEPT=1)");
            None
        }
    }
}

// ---- trusted devices ----
//
// When the operator chooses "Always allow", the device receives a 256-bit token. The
// computer stores (~/.config/remotefriend/trusted_devices.json, 0600) its SHA-256 plus a
// salted PBKDF2 key of it, which lets the device log in end-to-end encrypted without the
// computer's password (that one changes after every session). Entries from before v0.9
// only have the hash; they are upgraded the next time the device logs in with the password.

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Device {
    hash: String,
    label: String,
    added: u64,
    /// Public device id (see e2e::device_id); empty for entries from before v0.9.
    #[serde(default)]
    id: String,
    #[serde(default)]
    salt: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    last_used: u64,
}

/// A trusted device as shown in the app.
#[derive(Clone, Debug)]
pub struct TrustedDevice {
    /// Stable handle for `remove_device`.
    pub handle: String,
    pub label: String,
    pub added: u64,
    pub last_used: u64,
}

/// Key material for a device login.
pub(crate) struct DeviceKey {
    pub salt: [u8; remote_friend_common::e2e::SALT_LEN],
    pub key: [u8; 32],
    pub label: String,
    /// One-time grant (switching sides): consumed by the login.
    pub temporary: bool,
}

static DEVICES_LOCK: Mutex<()> = Mutex::new(());

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn devices_path() -> std::path::PathBuf {
    remote_friend_common::identity::config_dir().join("trusted_devices.json")
}

fn load_devices() -> Vec<Device> {
    std::fs::read(devices_path())
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default()
}

fn save_devices(list: &[Device]) {
    let data = serde_json::to_vec_pretty(list).unwrap_or_default();
    if let Err(e) = remote_friend_common::identity::write_private(&devices_path(), &data) {
        tracing::warn!("could not write trusted device list: {e}");
    }
}

fn token_hash(token: &str) -> Option<String> {
    let token = token.trim().to_ascii_lowercase();
    (token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| remote_friend_common::fingerprint_full(token.as_bytes()))
}

fn with_login_key(mut d: Device, token: &str) -> Device {
    let (salt, key) = remote_friend_common::e2e::device_key(token);
    d.id = remote_friend_common::e2e::device_id(token);
    d.salt = hex::encode(salt);
    d.key = hex::encode(key);
    d
}

/// Add a trusted device and return the token to hand to the client.
pub(crate) fn trust_device(label: &str) -> String {
    let token = remote_friend_common::new_secret_hex().to_ascii_lowercase();
    let device = Device {
        hash: token_hash(&token).expect("64 hex chars"),
        label: label.chars().take(80).collect(),
        added: now_secs(),
        id: String::new(),
        salt: String::new(),
        key: String::new(),
        last_used: now_secs(),
    };
    let device = with_login_key(device, &token);
    let _g = DEVICES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = load_devices();
    if list.len() >= 32 {
        list.remove(0);
    }
    list.push(device);
    save_devices(&list);
    token
}

/// Password login that also presented a device token: is it a trusted device? Entries from
/// before v0.9 get their login key now, so the device can log in without the password next time.
pub(crate) fn is_trusted(token: &str) -> bool {
    let Some(hash) = token_hash(token) else { return false };
    let _g = DEVICES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = load_devices();
    let Some(pos) = list.iter().position(|d| remote_friend_common::constant_time_eq(d.hash.as_bytes(), hash.as_bytes()))
    else {
        return false;
    };
    let mut d = list[pos].clone();
    if d.key.is_empty() {
        d = with_login_key(d, token);
    }
    d.last_used = now_secs();
    list[pos] = d;
    save_devices(&list);
    true
}

/// Key material for a device login, by public device id.
pub(crate) fn device_login_key(id: &str) -> Option<DeviceKey> {
    if let Some(k) = take_temporary(id) {
        return Some(k);
    }
    let _g = DEVICES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = load_devices().into_iter().find(|d| !d.id.is_empty() && d.id == id)?;
    let salt = hex::decode(&d.salt).ok()?.try_into().ok()?;
    let key = hex::decode(&d.key).ok()?.try_into().ok()?;
    Some(DeviceKey { salt, key, label: d.label, temporary: false })
}

/// A successful device login: remember when the device was last used.
pub(crate) fn device_used(id: &str) {
    let _g = DEVICES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = load_devices();
    if let Some(d) = list.iter_mut().find(|d| d.id == id) {
        d.last_used = now_secs();
        save_devices(&list);
    }
}

pub fn trusted_count() -> usize {
    load_devices().len()
}

/// Trusted devices, most recently used first.
pub fn trusted_devices() -> Vec<TrustedDevice> {
    let mut list: Vec<TrustedDevice> = load_devices()
        .into_iter()
        .map(|d| TrustedDevice { handle: d.hash, label: d.label, added: d.added, last_used: d.last_used.max(d.added) })
        .collect();
    list.sort_by(|a, b| b.last_used.cmp(&a.last_used));
    list
}

/// Remove one trusted device (it will need the password and approval again).
pub fn remove_device(handle: &str) -> bool {
    let _g = DEVICES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = load_devices();
    let before = list.len();
    list.retain(|d| d.hash != handle);
    let removed = list.len() != before;
    if removed {
        save_devices(&list);
    }
    removed
}

/// Forget all trusted devices. Returns how many were removed.
pub(crate) fn forget_devices() -> usize {
    let _g = DEVICES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let n = load_devices().len();
    let _ = std::fs::remove_file(devices_path());
    n
}

// One-time grants: "switch sides" lets the computer we are controlling connect back to us
// once, within a few minutes, without our password and without asking.
static TEMPORARY: Mutex<Vec<(String, DeviceKeyStored, std::time::Instant)>> = Mutex::new(Vec::new());
const TEMPORARY_TTL: Duration = Duration::from_secs(180);

struct DeviceKeyStored {
    salt: [u8; remote_friend_common::e2e::SALT_LEN],
    key: [u8; 32],
    label: String,
}

/// Grant a one-time login and return its token.
pub fn grant_temporary(label: &str) -> String {
    let token = remote_friend_common::new_secret_hex().to_ascii_lowercase();
    let (salt, key) = remote_friend_common::e2e::device_key(&token);
    let id = remote_friend_common::e2e::device_id(&token);
    let mut list = TEMPORARY.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|(_, _, t)| t.elapsed() < TEMPORARY_TTL);
    list.push((id, DeviceKeyStored { salt, key, label: label.chars().take(80).collect() }, std::time::Instant::now()));
    token
}

fn take_temporary(id: &str) -> Option<DeviceKey> {
    let mut list = TEMPORARY.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|(_, _, t)| t.elapsed() < TEMPORARY_TTL);
    let pos = list.iter().position(|(i, _, _)| i == id)?;
    let (_, k, _) = list.remove(pos);
    Some(DeviceKey { salt: k.salt, key: k.key, label: k.label, temporary: true })
}

// ---- resume token ----
//
// When an approved session drops briefly (phone switched networks, tab went to the
// background) nobody should have to approve it again. Single use, valid for 10 minutes,
// and required TOGETHER with the password (it does not replace it).

static RESUME: Mutex<Vec<(String, std::time::Instant)>> = Mutex::new(Vec::new());
const RESUME_TTL: Duration = Duration::from_secs(600);

pub(crate) fn grant_resume() -> String {
    let token = remote_friend_common::new_secret_hex()[..32].to_string();
    let mut list = RESUME.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|(_, t)| t.elapsed() < RESUME_TTL);
    if list.len() >= 16 {
        list.remove(0);
    }
    list.push((token.clone(), std::time::Instant::now()));
    token
}

/// Consumes the token if valid and returns true.
pub(crate) fn consume_resume(token: &str) -> bool {
    if token.len() != 32 {
        return false;
    }
    let mut list = RESUME.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|(_, t)| t.elapsed() < RESUME_TTL);
    let pos = list
        .iter()
        .position(|(t, _)| remote_friend_common::constant_time_eq(t.as_bytes(), token.as_bytes()));
    match pos {
        Some(i) => {
            list.remove(i);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_tokens_are_single_use() {
        let t = grant_resume();
        assert!(consume_resume(&t));
        assert!(!consume_resume(&t));
        assert!(!consume_resume("00000000000000000000000000000000"));
    }
}
