//! Notices from the server for its owner (new free keys, warnings), fetched in the background
//! while the owner key is set: at start and every 10 minutes. The relay keeps them, so nothing
//! is missed while this app is closed.

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default, Clone)]
pub(crate) struct Notices {
    pub unseen: usize,
    /// Newest first: {id, time, kind ("key" | "warning"), text, seen}.
    pub events: Vec<Value>,
    /// The server mails notices to its owner.
    pub mail: bool,
    /// The server offers free keys by email.
    pub signup: bool,
    pub loaded: bool,
    /// The last action on a notice failed.
    pub error: Option<String>,
    /// An action is on its way to the server.
    pub busy: bool,
}

fn owner() -> bool {
    crate::ui_home::owner_key_like(&remote_friend_host::server_settings().key)
}

fn fetch(slot: &Arc<Mutex<Notices>>) {
    if !owner() {
        *slot.lock().unwrap() = Notices::default();
        return;
    }
    match remote_friend_host::manage_access_keys(json!({ "cmd": "events" })) {
        Ok(answer) if answer["ok"] == true => {
            let mut n = slot.lock().unwrap();
            let error = n.error.take();
            *n = Notices {
                unseen: answer["unseen"].as_u64().unwrap_or(0) as usize,
                events: answer["events"].as_array().cloned().unwrap_or_default(),
                mail: answer["mail"].as_bool().unwrap_or(false),
                signup: answer["signup"].as_bool().unwrap_or(false),
                loaded: true,
                error,
                busy: false,
            };
        }
        Ok(answer) => tracing::debug!("notices: {}", answer["error"]),
        Err(e) => tracing::debug!("notices: {e}"),
    }
}

pub(crate) fn start(slot: Arc<Mutex<Notices>>, ctx: egui::Context) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(5));
        loop {
            fetch(&slot);
            ctx.request_repaint();
            std::thread::sleep(Duration::from_secs(600));
        }
    });
}

/// Fetch again now (after opening the Access keys page).
pub(crate) fn refresh(slot: Arc<Mutex<Notices>>, ctx: egui::Context) {
    std::thread::spawn(move || {
        fetch(&slot);
        ctx.request_repaint();
    });
}

/// Mark all notices read on the server.
pub(crate) fn mark_seen(slot: Arc<Mutex<Notices>>) {
    {
        let mut n = slot.lock().unwrap();
        n.unseen = 0;
        for e in &mut n.events {
            e["seen"] = true.into();
        }
    }
    std::thread::spawn(move || {
        let _ = remote_friend_host::manage_access_keys(json!({ "cmd": "events_seen" }));
    });
}

/// Act on a notice ({"cmd": "revoke" | "block" | "more", "event": id, ...}), then reload.
pub(crate) fn act(slot: Arc<Mutex<Notices>>, request: Value, ctx: egui::Context) {
    {
        let mut n = slot.lock().unwrap();
        n.busy = true;
        n.error = None;
    }
    std::thread::spawn(move || {
        let error = match remote_friend_host::manage_access_keys(request) {
            Ok(answer) if answer["ok"] == true => None,
            Ok(answer) => Some(answer["error"].as_str().unwrap_or("the server refused").to_string()),
            Err(e) => Some(e),
        };
        slot.lock().unwrap().error = error;
        fetch(&slot);
        slot.lock().unwrap().busy = false;
        ctx.request_repaint();
    });
}
