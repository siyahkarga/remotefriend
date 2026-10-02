//! Kalıcı kimlik + ayar dosyaları (~/.config/remotefriend/).

use std::io::Write;
use std::path::{Path, PathBuf};

pub fn config_dir() -> PathBuf {
    let base = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let d = PathBuf::from(base).join(".config/remotefriend");
    let _ = std::fs::create_dir_all(&d);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    }
    d
}

/// Hassas ayarları Unix'te 0600 izinle yazar.
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(data)?;
    f.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Kalıcı 9 haneli host ID'si (yoksa CSPRNG ile üretir + saklar).
pub fn load_or_create_host_id() -> String {
    let p = config_dir().join("host_id");
    if let Ok(id) = std::fs::read_to_string(&p) {
        let id = id.trim().to_string();
        if id.len() == 9 && id.chars().all(|c| c.is_ascii_digit()) {
            return id;
        }
    }
    let id = super::new_host_id();
    let _ = write_private(&p, id.as_bytes());
    id
}

/// Rendezvous'a host kimliğini kanıtlayan 256-bit kalıcı sır.
/// Bu değer karşı tarafa verilmez; sadece TLS içindeki host<->VPS kayıtlarında kullanılır.
pub fn load_or_create_host_secret() -> String {
    let p = config_dir().join("host_secret");
    if let Ok(secret) = std::fs::read_to_string(&p) {
        let secret = secret.trim().to_ascii_lowercase();
        if secret.len() == 64 && secret.chars().all(|c| c.is_ascii_hexdigit()) {
            return secret;
        }
    }
    let secret = super::new_secret_hex();
    let _ = write_private(&p, secret.as_bytes());
    secret
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
    let data = serde_json::to_vec_pretty(c).unwrap_or_default();
    let _ = write_private(&p, &data);
}
