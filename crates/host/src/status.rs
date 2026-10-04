//! Shared host state for the desktop app (and the terminal).
//!
//! The host engine updates a snapshot (ID, password, server status, sessions, screen
//! permission, recent notices). A UI polls `snapshot()` and answers prompts
//! (incoming connections, first-time server trust) through `pending_prompts()` /
//! `answer()`. Without a UI, prompts are asked on the terminal.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScreenState {
    /// Screen capture and input are available.
    #[default]
    Ready,
    /// Waiting for the desktop's screen-sharing permission dialog (Wayland).
    WaitingPermission,
    /// The permission dialog was cancelled or failed.
    Denied,
    /// Screen is shared but remote control (mouse/keyboard) was not allowed.
    NoInput,
}

#[derive(Clone, Debug)]
pub struct Notice {
    /// "HH:MM" local-ish clock (UTC offset not applied; used only for display order).
    pub time: String,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub name: String,
    pub id: String,
    pub password: String,
    pub password_from_env: bool,
    /// Relay server address (host:port) if configured.
    pub server: Option<String>,
    pub online: bool,
    pub server_error: Option<String>,
    /// Address to open on phones/browsers (HTTPS via the relay).
    pub web_url: Option<String>,
    pub lan_url: String,
    pub sessions: usize,
    pub screen: ScreenState,
    pub trusted_devices: usize,
    pub auto_accept: bool,
    pub notices: Vec<Notice>,
}

static STATE: OnceLock<Mutex<Snapshot>> = OnceLock::new();

fn state() -> &'static Mutex<Snapshot> {
    STATE.get_or_init(|| Mutex::new(Snapshot::default()))
}

/// Current state (cheap clone; call every frame if needed).
pub fn snapshot() -> Snapshot {
    let mut s = state().lock().unwrap_or_else(|e| e.into_inner()).clone();
    s.trusted_devices = crate::approval::trusted_count();
    s
}

pub(crate) fn update(f: impl FnOnce(&mut Snapshot)) {
    let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut s);
}

fn clock() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let local = secs as i64 + local_offset_secs();
    let m = local.rem_euclid(86_400) / 60;
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// Best-effort local UTC offset (from `date +%z` on Unix); 0 if unknown.
fn local_offset_secs() -> i64 {
    static OFF: OnceLock<i64> = OnceLock::new();
    *OFF.get_or_init(|| {
        #[cfg(unix)]
        {
            if let Ok(out) = std::process::Command::new("date").arg("+%z").output() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if s.len() == 5 {
                    let sign = if s.starts_with('-') { -1 } else { 1 };
                    let h: i64 = s[1..3].parse().unwrap_or(0);
                    let m: i64 = s[3..5].parse().unwrap_or(0);
                    return sign * (h * 3600 + m * 60);
                }
            }
        }
        0
    })
}

