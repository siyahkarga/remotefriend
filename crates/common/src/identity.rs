//! Kalıcı kimlik + ayar dosyaları (~/.config/remotefriend/).

use std::path::PathBuf;

pub fn config_dir() -> PathBuf {
    let base = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or("/tmp".into());
    let d = PathBuf::from(base).join(".config/remotefriend");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Kalıcı 9 haneli host ID'si (yoksa üretir + saklar).
pub fn load_or_create_host_id() -> String {
    let p = config_dir().join("host_id");
    if let Ok(id) = std::fs::read_to_string(&p) {
        let id = id.trim().to_string();
        if id.len() == 9 && id.chars().all(|c| c.is_ascii_digit()) {
            return id;
        }
    }
    let id = super::new_host_id();
    let _ = std::fs::write(&p, &id);
    id
}

/// Rendezvous sunucu ayarı (adres + sertifika fingerprint).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
pub struct RvConfig {
    pub server: String,
    pub fp: String,
}

pub fn load_rv_config() -> RvConfig {
    let p = config_dir().join("rendezvous.json");
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_rv_config(c: &RvConfig) {
    let p = config_dir().join("rendezvous.json");
    let _ = std::fs::write(p, serde_json::to_string_pretty(c).unwrap_or_default());
}
