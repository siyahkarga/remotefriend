//! Password check + brute-force limits (all paths: LAN web, native, internet).
//!
//! - Per source (IP): 5 failures within 60 s lock that source for 1 min, then 2/4/8/16 min.
//!   The level resets after an hour without failures, so someone who knows the ID locks out
//!   their own IP, not the owner.
//! - Global cap (distributed guessing): 30 failures within 10 min lock everyone for 5 min.
//! - While locked, the password is NOT evaluated at all (otherwise the lock would be an oracle).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

struct Secret {
    value: String,
    /// Generated password: dashes, spaces and letter case are ignored.
    generated: bool,
}

static SECRET: RwLock<Option<Secret>> = RwLock::new(None);

#[derive(Default)]
struct Source {
    fails: VecDeque<Instant>,
    locked_until: Option<Instant>,
    locks: u32,
    last_fail: Option<Instant>,
}

struct Limiter {
    sources: HashMap<String, Source>,
    global_fails: VecDeque<Instant>,
    global_until: Option<Instant>,
}

static LIMITER: OnceLock<Mutex<Limiter>> = OnceLock::new();

fn limiter() -> &'static Mutex<Limiter> {
    LIMITER.get_or_init(|| Mutex::new(Limiter { sources: HashMap::new(), global_fails: VecDeque::new(), global_until: None }))
}

const WINDOW: Duration = Duration::from_secs(60);
const MAX_FAILS: usize = 5;
const DECAY: Duration = Duration::from_secs(3600);
const GLOBAL_WINDOW: Duration = Duration::from_secs(600);
const GLOBAL_MAX_FAILS: usize = 30;
const GLOBAL_LOCK: Duration = Duration::from_secs(300);

pub(crate) enum Auth {
    Ok,
    Bad,
    /// Remaining lock time (seconds).
    Locked(u64),
}

fn password_path() -> std::path::PathBuf {
    remote_friend_common::identity::config_dir().join("password")
}

/// Read the saved (persistent) generated password; ignore it if malformed.
fn load_saved_password() -> Option<String> {
    let p = std::fs::read_to_string(password_path()).ok()?;
    let p = p.trim().to_string();
    let n = remote_friend_common::normalize_password(&p);
    (n.len() == 10 && n.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(p)
}

/// When the password changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordPolicy {
    /// Each time RemoteFriend starts (default).
    OnStart,
    /// After every session that used it.
    AfterSession,
    /// Only by hand ("New password").
    Never,
}

pub fn policy() -> PasswordPolicy {
    if std::env::var("REMOTE_FRIEND_PASS").is_ok_and(|v| !v.trim().is_empty()) {
        return PasswordPolicy::Never;
    }
    let s = remote_friend_common::identity::load_host_settings();
    match s.password_change.as_str() {
        "session" => PasswordPolicy::AfterSession,
        "never" => PasswordPolicy::Never,
        "start" => PasswordPolicy::OnStart,
        _ if s.fixed_password => PasswordPolicy::Never,
        _ => PasswordPolicy::OnStart,
    }
}

pub fn set_policy(p: PasswordPolicy) {
    let mut s = remote_friend_common::identity::load_host_settings();
    s.password_change = match p {
        PasswordPolicy::OnStart => "start",
        PasswordPolicy::AfterSession => "session",
        PasswordPolicy::Never => "never",
    }
    .into();
    s.fixed_password = false;
    remote_friend_common::identity::save_host_settings(&s);
}

