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
    matches!(s.to_lowercase().as_str(), "y" | "yes" | "a" | "allow" | "e" | "evet")
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
    /// This device may connect without approval from now on (password still required).
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
            Some(a) if can_remember && matches!(a.as_str(), "p" | "permanent" | "always" | "k") => Decision::Always,
            Some(a) if yes(&a) || matches!(a.as_str(), "p" | "k") => Decision::Once,
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
    println!("\x07{question}");
    match rx.recv_timeout(timeout) {
        Ok(line) => Some(line),
        Err(RecvTimeoutError::Timeout) => {
            println!("*** timed out: treated as NO");
            None
        }
        Err(RecvTimeoutError::Disconnected) => {
            println!("*** no terminal input: cannot ask (use the desktop app, trusted devices or REMOTE_FRIEND_AUTO_ACCEPT=1)");
            None
        }
    }
}

// ---- trusted devices ----
//
// When the operator allows a browser permanently, the device receives a 256-bit token.
// The host stores only its SHA-256 (~/.config/remotefriend/trusted_devices.json, 0600).
// The token does not replace the password; it only skips the approval step.

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Device {
    hash: String,
    label: String,
    added: u64,
}

static DEVICES_LOCK: Mutex<()> = Mutex::new(());

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

/// Add a trusted device and return the token to hand to the client.
pub(crate) fn trust_device(label: &str) -> String {
    let token = remote_friend_common::new_secret_hex();
    let _g = DEVICES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = load_devices();
    if list.len() >= 32 {
        list.remove(0);
    }
    list.push(Device {
        hash: remote_friend_common::fingerprint_full(token.as_bytes()),
        label: label.chars().take(80).collect(),
        added: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    });
    save_devices(&list);
    token
}

/// Does the token belong to a trusted device?
pub(crate) fn is_trusted(token: &str) -> bool {
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let hash = remote_friend_common::fingerprint_full(token.to_ascii_lowercase().as_bytes());
    let _g = DEVICES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load_devices()
        .iter()
        .fold(false, |found, d| found | remote_friend_common::constant_time_eq(d.hash.as_bytes(), hash.as_bytes()))
}

pub fn trusted_count() -> usize {
    load_devices().len()
}

/// Forget all trusted devices. Returns how many were removed.
pub(crate) fn forget_devices() -> usize {
    let _g = DEVICES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let n = load_devices().len();
    let _ = std::fs::remove_file(devices_path());
    n
}

// ---- resume token ----
//
// When an approved session drops briefly (phone switched networks, tab went to the
// background) nobody should have to approve it again. Single use, valid for 10 minutes,
// and required TOGETHER with the password (it does not replace it).

static RESUME: Mutex<Vec<(String, std::time::Instant)>> = Mutex::new(Vec::new());
const RESUME_TTL: Duration = Duration::from_secs(600);
/// On the relay path the token is prefixed to the password: "rf-resume:<32 hex>:<password>".
const RESUME_PREFIX: &str = "rf-resume:";
const DEVICE_PREFIX: &str = "rf-dev:";

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

/// Relay auth data: [rf-resume:<32hex>:][rf-dev:<64hex>:]<password>
pub(crate) struct RelayAuth<'a> {
    pub password: &'a str,
    pub resume: Option<&'a str>,
    pub device: Option<&'a str>,
}

pub(crate) fn split_relay_auth(auth: &str) -> RelayAuth<'_> {
    let mut out = RelayAuth { password: auth, resume: None, device: None };
    loop {
        let rest = out.password;
        let hex_ok = |t: &str, n: usize| t.len() == n && t.bytes().all(|b| b.is_ascii_hexdigit());
        if let Some(r) = rest.strip_prefix(RESUME_PREFIX) {
            if let Some((t, p)) = r.split_once(':') {
                if hex_ok(t, 32) && out.resume.is_none() {
                    out.resume = Some(t);
                    out.password = p;
                    continue;
                }
            }
        }
        if let Some(r) = rest.strip_prefix(DEVICE_PREFIX) {
            if let Some((t, p)) = r.split_once(':') {
                if hex_ok(t, 64) && out.device.is_none() {
                    out.device = Some(t);
                    out.password = p;
                    continue;
                }
            }
        }
        return out;
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

    #[test]
    fn relay_auth_split() {
        let tok = "0123456789abcdef0123456789abcdef";
        let dev = "ab".repeat(32);
        let raw = format!("rf-resume:{tok}:rf-dev:{dev}:gizli:sifre");
        let a = split_relay_auth(&raw);
        assert_eq!((a.password, a.resume, a.device), ("gizli:sifre", Some(tok), Some(dev.as_str())));
        let b = split_relay_auth("normal-sifre");
        assert_eq!((b.password, b.resume, b.device), ("normal-sifre", None, None));
        let c = split_relay_auth("rf-resume:kisa:x");
        assert_eq!((c.password, c.resume), ("rf-resume:kisa:x", None));
    }
}
