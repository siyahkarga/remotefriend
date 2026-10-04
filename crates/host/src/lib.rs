//! RemoteFriend host engine as a library: used by the desktop app (`remotefriend`) and by
//! the terminal host (`remote-friend-host`).
//!
//! Start it with [`run`] on a tokio runtime; read state with [`snapshot`] and answer
//! prompts with [`pending_prompts`] / [`answer`] (after [`enable_ui_prompts`]).

mod approval;
pub mod audio;
mod auth;
mod clipboard;
mod convert;
mod core;
mod files;
mod input;
pub mod privacy;
pub mod p2p;
mod server;
pub mod sessions;
pub mod status;
mod video;
#[cfg(target_os = "linux")]
mod wayland;
mod web;

pub use approval::{grant_temporary, remove_device, trusted_devices, Decision, TrustedDevice};
pub use server::{lan_enabled, reconnect_server, run, RunOptions};
pub use status::{
    answer, duration_text, enable_ui_prompts, local_time, notice, pending_prompts, prompt_sequence, snapshot, take_switch,
    Answer, Notice, Prompt,
    PromptKind, ScreenState, Snapshot, SwitchTarget,
};

pub(crate) use server::host_name;
pub use video::{current_monitor, monitors, preview, select_monitor, MonitorInfo};
pub use files::{cancel as cancel_incoming_file, save_download_chunk};

/// Logging to stderr: "info" by default; harmless D-Bus/portal warnings are hidden.
/// Override with RUST_LOG (e.g. RUST_LOG=debug,zbus=warn).
pub fn init_logging() {
    use tracing_subscriber::prelude::*;
    let default = "info,zbus=error,ashpd=error,pipewire=warn";
    let filter: tracing_subscriber::filter::Targets = std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| default.parse().expect("valid default log filter"));
    let _ = tracing_subscriber::registry().with(tracing_subscriber::fmt::layer()).with(filter).try_init();
}

/// Forget all permanently allowed devices. Returns how many were removed.
pub fn forget_devices() -> usize {
    approval::forget_devices()
}

pub use auth::{policy as password_policy, set_policy as set_password_policy, PasswordPolicy};

/// Try direct (peer-to-peer) connections when possible (on by default).
pub fn set_direct_enabled(on: bool) {
    let mut s = remote_friend_common::identity::load_host_settings();
    s.direct_off = !on;
    remote_friend_common::identity::save_host_settings(&s);
}

/// Generate and save a new password. None if it comes from REMOTE_FRIEND_PASS.
pub fn renew_password() -> Option<String> {
    auth::renew_password()
}

/// Relay settings as saved: server "host:port" (empty: the default server), web address for
/// phones, access key, and whether a server is used at all.
pub struct ServerSettings {
    pub server: String,
    pub web_url: String,
    pub key: String,
    pub use_server: bool,
}

pub fn server_settings() -> ServerSettings {
    let cfg = remote_friend_common::identity::load_rv_config_saved();
    ServerSettings { server: cfg.server, web_url: cfg.web_url, key: cfg.register_key, use_server: !cfg.no_server }
}

/// Accept direct connections from the local network (takes effect after a restart).
pub fn set_lan_enabled(enabled: bool) {
    let mut s = remote_friend_common::identity::load_host_settings();
    s.lan = enabled;
    remote_friend_common::identity::save_host_settings(&s);
}

/// Access keys on the server: list, add, revoke, ... (see the relay's keys.rs). Needs the
/// server's owner key in Settings; blocks, so call it off the UI thread.
pub fn manage_access_keys(request: serde_json::Value) -> Result<serde_json::Value, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(server::admin_request(&request)).map_err(|e| format!("{e:#}"))
}

/// Save relay settings and reconnect. An empty server means the default one; `use_server`
/// off keeps the computer on the local network.
pub fn set_server(server: &str, web_url: &str, register_key: &str, use_server: bool) {
    use remote_friend_common::identity::{DEFAULT_SERVER, DEFAULT_WEB_URL};
    let mut cfg = remote_friend_common::identity::load_rv_config_saved();
    let mut server = server.trim().to_string();
    if !server.is_empty() && !server.contains(':') {
        server = format!("{server}:{}", remote_friend_common::RENDEZVOUS_PORT);
    }
    if server == DEFAULT_SERVER {
        server.clear(); // stays on the default, with its pinned certificate
    }
    if cfg.server != server {
        cfg.fp.clear(); // a different server needs its own trusted fingerprint
    }
    let mut web_url = web_url.trim().trim_end_matches('/').to_string();
    if server.is_empty() && web_url == DEFAULT_WEB_URL {
        web_url.clear();
    }
    cfg.server = server;
    cfg.web_url = web_url;
    cfg.register_key = register_key.trim().to_string();
    cfg.no_server = !use_server;
    remote_friend_common::identity::save_rv_config(&cfg);
    reconnect_server();
}

/// Ask the desktop again for screen-sharing permission (Wayland).
/// Returns false when the app must be restarted to ask again; true otherwise.
pub fn retry_screen_permission() -> bool {
    #[cfg(target_os = "linux")]
    return wayland::retry();
    #[cfg(not(target_os = "linux"))]
    true
}

/// Folder where received files are saved.
pub fn receive_dir() -> std::path::PathBuf {
    files::receive_dir()
}
