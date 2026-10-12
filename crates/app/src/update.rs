//! Updates. Twice a day the app asks blobidea.com which version is current (the manifest the
//! download page uses); only the request itself is sent. Settings → General turns it off.
//!
//! Installing: the manifest is trusted only with a valid Ed25519 signature from the release key
//! (manifest.sig, made on the release PC by deploy/publish-website.sh). The installer for this
//! system is downloaded from blobidea.com, checked against the size and SHA-256 in the signed
//! manifest, and only then run:
//! - Windows: the setup program, silently (it closes RemoteFriend), then RemoteFriend starts again.
//! - macOS: the app in the disk image replaces this one, then it starts again.
//! - Linux .deb: `pkexec apt-get install` (the desktop asks for the password), then a restart.
//! - Linux tar.gz (~/.local): its install.sh, then a restart.
//!
//! Anything else (a self-built copy, an unknown install place) only gets the download page.

use anyhow::{bail, Context, Result};
use base64::Engine;
use sha2::Digest;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HOST: &str = "blobidea.com";
const DIR: &str = "/downloads/remotefriend/";
pub const DOWNLOAD_PAGE: &str = "https://blobidea.com/products/remotefriend";
/// Public half of the release signing key (deploy/make-update-key.sh). Empty: updates are
/// only offered as a download.
const UPDATE_PUBLIC_KEY: &str = "VITwryCXz1tAZD25eSss3miNIOkMyfH0PT8/XKl2i5o=";

#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub version: String,
    /// The installed version has a known security problem that this one fixes.
    pub security: bool,
    /// The installer for this system, from a signed manifest; None: download page only.
    pub install: Option<Installer>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Installer {
    pub file: String,
    pub size: u64,
    pub sha256: String,
    /// The desktop asks for the computer's password (Linux .deb).
    pub needs_password: bool,
}

/// Where an update stands, for the strip above the page.
#[derive(Clone, Debug, Default)]
pub enum Progress {
    #[default]
    Idle,
    Downloading(u64, u64),
    Checking,
    /// The installer runs; the app restarts or quits by itself.
    Installing,
    Failed(String),
}

pub fn enabled() -> bool {
    !remote_friend_common::identity::load_host_settings().no_update_check
}

pub fn set_enabled(on: bool) {
    let mut s = remote_friend_common::identity::load_host_settings();
    s.no_update_check = !on;
    remote_friend_common::identity::save_host_settings(&s);
}

/// Install updates without a click (when nobody is connected and no password is needed).
pub fn automatic() -> bool {
    !remote_friend_common::identity::load_host_settings().no_auto_update
}

