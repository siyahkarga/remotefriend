//! The part of a remote session shared by browsers and the desktop app: the login
//! decision, control messages, permissions, sound, files, clipboard and switching sides.

use anyhow::Result;
use remote_friend_common::{FileChunk, InputEvent, MouseButton};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};

use crate::approval::Decision;
use crate::audio::AudioPacket;
use crate::sessions::{SessionHandle, Transfer};
use crate::web::Flow;

/// Size limit for one control message from a viewer (including base64 file chunks).
pub(crate) const MAX_TEXT: usize = 400 * 1024;

/// How the viewer proved itself in the encrypted handshake.
pub(crate) enum Login {
    Password,
    /// A trusted device (or a one-time grant for switching sides).
    Device { id: String, label: String, temporary: bool },
}

impl Login {
    pub(crate) fn used_password(&self) -> bool {
        matches!(self, Login::Password)
    }
}

/// The viewer's first encrypted message: {t:"auth", device?, resume?, name?, switch?}.
#[derive(Default)]
pub(crate) struct AuthMsg {
    /// Device token from before v0.9, sent along with the password.
    pub device: String,
    pub resume: String,
    /// Device name for the computer's session list ("Chrome on Android", a computer name).
    pub name: String,
    /// A RemoteFriend app that can switch sides.
    pub can_switch: bool,
}

impl AuthMsg {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        if v.get("t")?.as_str()? != "auth" {
            return None;
        }
        let field = |k: &str, n: usize| -> String {
            v.get(k).and_then(|x| x.as_str()).unwrap_or("").chars().filter(|c| !c.is_control()).take(n).collect()
        };
        Some(Self {
            device: field("device", 64),
            resume: field("resume", 64),
            name: field("name", 60),
            can_switch: v.get("switch").and_then(|x| x.as_bool()).unwrap_or(false),
        })
    }
}

/// Name for this viewer in lists and messages.
pub(crate) fn viewer_label(login: &Login, auth: &AuthMsg, peer: &str) -> String {
    if !auth.name.trim().is_empty() {
        return auth.name.trim().to_string();
    }
    match login {
        Login::Device { label, .. } if !label.is_empty() => label.clone(),
        _ => peer.to_string(),
    }
}

/// Approval without asking anyone, when possible: trusted device, resumed session, or a
/// device token from before v0.9 presented together with the password.
pub(crate) fn quick_decision(login: &Login, auth: &AuthMsg, label: &str) -> Option<Decision> {
    match login {
        Login::Device { id, temporary, .. } => {
            if !*temporary {
                crate::approval::device_used(id);
            }
            crate::status::notice(if *temporary {
                format!("{label}: connected (switched sides)")
            } else {
                format!("{label}: trusted device connected")
            });
            Some(Decision::Once)
        }
        Login::Password if crate::approval::consume_resume(&auth.resume) => {
            crate::status::notice(format!("{label}: session resumed"));
            Some(Decision::Once)
        }
        Login::Password if crate::approval::is_trusted(&auth.device) => {
            crate::status::notice(format!("{label}: trusted device connected"));
            Some(Decision::Once)
        }
        Login::Password => None,
    }
}

/// Which operating system this computer runs (viewers pick Ctrl or Cmd shortcuts by it).
pub(crate) fn host_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// The session's sound: on when the viewer wants it and the computer's user allows it.
#[derive(Default)]
pub(crate) struct Sound {
    rx: Option<broadcast::Receiver<Arc<AudioPacket>>>,
    wanted: bool,
    /// 24 kHz PCM instead of Opus (browsers without an Opus decoder).
    pub pcm: bool,
}

impl Sound {
    fn apply(&mut self, allowed: bool) {
        if !(self.wanted && allowed) {
            self.rx = None;
        } else if self.rx.is_none() {
            self.rx = Some(crate::audio::subscribe());
        }
    }
}

async fn next_sound(rx: &mut Option<broadcast::Receiver<Arc<AudioPacket>>>) -> Option<Arc<AudioPacket>> {
    match rx {
        Some(r) => match r.recv().await {
            Ok(p) => Some(p),
            Err(broadcast::error::RecvError::Lagged(_)) => None,
            Err(broadcast::error::RecvError::Closed) => {
                *rx = None;
                None
            }
        },
        None => std::future::pending().await,
    }
}

async fn next_clip(rx: &mut Option<broadcast::Receiver<String>>) -> Option<String> {
    match rx {
        Some(r) => r.recv().await.ok(),
        None => std::future::pending().await,
    }
}

