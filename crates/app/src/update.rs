//! Update check: twice a day the app asks blobidea.com which version is current (the manifest
//! the download page uses). Only the request itself is sent; Settings → General turns it off.

use anyhow::{bail, Context, Result};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HOST: &str = "blobidea.com";
const MANIFEST: &str = "/downloads/remotefriend/manifest.json";
pub const DOWNLOAD_PAGE: &str = "https://blobidea.com/products/remotefriend";

#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub version: String,
    /// The installed version has a known security problem that this one fixes.
    pub security: bool,
}

pub fn enabled() -> bool {
    !remote_friend_common::identity::load_host_settings().no_update_check
}

pub fn set_enabled(on: bool) {
    let mut s = remote_friend_common::identity::load_host_settings();
    s.no_update_check = !on;
    remote_friend_common::identity::save_host_settings(&s);
}

/// Checks in the background and fills `slot` when a newer version exists.
pub fn start(slot: Arc<Mutex<Option<Update>>>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
            return;
        };
        rt.block_on(async move {
            tokio::time::sleep(Duration::from_secs(20)).await;
            loop {
                if enabled() {
                    match fetch().await {
                        Ok(manifest) => {
                            let update = evaluate(&manifest, env!("CARGO_PKG_VERSION"));
                            if let Some(u) = &update {
                                tracing::info!("update available: {} (security: {})", u.version, u.security);
                            }
                            *slot.lock().unwrap() = update;
                            ctx.request_repaint();
                        }
                        Err(e) => tracing::debug!("update check failed: {e:#}"),
                    }
                }
                tokio::time::sleep(Duration::from_secs(12 * 3600)).await;
            }
        });
    });
}

async fn fetch() -> Result<serde_json::Value> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut tls = remote_friend_common::tls::tls_connect(&format!("{HOST}:443"), HOST, None).await?;
    // HTTP/1.0: the answer comes in one piece, no chunked encoding to undo.
    let request = format!(
        "GET {MANIFEST} HTTP/1.0\r\nHost: {HOST}\r\nUser-Agent: RemoteFriend/{}\r\nAccept: application/json\r\n\r\n",
        env!("CARGO_PKG_VERSION")
    );
    tls.write_all(request.as_bytes()).await?;
    let mut body = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), (&mut tls).take(64 * 1024).read_to_end(&mut body))
        .await
        .context("update check timed out")??;
    let text = String::from_utf8_lossy(&body);
    let (head, json) = text.split_once("\r\n\r\n").context("malformed HTTP answer")?;
    let status = head.split_whitespace().nth(1).unwrap_or("");
    if status != "200" {
        bail!("HTTP {status}");
    }
    Ok(serde_json::from_str(json)?)
}

fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut parts = v.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??, parts.next().flatten().unwrap_or(0)))
}

fn newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(a), Some(b)) if a > b)
}

/// `security_since`: the first version that fixes the newest known security problem.
fn evaluate(manifest: &serde_json::Value, current: &str) -> Option<Update> {
    let version = manifest.get("version")?.as_str()?;
    if !newer(version, current) {
        return None;
    }
    let security = manifest
        .get("security_since")
        .and_then(|v| v.as_str())
        .is_some_and(|fixed| newer(fixed, current));
    Some(Update { version: version.to_string(), security })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        let m = serde_json::json!({"version": "0.12.3", "security_since": "0.12.1"});
        assert_eq!(evaluate(&m, "0.12.3"), None);
        assert_eq!(evaluate(&m, "0.12.2"), Some(Update { version: "0.12.3".into(), security: false }));
        assert_eq!(evaluate(&m, "0.12.0"), Some(Update { version: "0.12.3".into(), security: true }));
        assert_eq!(evaluate(&serde_json::json!({"version": "0.13.0"}), "0.12.9").map(|u| u.security), Some(false));
        assert!(newer("0.10.0", "0.9.9"));
        assert_eq!(evaluate(&serde_json::json!({"version": "garbage"}), "0.1.0"), None);
    }

    /// Network: cargo test -p remotefriend -- --ignored live_manifest
    #[test]
    #[ignore]
    fn live_manifest() {
        remote_friend_common::tls::init_crypto();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let manifest = rt.block_on(fetch()).unwrap();
        assert!(parse_version(manifest["version"].as_str().unwrap()).is_some());
        assert!(evaluate(&manifest, "0.0.1").is_some());
    }
}