pub fn set_automatic(on: bool) {
    let mut s = remote_friend_common::identity::load_host_settings();
    s.no_auto_update = !on;
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
                    match check().await {
                        Ok(update) => {
                            if let Some(u) = &update {
                                tracing::info!("update available: {} (security: {}, installable: {})", u.version, u.security, u.install.is_some());
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

async fn check() -> Result<Option<Update>> {
    let manifest_bytes = get(&format!("{DIR}manifest.json"), 64 * 1024, |_, _| {}).await?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
    // An unsigned or badly signed manifest still tells that a version exists, but nothing from
    // it is installed.
    let signed = match get(&format!("{DIR}manifest.sig"), 4096, |_, _| {}).await {
        Ok(sig) => verify(&manifest_bytes, &sig),
        Err(_) => false,
    };
    if !signed {
        tracing::debug!("update manifest is not signed with the release key: download only");
    }
    Ok(evaluate(&manifest, env!("CARGO_PKG_VERSION"), signed.then(install_kind).flatten()))
}

/// Ed25519 signature (base64 text) of the manifest bytes by the release key.
fn verify(manifest: &[u8], sig_b64: &[u8]) -> bool {
    let b64 = base64::engine::general_purpose::STANDARD;
    let (Ok(key), Ok(sig)) = (b64.decode(UPDATE_PUBLIC_KEY), b64.decode(String::from_utf8_lossy(sig_b64).trim())) else {
        return false;
    };
    key.len() == 32 && ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &key).verify(manifest, &sig).is_ok()
}

/// GET a file from blobidea.com (HTTP/1.0 over TLS), at most `limit` bytes.
async fn get(path: &str, limit: u64, mut progress: impl FnMut(u64, u64)) -> Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut tls = remote_friend_common::tls::tls_connect(&format!("{HOST}:443"), HOST, None).await?;
    // HTTP/1.0: the answer comes in one piece, no chunked encoding to undo.
    let request = format!(
        "GET {path} HTTP/1.0\r\nHost: {HOST}\r\nUser-Agent: RemoteFriend/{}\r\n\r\n",
        env!("CARGO_PKG_VERSION")
    );
    tls.write_all(request.as_bytes()).await?;
    let mut data = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut body_at = None;
    let mut total = 0u64;
    loop {
        let n = tokio::time::timeout(Duration::from_secs(30), tls.read(&mut buf)).await.context("download stalled")??;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if body_at.is_none() {
            if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&data[..end]).to_string();
                let status = head.split_whitespace().nth(1).unwrap_or("");
                if status != "200" {
                    bail!("HTTP {status}");
                }
                total = head
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
                    .unwrap_or(0);
                body_at = Some(end + 4);
            }
        }
        if let Some(at) = body_at {
            let got = (data.len() - at) as u64;
            if got > limit {
                bail!("file larger than expected");
            }
            progress(got, total);
        }
    }
    let at = body_at.context("malformed HTTP answer")?;
    Ok(data.split_off(at))
}

fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut parts = v.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??, parts.next().flatten().unwrap_or(0)))
}

fn newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(a), Some(b)) if a > b)
}

/// `security_since`: the first version that fixes the newest known security problem.
/// `kind`: how this copy was installed, when the manifest is signed.
fn evaluate(manifest: &serde_json::Value, current: &str, kind: Option<Kind>) -> Option<Update> {
    let version = manifest.get("version")?.as_str()?;
    if !newer(version, current) {
        return None;
    }
    let security = manifest
        .get("security_since")
        .and_then(|v| v.as_str())
        .is_some_and(|fixed| newer(fixed, current));
    let install = kind.and_then(|kind| {
        let name = kind.file();
        let entry = manifest.get("files")?.as_array()?.iter().find(|f| f.get("file").and_then(|n| n.as_str()) == Some(name))?;
        let sha256 = entry.get("sha256")?.as_str()?.to_ascii_lowercase();
        let size = entry.get("size")?.as_u64()?;
        (sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit()) && size > 0 && size < 512 << 20).then(|| Installer {
            file: name.to_string(),
            size,
            sha256,
            needs_password: matches!(kind, Kind::Deb),
        })
    });
    Some(Update { version: version.to_string(), security, install })
}

/// How this copy of RemoteFriend was installed.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    WindowsSetup,
    MacApp,
    Deb,
    Tarball,
}

impl Kind {
    fn file(self) -> &'static str {
        match self {
            Kind::WindowsSetup => "RemoteFriend-Setup.exe",
            Kind::MacApp => "RemoteFriend-macOS.dmg",
            Kind::Deb => "RemoteFriend-linux-amd64.deb",
            Kind::Tarball => "RemoteFriend-linux-x86_64.tar.gz",
        }
    }
}

fn install_kind() -> Option<Kind> {
    let exe = std::env::current_exe().ok()?;
    kind_for(&exe, std::env::var_os("HOME").map(PathBuf::from).as_deref())
}

