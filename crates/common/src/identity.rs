//! Persistent identity and settings files (~/.config/remotefriend/).

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

/// Writes sensitive settings with 0600 permissions on Unix.
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

/// Persistent 9-digit host ID (generated with a CSPRNG and saved if missing).
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

/// Persistent 256-bit secret proving the host's identity to the relay.
/// Never shown to clients; only used for host<->relay registration inside TLS.
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

/// The built-in relay, used when Settings name none: its address, the SHA-256 fingerprint of its
/// certificate (pinned, so nobody is asked whether to trust it) and its web page for phones.
/// They come from the build (RF_DEFAULT_SERVER, RF_DEFAULT_SERVER_FP, RF_DEFAULT_WEB_URL; the
/// release builds set them), so the source names no server. Without them there is no built-in
/// server and the app works on the local network until one is set in Settings.
pub const DEFAULT_SERVER: &str = match option_env!("RF_DEFAULT_SERVER") {
    Some(s) => s,
    None => "",
};
pub const DEFAULT_SERVER_FP: &str = match option_env!("RF_DEFAULT_SERVER_FP") {
    Some(s) => s,
    None => "",
};
pub const DEFAULT_WEB_URL: &str = match option_env!("RF_DEFAULT_WEB_URL") {
    Some(s) => s,
    None => "",
};

/// Relay server settings (address + pinned certificate fingerprint + public web URL).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
pub struct RvConfig {
    pub server: String,
    pub fp: String,
    /// Address phones/browsers open (e.g. https://remote.example.com). Optional.
    #[serde(default)]
    pub web_url: String,
    /// Access key for the server (from its owner): lets this computer register there.
    #[serde(default)]
    pub register_key: String,
    /// Local network only: no server at all, not even the default one.
    #[serde(default)]
    pub no_server: bool,
}

/// Host settings (~/.config/remotefriend/settings.json).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
pub struct HostSettings {
    /// Accept direct connections from the local network (off: only via the server).
    #[serde(default)]
    pub lan: bool,
    /// Old setting (v0.9): keep the same password. Replaced by `password_change`.
    #[serde(default)]
    pub fixed_password: bool,
    /// When the password changes: "start" (each time RemoteFriend starts, default),
    /// "session" (after every session that used it) or "never".
    #[serde(default)]
    pub password_change: String,
    /// Never try direct (peer-to-peer) connections; everything goes through the server.
    #[serde(default)]
    pub direct_off: bool,
    /// Viewers see everything; otherwise RemoteFriend and private apps are hidden (safe view).
    #[serde(default)]
    pub full_view: bool,
    /// Name parts of private apps hidden in the safe view (empty: the built-in list).
    #[serde(default)]
    pub private_apps: Vec<String>,
    /// Don't ask blobidea.com for new versions.
    #[serde(default)]
    pub no_update_check: bool,
}

pub fn load_host_settings() -> HostSettings {
    std::fs::read_to_string(config_dir().join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_host_settings(s: &HostSettings) {
    let data = serde_json::to_vec_pretty(s).unwrap_or_default();
    let _ = write_private(&config_dir().join("settings.json"), &data);
}

/// Relay settings as used for connections: the default server when none is saved.
pub fn load_rv_config() -> RvConfig {
    let mut c = load_rv_config_saved();
    if c.no_server {
        c.server.clear();
    } else if c.server.trim().is_empty() && !DEFAULT_SERVER.is_empty() {
        c.server = DEFAULT_SERVER.into();
        c.fp = DEFAULT_SERVER_FP.into();
        if c.web_url.trim().is_empty() {
            c.web_url = DEFAULT_WEB_URL.into();
        }
    }
    c
}

/// Relay settings exactly as saved (empty server: the default one).
pub fn load_rv_config_saved() -> RvConfig {
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