/// What a session gives the core so that it can move to a direct (peer-to-peer) path.
pub(crate) struct DirectHooks {
    pub keys: remote_friend_common::e2e::DirectPair,
    /// Tells the session's writer to send through the direct path from now on.
    pub set_out: mpsc::Sender<crate::p2p::DirectOut>,
    /// Decrypts what arrives on the direct path and feeds the session (browser JSON or app
    /// packets); the task ends when the path closes.
    pub start_reader: DirectHooksReader,
}

pub(crate) type DirectHooksReader =
    Box<dyn FnOnce(mpsc::Receiver<Vec<u8>>, remote_friend_common::e2e::Cipher) -> tokio::task::JoinHandle<()> + Send>;

/// Something the session loop must act on.
pub(crate) enum Event {
    /// Sound for the viewer.
    Audio(Arc<AudioPacket>),
    /// A control message for the viewer (JSON).
    Send(String),
    /// The computer's user ended this session.
    Kill,
    Nothing,
}

struct Incoming {
    name: String,
    last_report: Instant,
}

pub(crate) struct Core {
    pub handle: SessionHandle,
    pub flow: Flow,
    pub sound: Sound,
    monitors: tokio::sync::watch::Receiver<u64>,
    ctrl: mpsc::Sender<String>,
    clip: Option<broadcast::Receiver<String>>,
    incoming: std::collections::HashMap<u64, Incoming>,
    label: String,
    direct: Option<DirectHooks>,
    /// Lowest-priority lane for files this computer sends (downloads).
    file_lane: Option<mpsc::Sender<FileChunk>>,
    downloads: std::collections::HashMap<u64, Arc<std::sync::atomic::AtomicBool>>,
}

impl Core {
    pub(crate) fn new(handle: SessionHandle, flow: Flow, ctrl: mpsc::Sender<String>, direct: Option<DirectHooks>) -> Self {
        let label = handle.label();
        let clip = handle.perms().clipboard.then(crate::clipboard::subscribe);
        let monitors = crate::video::monitor_changes();
        Self {
            handle,
            flow,
            sound: Sound::default(),
            monitors,
            ctrl,
            clip,
            incoming: Default::default(),
            label,
            direct,
            file_lane: None,
            downloads: Default::default(),
        }
    }

    /// Where file chunks for downloads go (the session sends them when there is room).
    pub(crate) fn set_file_lane(&mut self, lane: mpsc::Sender<FileChunk>) {
        self.file_lane = Some(lane);
    }

    fn send(&self, v: serde_json::Value) {
        let _ = self.ctrl.try_send(v.to_string());
    }

