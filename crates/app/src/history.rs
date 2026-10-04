//! Recent connections, favorites and thumbnails.
//! Stored in ~/.config/remotefriend/recents.json

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RecentEntry {
    pub addr: String,
    pub name: String,
    pub fav: bool,
    pub last_seen: u64,
    /// Small preview PNG in the config folder
    pub thumb: Option<String>,
    /// Trusted-device token from that computer ("Always allow"): connect without a password.
    #[serde(default)]
    pub device: String,
    /// Last password that worked (until that computer changes it). The file is private.
    #[serde(default)]
    pub password: String,
}

/// Same computer? IDs compare by their digits ("726 930 030" == "726930030").
pub fn same_addr(a: &str, b: &str) -> bool {
    let digits = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
    let id = |s: &str| !s.contains(':') && !s.contains('.') && digits(s).len() == 9;
    if id(a) && id(b) {
        digits(a) == digits(b)
    } else {
        a.trim() == b.trim()
    }
}

/// Saved trusted-device token for this computer, if any.
pub fn device_for(recents: &[RecentEntry], addr: &str) -> Option<String> {
    recents.iter().find(|e| same_addr(&e.addr, addr)).map(|e| e.device.clone()).filter(|d| d.len() == 64)
}

/// Saved password for this computer, if any.
pub fn password_for(recents: &[RecentEntry], addr: &str) -> Option<String> {
    recents.iter().find(|e| same_addr(&e.addr, addr)).map(|e| e.password.clone()).filter(|p| !p.is_empty())
}

/// Remember (or forget, with "") the password that worked for this computer.
pub fn set_password(recents: &mut [RecentEntry], addr: &str, password: &str) {
    if let Some(e) = recents.iter_mut().find(|e| same_addr(&e.addr, addr)) {
        if e.password != password {
            e.password = password.to_string();
            save_recents(recents);
        }
    }
}

/// Remember (or forget, with "") the trusted-device token for this computer.
pub fn set_device(recents: &mut [RecentEntry], addr: &str, token: &str) {
    if let Some(e) = recents.iter_mut().find(|e| same_addr(&e.addr, addr)) {
        e.device = token.to_string();
        save_recents(recents);
    }
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or("/tmp".into());
    let d = PathBuf::from(base).join(".config/remotefriend");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn load_recents() -> Vec<RecentEntry> {
    let p = config_dir().join("recents.json");
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Private file (0600): it holds trusted-device tokens.
pub fn save_recents(r: &[RecentEntry]) {
    let p = config_dir().join("recents.json");
    let data = serde_json::to_string_pretty(r).unwrap_or_default();
    let _ = remote_friend_common::identity::write_private(&p, data.as_bytes());
}

/// Unix seconds
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Add/update a connection (max 20; favorites are kept)
pub fn touch_recent(recents: &mut Vec<RecentEntry>, addr: &str, name: &str) {
    if let Some(e) = recents.iter_mut().find(|e| same_addr(&e.addr, addr)) {
        e.last_seen = now_unix();
        if !name.is_empty() {
            e.name = name.to_string();
        }
    } else {
        recents.push(RecentEntry {
            addr: addr.to_string(),
            name: if name.is_empty() { addr.to_string() } else { name.to_string() },
            fav: false,
            last_seen: now_unix(),
            thumb: None,
            device: String::new(),
            password: String::new(),
        });
    }
    // favorites first, then most recent; drop beyond 20
    recents.sort_by(|a, b| b.fav.cmp(&a.fav).then(b.last_seen.cmp(&a.last_seen)));
    recents.truncate(20);
    save_recents(recents);
}

/// Save a 320 px preview and return its file name
pub fn save_thumb(addr: &str, img: &egui::ColorImage) -> Option<String> {
    let [w, h] = img.size;
    if w == 0 || h == 0 {
        return None;
    }
    let mut flat = Vec::with_capacity(w * h * 4);
    for p in &img.pixels {
        flat.extend_from_slice(&p.to_array());
    }
    let rgba = image::RgbaImage::from_raw(w as u32, h as u32, flat)?;
    let tw = 320u32;
    let th = ((h as f32 * (tw as f32 / w as f32)) as u32).max(1);
    let small = image::imageops::resize(&rgba, tw, th, image::imageops::FilterType::Triangle);
    let safe: String = addr.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
    let fname = format!("thumb_{safe}.png");
    small.save(config_dir().join(&fname)).ok()?;
    Some(fname)
}

pub fn thumb_path(fname: &str) -> PathBuf {
    config_dir().join(fname)
}
