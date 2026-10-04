//! Live remote sessions: who is connected, what they may do, and who connected recently.
//!
//! Every session registers here and gets a `SessionHandle`. The desktop app lists the
//! sessions, changes their permissions, disconnects them, or takes control back for all.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{watch, Notify};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Browser,
    App,
}

/// What a viewer may do; the computer's user can change it during the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Perms {
    /// Mouse and keyboard.
    pub control: bool,
    pub sound: bool,
    pub files: bool,
    pub clipboard: bool,
}

impl Default for Perms {
    fn default() -> Self {
        Self { control: true, sound: true, files: true, clipboard: true }
    }
}

impl Perms {
    pub(crate) fn to_json(self, blocked: bool) -> serde_json::Value {
        serde_json::json!({
            "t": "perms",
            "control": self.control && !blocked,
            "sound": self.sound,
            "files": self.files,
            "clipboard": self.clipboard,
            "blocked": blocked,
        })
    }
}

/// A file being received from a viewer.
#[derive(Clone, Debug)]
pub struct Transfer {
    pub name: String,
    pub got: u64,
    pub total: u64,
}

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub id: u64,
    /// Device name sent by the viewer ("Chrome on Android", a computer name) or its address.
    pub label: String,
    /// Network address and path ("83.135.241.223 (via internet)").
    pub peer: String,
    pub via: Via,
    /// Unix seconds.
    pub since: u64,
    /// Logged in as a trusted device (no password, no approval).
    pub trusted: bool,
    pub perms: Perms,
    pub transfer: Option<Transfer>,
    /// The viewer is a RemoteFriend app that can switch sides.
    pub can_switch: bool,
    /// Traffic flows directly (peer-to-peer), not through the relay server.
    pub direct: bool,
}

/// Finished session, for the "Recent" list.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RecentSession {
    pub label: String,
    pub peer: String,
    pub app: bool,
    pub start: u64,
    pub secs: u64,
    pub trusted: bool,
}

struct Live {
    info: SessionInfo,
    perms_tx: watch::Sender<Perms>,
    kill: Arc<Notify>,
    switch_req: Arc<Notify>,
}

static LIVE: Mutex<Vec<Live>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);
const MAX_RECENT: usize = 30;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn live() -> std::sync::MutexGuard<'static, Vec<Live>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// "Take back control": while set, no viewer can move the mouse or type.
fn blocked_tx() -> &'static watch::Sender<bool> {
    static TX: OnceLock<watch::Sender<bool>> = OnceLock::new();
    TX.get_or_init(|| watch::channel(false).0)
}

pub fn set_input_blocked(on: bool) {
    blocked_tx().send_replace(on);
    crate::status::notice(if on {
        "Remote control paused: viewers can only watch."
    } else {
        "Remote control allowed again."
    });
}

pub fn input_blocked() -> bool {
    *blocked_tx().borrow()
}

fn update_count() {
    let n = live().len();
    crate::status::update(|s| s.sessions = n);
}

/// A session's link to the registry; dropping it ends the session's entry.
pub(crate) struct SessionHandle {
    pub id: u64,
    perms_rx: watch::Receiver<Perms>,
    blocked_rx: watch::Receiver<bool>,
    kill: Arc<Notify>,
    switch_req: Arc<Notify>,
    started: Instant,
    /// Logged in with the computer's password (rotates it when the session ends).
    pub used_password: bool,
    /// The viewer said goodbye (Disconnect button), not a network drop.
    pub bye: bool,
}

pub(crate) fn register(label: &str, peer: &str, via: Via, trusted: bool, used_password: bool) -> SessionHandle {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (perms_tx, perms_rx) = watch::channel(Perms::default());
    let kill = Arc::new(Notify::new());
    let switch_req = Arc::new(Notify::new());
    let label = if label.trim().is_empty() { peer.to_string() } else { label.trim().chars().take(60).collect() };
    live().push(Live {
        info: SessionInfo {
            id,
            label,
            peer: peer.to_string(),
            via,
            since: now_secs(),
            trusted,
            perms: Perms::default(),
            transfer: None,
            can_switch: false,
            direct: false,
        },
        perms_tx,
        kill: kill.clone(),
        switch_req: switch_req.clone(),
    });
    update_count();
    crate::auth::session_started();
    SessionHandle {
        id,
        perms_rx,
        blocked_rx: blocked_tx().subscribe(),
        kill,
        switch_req,
        started: Instant::now(),
        used_password,
        bye: false,
    }
}