/// Set up the password once. Returns (password to show, came from the environment).
///
/// Priority: REMOTE_FRIEND_PASS > saved persistent password > newly generated (saved).
/// When the password rotates, every start gets a new one too.
/// `new_password` (or RF_NEW_PASSWORD=1) renews the saved password.
/// RF_EPHEMERAL_PASSWORD=1: a new password on every start (not saved).
pub(crate) fn init(new_password: bool) -> (String, bool) {
    let configured = std::env::var("REMOTE_FRIEND_PASS").ok().filter(|s| !s.trim().is_empty());
    let ephemeral = std::env::var("RF_EPHEMERAL_PASSWORD").map(|v| v == "1").unwrap_or(false);
    let renew = new_password || std::env::var("RF_NEW_PASSWORD").map(|v| v == "1").unwrap_or(false);
    let secret = match &configured {
        Some(p) => Secret { value: p.trim().to_string(), generated: false },
        None if ephemeral => Secret { value: remote_friend_common::new_session_password(), generated: true },
        None => {
            let keep = policy() == PasswordPolicy::Never;
            let value = match load_saved_password().filter(|_| !renew && keep) {
                Some(p) => p,
                None => {
                    let p = remote_friend_common::new_session_password();
                    save_password(&p);
                    p
                }
            };
            Secret { value, generated: true }
        }
    };
    if !secret.generated && secret.value.chars().count() < 10 {
        tracing::warn!("weak REMOTE_FRIEND_PASS: use at least 10 characters");
    }
    let shown = secret.value.clone();
    *SECRET.write().unwrap_or_else(|e| e.into_inner()) = Some(secret);
    (shown, configured.is_some())
}

fn save_password(p: &str) {
    if let Err(e) = remote_friend_common::identity::write_private(&password_path(), p.as_bytes()) {
        tracing::warn!("could not save the password (it will only last until restart): {e}");
    }
}

/// Generate and save a new password (desktop app "new password" button).
/// Returns None when the password comes from REMOTE_FRIEND_PASS.
pub fn renew_password() -> Option<String> {
    let mut guard = SECRET.write().unwrap_or_else(|e| e.into_inner());
    if guard.as_ref().is_some_and(|s| !s.generated) {
        return None;
    }
    let p = remote_friend_common::new_session_password();
    save_password(&p);
    *guard = Some(Secret { value: p.clone(), generated: true });
    drop(guard);
    crate::status::update(|s| s.password = p.clone());
    Some(p)
}

fn matches(candidate: &str) -> bool {
    let guard = SECRET.read().unwrap_or_else(|e| e.into_inner());
    let Some(secret) = guard.as_ref() else { return false };
    if secret.generated {
        let a = remote_friend_common::normalize_password(candidate);
        let b = remote_friend_common::normalize_password(&secret.value);
        remote_friend_common::constant_time_eq(a.as_bytes(), b.as_bytes())
    } else {
        remote_friend_common::constant_time_eq(candidate.trim().as_bytes(), secret.value.as_bytes())
    }
}

// ---- a new password after every session ----

/// Someone logged in with the current password since it was last changed.
static PASSWORD_USED: AtomicBool = AtomicBool::new(false);
/// Bumped by every session start: cancels a pending change.
static ROTATE_GEN: AtomicU64 = AtomicU64::new(0);
/// A viewer whose connection dropped can come back with the same password this long.
const ROTATE_GRACE: Duration = Duration::from_secs(60);

/// The password was just proven by a viewer.
pub(crate) fn password_login() {
    PASSWORD_USED.store(true, Ordering::Relaxed);
}

pub(crate) fn session_started() {
    ROTATE_GEN.fetch_add(1, Ordering::Relaxed);
}

/// A session ended (or a password login was denied). Once nobody is connected any more, a
/// used password is replaced: at once after a normal goodbye, otherwise after a short grace.
pub(crate) fn session_ended(used_password: bool, bye: bool) {
    if used_password {
        PASSWORD_USED.store(true, Ordering::Relaxed);
    }
    if !PASSWORD_USED.load(Ordering::Relaxed) || policy() != PasswordPolicy::AfterSession || crate::sessions::count() > 0 {
        return;
    }
    let gen = ROTATE_GEN.fetch_add(1, Ordering::Relaxed) + 1;
    let wait = if bye { Duration::from_millis(300) } else { ROTATE_GRACE };
    let _ = std::thread::Builder::new().name("rf-rotate".into()).spawn(move || {
        std::thread::sleep(wait);
        if ROTATE_GEN.load(Ordering::Relaxed) == gen
            && crate::sessions::count() == 0
            && PASSWORD_USED.swap(false, Ordering::Relaxed)
            && renew_password().is_some()
        {
            crate::status::notice("New password: the previous one was used for a session that has ended.");
        }
    });
}