    /// Waits for the next thing to forward to the viewer.
    pub(crate) async fn next_event(&mut self) -> Event {
        let kill = self.handle.kill_signal();
        let switch = self.handle.switch_signal();
        tokio::select! {
            p = next_sound(&mut self.sound.rx) => p.map_or(Event::Nothing, Event::Audio),
            msg = self.handle.changed() => {
                let perms = self.handle.perms();
                if !self.handle.control_allowed() {
                    crate::input::release_all();
                }
                self.sound.apply(perms.sound);
                if perms.clipboard != self.clip.is_some() {
                    self.clip = perms.clipboard.then(crate::clipboard::subscribe);
                }
                Event::Send(msg.to_string())
            }
            text = next_clip(&mut self.clip) => match text {
                Some(text) => Event::Send(serde_json::json!({"t": "clip", "text": text}).to_string()),
                None => Event::Nothing,
            },
            r = self.monitors.changed() => match r {
                Ok(()) => Event::Send(crate::video::monitors_json().to_string()),
                Err(_) => std::future::pending().await,
            },
            _ = kill.notified() => Event::Kill,
            _ = switch.notified() => Event::Send(r#"{"t":"switch_req"}"#.into()),
        }
    }

    /// Mouse/keyboard from the viewer (frame coordinates), if allowed right now.
    pub(crate) fn input(&mut self, ev: InputEvent) {
        if !self.handle.control_allowed() {
            return;
        }
        let ev = match ev {
            InputEvent::MouseMove { x, y } => match self.flow.map_point(x, y) {
                Some((x, y)) => InputEvent::MouseMove { x, y },
                None => return,
            },
            other => other,
        };
        if let Err(e) = crate::input::apply(ev) {
            tracing::debug!("input error: {e:#}");
        }
    }

    /// The viewer turns its sound on or off.
    pub(crate) fn want_sound(&mut self, on: bool, pcm: bool) {
        self.sound.wanted = on;
        self.sound.pcm = pcm;
        self.sound.apply(self.handle.perms().sound);
    }

    /// A piece of a file from the viewer.
    pub(crate) fn file(&mut self, chunk: FileChunk) {
        let id = chunk.transfer_id;
        if !self.handle.perms().files {
            self.incoming.remove(&id);
            self.send(serde_json::json!({"t": "file_err", "id": id, "msg": "file transfer is turned off on the remote computer"}));
            return;
        }
        let name: String = chunk.name.chars().take(128).collect();
        let (got, total) = (chunk.offset + chunk.data.len() as u64, chunk.total);
        if chunk.offset == 0 && !self.incoming.contains_key(&id) {
            crate::status::notice(format!("Receiving {name} ({}) from {}…", human_size(total), self.label));
            self.incoming.insert(id, Incoming { name: name.clone(), last_report: Instant::now() });
        }
        self.handle.set_transfer(Some(Transfer { name: name.clone(), got, total }));
        match crate::files::save_chunk(chunk) {
            Ok(Some(path)) => {
                self.incoming.remove(&id);
                self.handle.set_transfer(None);
                self.send(serde_json::json!({"t": "file_done", "id": id, "name": name, "path": path}));
                crate::status::notice(format!("Received {name} from {}: {path}", self.label));
                crate::status::desktop_notify("RemoteFriend: file received", &format!("{name} saved to {path}"));
            }
            Ok(None) => {
                if let Some(inc) = self.incoming.get_mut(&id) {
                    if inc.last_report.elapsed() > Duration::from_millis(300) {
                        inc.last_report = Instant::now();
                        self.send(serde_json::json!({"t": "file_progress", "id": id, "got": got, "total": total}));
                    }
                }
            }
            Err(e) => {
                let name = self.incoming.remove(&id).map(|i| i.name).unwrap_or(name);
                self.handle.set_transfer(None);
                crate::status::notice(format!("Receiving {name} failed: {e:#}"));
                self.send(serde_json::json!({"t": "file_err", "id": id, "msg": format!("{e:#}")}));
            }
        }
    }

    /// A control message from the viewer (browsers also send their input this way).
    pub(crate) fn on_json(&mut self, t: &str) -> Result<()> {
        let v: serde_json::Value = serde_json::from_str(t)?;
        match v.get("t").and_then(|x| x.as_str()) {
            Some("mouse") => self.input(InputEvent::MouseMove { x: num_u32(&v, "x"), y: num_u32(&v, "y") }),
            Some("down") => self.input(InputEvent::MouseDown { button: btn(&v) }),
            Some("up") => self.input(InputEvent::MouseUp { button: btn(&v) }),
            Some("scroll") => {
                let (dx, dy) = (num_i32(&v, "dx"), num_i32(&v, "dy"));
                if dx != 0 || dy != 0 {
                    self.input(InputEvent::Scroll { dx, dy });
                }
            }
            Some("key") => {
                let code = v.get("code").and_then(|x| x.as_str()).unwrap_or("");
                let down = v.get("down").and_then(|x| x.as_bool()).unwrap_or(true);
                if let Some(key) = crate::input::key_from_web(code) {
                    self.input(InputEvent::Key { key, down });
                }
            }
            Some("text") => {
                let s: String = v.get("s").and_then(|x| x.as_str()).unwrap_or("").chars().take(4096).collect();
                if !s.is_empty() {
                    self.input(InputEvent::Text(s));
                }
            }
            Some("ack") => {
                if let Some(s) = v.get("s").and_then(|n| n.as_u64()) {
                    // The header carries only the low 32 bits of seq; rebuild the full value from the last sent frame.
                    let full = self.flow.last.as_ref().map_or(s, |f| (f.seq & !0xffff_ffff) | s);
                    self.flow.on_ack(full);
                }
            }
            Some("kf") => self.flow.want_key(),
            Some("quality") => {
                if let Some(p) = v.get("p").and_then(|x| x.as_str()).and_then(crate::video::Preset::from_name) {
                    crate::video::set_preset(p);
                    self.send(serde_json::json!({"t": "quality", "p": p.name()}));
                }
            }
            Some("ping") => {
                let ts = v.get("ts").cloned().unwrap_or(serde_json::Value::Null);
                self.send(serde_json::json!({"t": "pong", "ts": ts}));
            }
            Some("audio") => {
                let on = v.get("on").and_then(|x| x.as_bool()).unwrap_or(false);
                let pcm = v.get("codec").and_then(|x| x.as_str()) == Some("pcm");
                self.want_sound(on, pcm);
            }
            Some("clip") => {
                if self.handle.perms().clipboard {
                    if let Some(text) = v.get("text").and_then(|x| x.as_str()) {
                        crate::clipboard::set(text.to_string());
                    }
                }
            }
            Some("bye") => self.handle.bye = true,
            Some("monitor") => {
                // Changing the shared screen is a control action.
                if self.handle.control_allowed() {
                    if let Some(i) = v.get("i").and_then(|x| x.as_u64()) {
                        crate::video::select_monitor(i as usize);
                    }
                }
            }
            Some("monitors") => self.send(crate::video::monitors_json()),
            Some("switch") => self.on_switch(&v),
            Some("file_cancel") => {
                let id = v.get("id").and_then(|n| n.as_u64()).unwrap_or(0);
                self.incoming.remove(&id);
                self.handle.set_transfer(None);
                if let Some(name) = crate::files::cancel(id) {
                    crate::status::notice(format!("{} canceled sending {name}", self.label));
                }
            }
            Some("ls") => self.list(v.get("path").and_then(|x| x.as_str()).unwrap_or("").to_string()),
            Some("get") => {
                let path = v.get("path").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let id = v.get("id").and_then(|n| n.as_u64()).unwrap_or(0);
                self.download(path, id);
            }
            Some("get_cancel") => {
                let id = v.get("id").and_then(|n| n.as_u64()).unwrap_or(0);
                if let Some(flag) = self.downloads.remove(&id) {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            Some("p2p_offer") => {
                let sdp = v.get("sdp").and_then(|x| x.as_str()).unwrap_or("").to_string();
                self.start_direct(sdp);
            }
            Some("file") => {
                use base64::Engine;
                let data_b64 = v.get("data").and_then(|x| x.as_str()).unwrap_or("");
                if data_b64.len() > MAX_TEXT {
                    anyhow::bail!("file chunk too large");
                }
                self.file(FileChunk {
                    transfer_id: v.get("transfer_id").and_then(|n| n.as_u64()).unwrap_or(0),
                    name: v.get("name").and_then(|x| x.as_str()).unwrap_or("file").chars().take(128).collect(),
                    offset: v.get("offset").and_then(|n| n.as_u64()).unwrap_or(0),
                    total: v.get("total").and_then(|n| n.as_u64()).unwrap_or(0),
                    data: base64::engine::general_purpose::STANDARD.decode(data_b64)?,
                    last: v.get("last").and_then(|x| x.as_bool()).unwrap_or(false),
                });
            }
            _ => {}
        }
        Ok(())
    }

    /// Folder listing for the viewer's file browser.
    fn list(&mut self, path: String) {
        if !self.handle.perms().files {
            self.send(serde_json::json!({"t": "ls", "error": "file access is turned off on the remote computer"}));
            return;
        }
        let ctrl = self.ctrl.clone();
        tokio::spawn(async move {
            let msg = match tokio::task::spawn_blocking(move || crate::files::list_dir(&path)).await {
                Ok(Ok((path, parent, entries))) => serde_json::json!({"t": "ls", "path": path, "parent": parent, "entries": entries}),
                Ok(Err(e)) => serde_json::json!({"t": "ls", "error": format!("{e:#}")}),
                Err(_) => return,
            };
            let _ = ctrl.send(msg.to_string()).await;
        });
    }

    /// Send a file of this computer to the viewer, in 64 KiB chunks on the file lane.
    fn download(&mut self, path: String, id: u64) {
        let fail = |core: &Self, msg: &str| core.send(serde_json::json!({"t": "get_err", "id": id, "msg": msg}));
        if id == 0 {
            return;
        }
        if !self.handle.perms().files {
            return fail(self, "file access is turned off on the remote computer");
        }
        let Some(lane) = self.file_lane.clone() else { return fail(self, "downloads are not supported here") };
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.downloads.insert(id, cancel.clone());
        let (ctrl, label) = (self.ctrl.clone(), self.label.clone());
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let err = |msg: String| serde_json::json!({"t": "get_err", "id": id, "msg": msg}).to_string();
            let (file, name, total) = match tokio::task::spawn_blocking(move || crate::files::open_for_download(&path)).await {
                Ok(Ok(x)) => x,
                Ok(Err(e)) => {
                    let _ = ctrl.send(err(format!("{e:#}"))).await;
                    return;
                }
                Err(_) => return,
            };
            crate::status::notice(format!("{label} is downloading {name} ({}) from this computer", human_size(total)));
            let mut file = tokio::fs::File::from_std(file);
            let mut offset = 0u64;
            loop {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    crate::status::notice(format!("{label} canceled downloading {name}"));
                    return;
                }
                let mut buf = vec![0u8; 64 * 1024];
                let n = match file.read(&mut buf).await {
                    Ok(n) => n,
                    Err(e) => {
                        let _ = ctrl.send(err(format!("cannot read {name}: {e}"))).await;
                        return;
                    }
                };
                buf.truncate(n);
                let last = n == 0 || offset + n as u64 >= total;
                if n == 0 && offset < total {
                    let _ = ctrl.send(err(format!("{name} got shorter while reading"))).await;
                    return;
                }
                let chunk = FileChunk { transfer_id: id, name: name.clone(), offset, total, data: buf, last };
                if lane.send(chunk).await.is_err() {
                    return;
                }
                offset += n as u64;
                if last {
                    break;
                }
            }
            crate::status::notice(format!("{label} downloaded {name}"));
        });
    }

