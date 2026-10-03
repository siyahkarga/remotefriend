//! RemoteFriend host engine as a library: used by the desktop app (`remotefriend`) and by
//! the terminal host (`remote-friend-host`).
//!
//! Start it with [`run`] on a tokio runtime; read state with [`snapshot`] and answer
//! prompts with [`pending_prompts`] / [`answer`] (after [`enable_ui_prompts`]).

mod approval;
mod auth;
mod convert;
mod files;
mod input;
mod server;
pub mod status;
mod video;
#[cfg(target_os = "linux")]
mod wayland;
mod web;

pub use approval::Decision;
pub use server::{lan_enabled, reconnect_server, run, RunOptions};
pub use status::{
    answer, enable_ui_prompts, notice, pending_prompts, prompt_sequence, snapshot, Answer, Notice, Prompt, PromptKind,
    ScreenState, Snapshot,
};

pub(crate) use server::host_name;

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

/// Generate and save a new password. None if it comes from REMOTE_FRIEND_PASS.
pub fn renew_password() -> Option<String> {
    auth::renew_password()
}

/// Saved relay settings: (server "host:port", web address for phones, server key).
pub fn server_settings() -> (String, String, String) {
    let cfg = remote_friend_common::identity::load_rv_config();
    (cfg.server, cfg.web_url, cfg.register_key)
}

/// Accept direct connections from the local network (takes effect after a restart).
pub fn set_lan_enabled(enabled: bool) {
    let mut s = remote_friend_common::identity::load_host_settings();
    s.lan = enabled;
    remote_friend_common::identity::save_host_settings(&s);
}

/// Save relay settings and reconnect. An empty server disables internet access.
pub fn set_server(server: &str, web_url: &str, register_key: &str) {
    let mut cfg = remote_friend_common::identity::load_rv_config();
    let mut server = server.trim().to_string();
    if !server.is_empty() && !server.contains(':') {
        server = format!("{server}:{}", remote_friend_common::RENDEZVOUS_PORT);
    }
    if cfg.server != server {
        cfg.fp.clear(); // a different server needs its own trusted fingerprint
    }
    cfg.server = server;
    cfg.web_url = web_url.trim().trim_end_matches('/').to_string();
    cfg.register_key = register_key.trim().to_string();
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