/// Source key: "1.2.3.4:5678" -> "1.2.3.4"; other text as is.
pub(crate) fn source_key(peer: &str) -> String {
    peer.parse::<std::net::SocketAddr>()
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| peer.chars().take(80).collect())
}

/// The secret for the encrypted handshake: (password, compare normalized).
pub(crate) fn e2e_secret() -> Option<(String, bool)> {
    let guard = SECRET.read().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().map(|s| (s.value.clone(), s.generated))
}

/// Remaining lock time for this source (or globally), if locked.
pub(crate) fn locked(source: &str) -> Option<u64> {
    let now = Instant::now();
    let key = source_key(source);
    let mut lim = limiter().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(until) = lim.global_until {
        if until > now {
            return Some((until - now).as_secs().max(1));
        }
        lim.global_until = None;
    }
    let src = lim.sources.entry(key).or_default();
    if src.last_fail.is_some_and(|t| now.duration_since(t) > DECAY) {
        *src = Source::default();
    }
    match src.locked_until {
        Some(until) if until > now => Some((until - now).as_secs().max(1)),
        _ => {
            src.locked_until = None;
            None
        }
    }
}

/// Record the outcome of a password proof from `source`.
pub(crate) fn record(source: &str, ok: bool) {
    let now = Instant::now();
    let key = source_key(source);
    let mut lim = limiter().lock().unwrap_or_else(|e| e.into_inner());
    if ok {
        if let Some(src) = lim.sources.get_mut(&key) {
            src.fails.clear();
        }
        return;
    }
    let src = lim.sources.entry(key.clone()).or_default();
    while src.fails.front().is_some_and(|t| now.duration_since(*t) > WINDOW) {
        src.fails.pop_front();
    }
    src.fails.push_back(now);
    src.last_fail = Some(now);
    if src.fails.len() >= MAX_FAILS {
        let mins = 1u64 << src.locks.min(4);
        src.locks += 1;
        src.fails.clear();
        src.locked_until = Some(now + Duration::from_secs(60 * mins));
        crate::status::notice(format!("{key}: too many wrong passwords; this source is locked for {mins} min"));
    }
    while lim.global_fails.front().is_some_and(|t| now.duration_since(*t) > GLOBAL_WINDOW) {
        lim.global_fails.pop_front();
    }
    lim.global_fails.push_back(now);
    if lim.global_fails.len() >= GLOBAL_MAX_FAILS {
        lim.global_fails.clear();
        lim.global_until = Some(now + GLOBAL_LOCK);
        crate::status::notice("Wrong passwords from many sources: all connections locked for 5 min");
    }
    // Prune old entries (memory bound).
    if lim.sources.len() > 1024 {
        lim.sources.retain(|_, s| s.locked_until.is_some_and(|u| u > now) || s.last_fail.is_some_and(|t| now.duration_since(t) < WINDOW));
    }
}

/// Plain password check (legacy, unencrypted local-network browser page).
pub(crate) fn check_password(candidate: &str, source: &str) -> Auth {
    if let Some(secs) = locked(source) {
        return Auth::Locked(secs);
    }
    let ok = matches(candidate);
    record(source, ok);
    if ok { Auth::Ok } else { Auth::Bad }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_source_lockout_does_not_lock_others() {
        *SECRET.write().unwrap() = Some(Secret { value: "abcde-23456".into(), generated: true });
        for _ in 0..MAX_FAILS {
            assert!(matches!(check_password("yanlis", "10.0.0.1:1000"), Auth::Bad));
        }
        // The attacker's source is locked: even the right password is not evaluated.
        assert!(matches!(check_password("abcde-23456", "10.0.0.1:2000"), Auth::Locked(_)));
        // Other sources are unaffected; formatting differences don't matter.
        assert!(matches!(check_password(" ABCDE 23456 ", "10.0.0.2:1000"), Auth::Ok));
    }

    #[test]
    fn source_key_strips_port() {
        assert_eq!(source_key("1.2.3.4:5678"), "1.2.3.4");
        assert_eq!(source_key("[::1]:80"), "::1");
        assert_eq!(source_key("browser 1.2.3.4"), "browser 1.2.3.4");
    }
}