fn kind_for(exe: &Path, home: Option<&Path>) -> Option<Kind> {
    let text = exe.to_string_lossy().replace('\\', "/").to_lowercase();
    if cfg!(windows) {
        return text.ends_with("/programs/remotefriend/remotefriend.exe").then_some(Kind::WindowsSetup);
    }
    if cfg!(target_os = "macos") {
        return text.contains(".app/contents/macos/").then_some(Kind::MacApp);
    }
    if !cfg!(target_arch = "x86_64") {
        return None;
    }
    if text == "/usr/bin/remotefriend" {
        return Some(Kind::Deb);
    }
    let local = home?.join(".local/bin/remotefriend");
    (exe == local).then_some(Kind::Tarball)
}

/// Download, check and install `update` in the background; `progress` tells the UI.
pub fn install(update: &Update, progress: Arc<Mutex<Progress>>, ctx: egui::Context) {
    let Some(inst) = update.install.clone() else { return };
    let version = update.version.clone();
    *progress.lock().unwrap() = Progress::Downloading(0, inst.size);
    std::thread::spawn(move || {
        let report = |p: Progress| {
            *progress.lock().unwrap() = p;
            ctx.request_repaint();
        };
        let result = (|| -> Result<()> {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            let data = rt.block_on(get(&format!("{DIR}{}", inst.file), inst.size, |got, _| {
                report(Progress::Downloading(got, inst.size));
            }))?;
            report(Progress::Checking);
            let digest = hex::encode(sha2::Sha256::digest(&data));
            if data.len() as u64 != inst.size || digest != inst.sha256 {
                bail!("the download does not match the signed checksum");
            }
            let dir = std::env::temp_dir().join(format!("remotefriend-update-{version}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir)?;
            let file = dir.join(&inst.file);
            std::fs::write(&file, &data)?;
            report(Progress::Installing);
            run_installer(install_kind().context("unknown install place")?, &file, &dir)
        })();
        if let Err(e) = result {
            tracing::warn!("update failed: {e:#}");
            report(Progress::Failed(format!("{e:#}")));
        }
    });
}

fn run_installer(kind: Kind, file: &Path, dir: &Path) -> Result<()> {
    use std::process::Command;
    let exe = std::env::current_exe()?;
    match kind {
        Kind::WindowsSetup => {
            // The setup closes RemoteFriend itself; cmd waits for it and starts the new version.
            let line = format!("\"\"{}\" /S & start \"\" \"{}\"\"", file.display(), exe.display());
            let mut cmd = Command::new("cmd");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                // cmd has its own quoting: the line goes in as it is.
                cmd.arg("/C").raw_arg(&line).creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }
            #[cfg(not(windows))]
            cmd.arg("/C").arg(&line);
            cmd.spawn()?;
            std::thread::sleep(Duration::from_secs(1));
            std::process::exit(0);
        }
        Kind::MacApp => {
            let bundle = exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).context("no app bundle")?.to_path_buf();
            let mount = dir.join("mnt");
            std::fs::create_dir_all(&mount)?;
            let ok = Command::new("hdiutil")
                .args(["attach", "-nobrowse", "-noautoopen", "-quiet", "-mountpoint"])
                .arg(&mount)
                .arg(file)
                .status()?
                .success();
            if !ok {
                bail!("could not open the disk image");
            }
            let app = std::fs::read_dir(&mount)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .find(|p| p.extension().is_some_and(|e| e == "app"));
            let copied = match &app {
                Some(app) => Command::new("ditto").arg(app).arg(&bundle).status().map(|s| s.success()).unwrap_or(false),
                None => false,
            };
            let _ = Command::new("hdiutil").args(["detach", "-quiet"]).arg(&mount).status();
            if !copied {
                bail!("could not replace {} (no write permission?)", bundle.display());
            }
            Command::new("open").arg("-n").arg(&bundle).spawn()?;
            std::process::exit(0);
        }
        Kind::Deb => {
            let ok = Command::new("pkexec").args(["apt-get", "install", "-y"]).arg(file).status().context("pkexec is missing")?.success();
            if !ok {
                bail!("installing was canceled or failed");
            }
            crate::restart_app();
            Ok(())
        }
        Kind::Tarball => {
            let ok = Command::new("tar").arg("-xzf").arg(file).arg("-C").arg(dir).status()?.success();
            if !ok {
                bail!("could not unpack the update");
            }
            let script = find_file(dir, "install.sh", 3).context("install.sh missing in the update")?;
            let ok = Command::new("sh").arg(&script).status()?.success();
            if !ok {
                bail!("install.sh failed");
            }
            crate::restart_app();
            Ok(())
        }
    }
}