impl SessionHandle {
    pub(crate) fn perms(&self) -> Perms {
        *self.perms_rx.borrow()
    }

    pub(crate) fn blocked(&self) -> bool {
        *self.blocked_rx.borrow()
    }

    /// May the viewer use mouse and keyboard right now?
    pub(crate) fn control_allowed(&self) -> bool {
        self.perms().control && !self.blocked()
    }

    /// The permissions or the global pause changed; returns the JSON to tell the viewer.
    pub(crate) async fn changed(&mut self) -> serde_json::Value {
        tokio::select! {
            r = self.perms_rx.changed() => if r.is_err() { std::future::pending::<()>().await },
            r = self.blocked_rx.changed() => if r.is_err() { std::future::pending::<()>().await },
        }
        self.perms().to_json(self.blocked())
    }

    /// Signalled when the computer's user presses Disconnect for this session.
    pub(crate) fn kill_signal(&self) -> Arc<Notify> {
        self.kill.clone()
    }

    /// Signalled when the computer's user asks this viewer to switch sides.
    pub(crate) fn switch_signal(&self) -> Arc<Notify> {
        self.switch_req.clone()
    }

    pub(crate) fn set_can_switch(&self, can: bool) {
        if let Some(l) = live().iter_mut().find(|l| l.info.id == self.id) {
            l.info.can_switch = can;
        }
    }

    pub(crate) fn label(&self) -> String {
        live().iter().find(|l| l.info.id == self.id).map(|l| l.info.label.clone()).unwrap_or_default()
    }

    pub(crate) fn set_transfer(&self, t: Option<Transfer>) {
        if let Some(l) = live().iter_mut().find(|l| l.info.id == self.id) {
            l.info.transfer = t;
        }
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        let info = {
            let mut list = live();
            let pos = list.iter().position(|l| l.info.id == self.id);
            pos.map(|p| list.remove(p).info)
        };
        update_count();
        if let Some(info) = info {
            push_recent(RecentSession {
                label: info.label,
                peer: info.peer,
                app: info.via == Via::App,
                start: info.since,
                secs: self.started.elapsed().as_secs(),
                trusted: info.trusted,
            });
        }
        crate::auth::session_ended(self.used_password, self.bye);
    }
}

pub(crate) fn set_direct(id: u64, on: bool) {
    if let Some(l) = live().iter_mut().find(|l| l.info.id == id) {
        l.info.direct = on;
    }
}

/// Sessions connected right now.
pub fn list() -> Vec<SessionInfo> {
    live().iter().map(|l| l.info.clone()).collect()
}

pub fn count() -> usize {
    live().len()
}

pub fn set_perms(id: u64, perms: Perms) {
    if let Some(l) = live().iter_mut().find(|l| l.info.id == id) {
        l.info.perms = perms;
        l.perms_tx.send_replace(perms);
    }
}

/// End one session.
pub fn disconnect(id: u64) {
    if let Some(l) = live().iter().find(|l| l.info.id == id) {
        l.kill.notify_one();
    }
}

/// Ask an app viewer to switch sides (it answers on its side).
pub fn request_switch(id: u64) {
    if let Some(l) = live().iter().find(|l| l.info.id == id) {
        l.switch_req.notify_one();
    }
}

// ---- recent sessions (kept across restarts) ----

fn recent_path() -> std::path::PathBuf {
    remote_friend_common::identity::config_dir().join("recent_sessions.json")
}

fn recent_store() -> &'static Mutex<VecDeque<RecentSession>> {
    static R: OnceLock<Mutex<VecDeque<RecentSession>>> = OnceLock::new();
    R.get_or_init(|| {
        let list = std::fs::read(recent_path())
            .ok()
            .and_then(|d| serde_json::from_slice(&d).ok())
            .unwrap_or_default();
        Mutex::new(list)
    })
}

fn push_recent(r: RecentSession) {
    let mut list = recent_store().lock().unwrap_or_else(|e| e.into_inner());
    list.push_front(r);
    list.truncate(MAX_RECENT);
    let data = serde_json::to_vec_pretty(&*list).unwrap_or_default();
    let _ = remote_friend_common::identity::write_private(&recent_path(), &data);
}

/// Finished sessions, newest first.
pub fn recent() -> Vec<RecentSession> {
    recent_store().lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
}

pub fn clear_recent() {
    recent_store().lock().unwrap_or_else(|e| e.into_inner()).clear();
    let _ = std::fs::remove_file(recent_path());
}