/// Print a line without panicking when stdout is closed (e.g. a desktop launch whose
/// output pipe went away); `println!` would panic on EPIPE and kill the calling task.
pub(crate) fn say(text: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

/// Important event for the user: printed on the terminal and shown in the app.
pub fn notice(text: impl Into<String>) {
    let text = text.into();
    say(&text);
    update(|s| {
        s.notices.push(Notice { time: clock(), text });
        if s.notices.len() > 50 {
            s.notices.remove(0);
        }
    });
}

// ---- prompts answered by a UI ----

static UI_PROMPTS: AtomicBool = AtomicBool::new(false);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Call before starting the host when a graphical UI will answer prompts.
pub fn enable_ui_prompts() {
    UI_PROMPTS.store(true, Ordering::Relaxed);
}

pub(crate) fn ui_prompts() -> bool {
    UI_PROMPTS.load(Ordering::Relaxed)
}

#[derive(Clone, Debug)]
pub enum PromptKind {
    /// Someone with the correct password wants to connect.
    Connection { peer: String, can_remember: bool },
    /// First connection to a relay server whose certificate is not yet trusted.
    TrustServer { addr: String, fingerprint: String },
    /// The viewer offers to switch sides: this computer's user would control theirs.
    SwitchSides { label: String },
}

#[derive(Clone, Debug)]
pub struct Prompt {
    pub id: u64,
    pub kind: PromptKind,
    /// Seconds left before the prompt is answered "no" automatically.
    pub seconds_left: u64,
}

#[derive(Clone, Copy, Debug)]
pub enum Answer {
    Connection(crate::approval::Decision),
    Trust(bool),
    Switch(bool),
}

struct Pending {
    id: u64,
    kind: PromptKind,
    deadline: Instant,
    tx: std::sync::mpsc::Sender<Answer>,
}

static PENDING: Mutex<VecDeque<Pending>> = Mutex::new(VecDeque::new());
static PROMPT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Prompts waiting for the user (oldest first).
pub fn pending_prompts() -> Vec<Prompt> {
    let now = Instant::now();
    let mut q = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    q.retain(|p| p.deadline > now);
    q.iter()
        .map(|p| Prompt { id: p.id, kind: p.kind.clone(), seconds_left: (p.deadline - now).as_secs() })
        .collect()
}

/// Changes whenever a prompt is added (lets a UI bring its window to front once).
pub fn prompt_sequence() -> u64 {
    PROMPT_SEQ.load(Ordering::Relaxed)
}

pub fn answer(id: u64, answer: Answer) {
    let mut q = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pos) = q.iter().position(|p| p.id == id) {
        let p = q.remove(pos).expect("position is valid");
        let _ = p.tx.send(answer);
    }
}

/// Blocks the calling (non-async) thread until the UI answers or the timeout passes.
pub(crate) fn ask_ui(kind: PromptKind, timeout: Duration) -> Option<Answer> {
    let (tx, rx) = std::sync::mpsc::channel();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    PENDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(Pending { id, kind, deadline: Instant::now() + timeout, tx });
    PROMPT_SEQ.fetch_add(1, Ordering::Relaxed);
    let res = rx.recv_timeout(timeout).ok();
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).retain(|p| p.id != id);
    res
}

/// Desktop notification (Linux: notify-send, macOS: Notification Center).
pub(crate) fn desktop_notify(title: &str, msg: &str) {
    // The text ends up on a command line / in AppleScript: strip quotes and backslashes.
    let clean = |t: &str| t.chars().filter(|c| !matches!(c, '"' | '\\' | '\'' | '`' | '$')).collect::<String>();
    let (title, msg) = (clean(title), clean(msg));
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "RemoteFriend", &title, &msg])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("osascript")
        .args(["-e", &format!("display notification \"{msg}\" with title \"{title}\"")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = (title, msg);
}

// ---- switching sides ----

/// The app should now connect to this computer as a viewer (sides were switched).
#[derive(Clone, Debug)]
pub struct SwitchTarget {
    pub id: String,
    pub token: String,
    pub label: String,
}

static SWITCH: Mutex<Option<SwitchTarget>> = Mutex::new(None);

pub(crate) fn start_switch(t: SwitchTarget) {
    *SWITCH.lock().unwrap_or_else(|e| e.into_inner()) = Some(t);
}

/// Taken by the app, which then opens the viewer.
pub fn take_switch() -> Option<SwitchTarget> {
    SWITCH.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// "14:05" for today, otherwise "3 Oct 14:05" (local time, best effort).
pub fn local_time(unix: u64) -> String {
    let off = local_offset_secs();
    let local = unix as i64 + off;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) + off;
    let (day, mins) = (local.div_euclid(86_400), local.rem_euclid(86_400) / 60);
    let hm = format!("{:02}:{:02}", mins / 60, mins % 60);
    if day == now.div_euclid(86_400) {
        return hm;
    }
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{d} {} {hm}", MONTHS[(m - 1) as usize])
}

/// "45 s", "12 min", "1 h 5 min".
pub fn duration_text(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s} s"),
        s if s < 3600 => format!("{} min", s / 60),
        s => format!("{} h {} min", s / 3600, (s % 3600) / 60),
    }
}