fn find_file(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == name) {
            return Some(path);
        }
        if depth > 0 && path.is_dir() {
            if let Some(found) = find_file(&path, name, depth - 1) {
                return Some(found);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> serde_json::Value {
        serde_json::json!({"version": "0.12.3", "security_since": "0.12.1", "files": [
            {"file": "RemoteFriend-linux-amd64.deb", "size": 100, "sha256": "ab".repeat(32)},
        ]})
    }

    #[test]
    fn versions() {
        let m = manifest();
        assert_eq!(evaluate(&m, "0.12.3", None), None);
        assert_eq!(evaluate(&m, "0.12.2", None), Some(Update { version: "0.12.3".into(), security: false, install: None }));
        assert!(evaluate(&m, "0.12.0", None).unwrap().security);
        assert_eq!(evaluate(&serde_json::json!({"version": "0.13.0"}), "0.12.9", None).map(|u| u.security), Some(false));
        assert!(newer("0.10.0", "0.9.9"));
        assert_eq!(evaluate(&serde_json::json!({"version": "garbage"}), "0.1.0", None), None);
    }

    #[test]
    fn only_the_file_for_this_install_from_a_signed_manifest() {
        let deb = evaluate(&manifest(), "0.12.0", Some(Kind::Deb)).unwrap().install.unwrap();
        assert_eq!(deb.file, "RemoteFriend-linux-amd64.deb");
        assert!(deb.needs_password);
        // No entry for this kind: download page only.
        assert_eq!(evaluate(&manifest(), "0.12.0", Some(Kind::Tarball)).unwrap().install, None);
    }

    #[test]
    fn a_wrong_signature_is_refused() {
        // Whatever key is built in, garbage never verifies.
        assert!(!verify(b"{}", b"bm90IGEgc2lnbmF0dXJl"));
        assert!(!verify(b"{}", b""));
    }

    #[test]
    fn a_release_signature_verifies() {
        // Made with the release key the way deploy/publish-website.sh signs (openssl pkeyutl -rawin).
        let message = b"RemoteFriend update signature test\n";
        let signature = b"Ws/fsBCRtWR3QZZ7BtLQjg5Mo/7f5jtHyi8aoX5kTqO6qekoJ143/7hC5YWRAXG/erFkWRMWcsRBYWFlbxp8CQ==";
        assert!(verify(message, signature));
        assert!(!verify(b"RemoteFriend update signature test!", signature));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn install_places_on_linux() {
        let home = Path::new("/home/ali");
        if cfg!(target_arch = "x86_64") {
            assert_eq!(kind_for(Path::new("/usr/bin/remotefriend"), Some(home)), Some(Kind::Deb));
            assert_eq!(kind_for(Path::new("/home/ali/.local/bin/remotefriend"), Some(home)), Some(Kind::Tarball));
        }
        assert_eq!(kind_for(Path::new("/home/ali/src/target/debug/remotefriend"), Some(home)), None);
    }

    /// Network: cargo test -p remotefriend -- --ignored live_manifest
    #[test]
    #[ignore]
    fn live_manifest() {
        remote_friend_common::tls::init_crypto();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let bytes = rt.block_on(get(&format!("{DIR}manifest.json"), 64 * 1024, |_, _| {})).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(parse_version(manifest["version"].as_str().unwrap()).is_some());
        assert!(evaluate(&manifest, "0.0.1", None).is_some());
    }
}
