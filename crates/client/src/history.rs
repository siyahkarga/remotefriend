//! Son bağlanılanlar + favoriler + thumbnail saklama.
//! Konum: ~/.config/remotefriend/recents.json

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RecentEntry {
    pub addr: String,
    pub name: String,
    pub fav: bool,
    pub last_seen: u64,
    /// config dizinindeki küçük önizleme png'si
    pub thumb: Option<String>,
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

pub fn save_recents(r: &[RecentEntry]) {
    let p = config_dir().join("recents.json");
    let _ = std::fs::write(p, serde_json::to_string_pretty(r).unwrap_or_default());
}

/// unix saniye
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Bağlantıyı kaydet/güncelle (max 20, favoriler korunur)
pub fn touch_recent(recents: &mut Vec<RecentEntry>, addr: &str, name: &str) {
    if let Some(e) = recents.iter_mut().find(|e| e.addr == addr) {
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
        });
    }
    // favoriler önce, sonra yeniler; 20'yi aşanı at (favs korunur)
    recents.sort_by(|a, b| b.fav.cmp(&a.fav).then(b.last_seen.cmp(&a.last_seen)));
    recents.truncate(20);
    save_recents(recents);
}

/// 320px önizleme kaydet, dosya adını döndür
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