    /// The viewer offers a direct path ({t:"p2p_offer", sdp}): answer it and, once the data
    /// channel opens, move the session's traffic there. One attempt per session.
    fn start_direct(&mut self, sdp: String) {
        if !crate::p2p::enabled() || sdp.is_empty() || sdp.len() > 20_000 {
            self.send(serde_json::json!({"t": "p2p_off"}));
            return;
        }
        let Some(hooks) = self.direct.take() else { return };
        let (ctrl, id, label) = (self.ctrl.clone(), self.handle.id, self.label.clone());
        tokio::spawn(async move {
            let stun = crate::p2p::stun_server().await;
            let (answer, open) = match crate::p2p::answer(&sdp, stun).await {
                Ok(x) => x,
                Err(e) => {
                    tracing::info!("{label}: no direct connection: {e:#}");
                    let _ = ctrl.send(r#"{"t":"p2p_off"}"#.into()).await;
                    return;
                }
            };
            let _ = ctrl.send(serde_json::json!({"t": "p2p_answer", "sdp": answer}).to_string()).await;
            let Ok(link) = open.await else {
                tracing::info!("{label}: no direct path (staying on the relay)");
                return;
            };
            let crate::p2p::Link { out, inc } = link;
            let remote_friend_common::e2e::DirectPair { tx, rx } = hooks.keys;
            let reader = (hooks.start_reader)(inc, rx);
            if hooks.set_out.send(crate::p2p::DirectOut { cipher: tx, out }).await.is_err() {
                return;
            }
            crate::sessions::set_direct(id, true);
            // A clean keyframe on the new path (frames still on their way through the relay are dropped).
            crate::video::request_keyframe();
            crate::status::notice(format!("{label}: direct connection (peer-to-peer)"));
            let _ = reader.await;
            crate::sessions::set_direct(id, false);
        });
    }

    /// The viewer (a RemoteFriend app) offers its own computer: {t:"switch", id, token, reply}.
    /// `reply` = it answers our request, otherwise this computer's user is asked first.
    fn on_switch(&mut self, v: &serde_json::Value) {
        let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let (id, token) = (field("id"), field("token"));
        if id.len() != 9 || !id.bytes().all(|b| b.is_ascii_digit()) || token.len() != 64 {
            return;
        }
        let reply = v.get("reply").and_then(|x| x.as_bool()).unwrap_or(false);
        let label = self.label.clone();
        let ctrl = self.ctrl.clone();
        tokio::spawn(async move {
            let yes = reply || {
                let l = label.clone();
                tokio::task::spawn_blocking(move || {
                    matches!(
                        crate::status::ask_ui(crate::status::PromptKind::SwitchSides { label: l }, Duration::from_secs(60)),
                        Some(crate::status::Answer::Switch(true))
                    )
                })
                .await
                .unwrap_or(false)
            };
            if yes {
                crate::status::start_switch(crate::status::SwitchTarget { id, token, label });
                let _ = ctrl.send(r#"{"t":"switch_ok"}"#.into()).await;
            } else {
                let _ = ctrl.send(r#"{"t":"switch_no"}"#.into()).await;
            }
        });
    }
}

pub(crate) fn human_size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} B"),
    }
}

fn btn(v: &serde_json::Value) -> MouseButton {
    match v.get("b").and_then(|x| x.as_str()).unwrap_or("left") {
        "right" => MouseButton::Right,
        "middle" => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn num_u32(v: &serde_json::Value, k: &str) -> u32 {
    v.get(k).and_then(|n| n.as_f64()).unwrap_or(0.0).clamp(0.0, 1e6) as u32
}

fn num_i32(v: &serde_json::Value, k: &str) -> i32 {
    v.get(k).and_then(|n| n.as_f64()).unwrap_or(0.0).clamp(-1000.0, 1000.0) as i32
}
