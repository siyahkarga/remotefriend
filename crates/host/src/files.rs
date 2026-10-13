//! Files: incoming chunks (ordered, size/count limits, safe names, atomic completion), and
//! browsing/reading this computer's files for a viewer that downloads them.
//!
//! What a viewer reaches depends on how far it is trusted ([`Scope`]): a guest only the
//! shared folder (Downloads/RemoteFriend), a trusted device its user's folders and data disks
//! without system folders or hidden files, the owner's own devices everything. Every path a
//! viewer names is resolved (symbolic links included) and checked before it is listed, read
//! or written to.

use anyhow::{Context, Result};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct IncomingTransfer {
    file: std::fs::File,
    part_path: std::path::PathBuf,
    dir: std::path::PathBuf,
    name: String,
    expected: u64,
    total: u64,
    updated: Instant,
}

static TRANSFERS: OnceLock<std::sync::Mutex<std::collections::HashMap<u64, IncomingTransfer>>> = OnceLock::new();

fn transfer_map() -> &'static std::sync::Mutex<std::collections::HashMap<u64, IncomingTransfer>> {
    TRANSFERS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn max_file_bytes() -> u64 {
    std::env::var("RF_MAX_FILE_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n: &u64| *n > 0)
        .unwrap_or(512 * 1024 * 1024)
}

/// Incoming files go to REMOTE_FRIEND_DIR, otherwise Downloads/RemoteFriend.
pub(crate) fn receive_dir() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("REMOTE_FRIEND_DIR") {
        return d.into();
    }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok().map(std::path::PathBuf::from);
    let Some(home) = home else {
        return remote_friend_common::identity::config_dir().join("received");
    };
    // On Linux the localized folder name (e.g. a translated "Downloads") is listed in user-dirs.dirs.
    #[cfg(target_os = "linux")]
    if let Ok(cfg) = std::fs::read_to_string(home.join(".config/user-dirs.dirs")) {
        for line in cfg.lines() {
            if let Some(v) = line.strip_prefix("XDG_DOWNLOAD_DIR=") {
                let v = v.trim().trim_matches('"').replace("$HOME", &home.to_string_lossy());
                let p = std::path::PathBuf::from(v);
                if p.is_dir() {
                    return p.join("RemoteFriend");
                }
            }
        }
    }
    let downloads = home.join("Downloads");
    if downloads.is_dir() {
        return downloads.join("RemoteFriend");
    }
    remote_friend_common::identity::config_dir().join("received")
}

fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("file");
    let mut out = String::with_capacity(base.len().min(128));
    for c in base.chars().take(128) {
        if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')' | '+' | ',') {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    // No hidden files, no names made only of dots or spaces.
    let out = out.trim().trim_start_matches('.').trim().to_string();
    if out.is_empty() {
        "file".into()
    } else {
        out
    }
}

/// `dir/name`, or `dir/name (1).ext`, `(2)`... if that file already exists.
fn unique_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    for n in 1..10_000 {
        let p = dir.join(format!("{stem} ({n}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{stem} ({}){ext}", remote_friend_common::new_secret_hex().get(..8).unwrap_or("x")))
}

/// The sender canceled: delete the partial file.
pub fn cancel(transfer_id: u64) -> Option<String> {
    let t = transfer_map().lock().unwrap().remove(&transfer_id)?;
    let _ = std::fs::remove_file(&t.part_path);
    Some(t.name)
}

/// Largest file a viewer may send (guests: RF_MAX_FILE_BYTES, 512 MB by default).
pub(crate) fn max_upload(scope: Scope) -> u64 {
    match scope {
        Scope::Shared => max_file_bytes(),
        Scope::NoSystem | Scope::All => max_file_bytes().max(64 << 30),
    }
}

/// Write one chunk of a file a viewer sends into `dir` (checked by the caller with
/// [`upload_dir`]). Returns the final path once it is complete.
pub(crate) fn save_chunk(c: remote_friend_common::FileChunk, dir: std::path::PathBuf, max: u64) -> Result<Option<String>> {
    save_chunk_limited(c, max, dir)
}

/// Write one chunk of a file downloaded from another computer (the user asked for it, so
/// the size limit for unrequested files does not apply).
pub fn save_download_chunk(c: remote_friend_common::FileChunk) -> Result<Option<String>> {
    save_chunk_limited(c, 1 << 40, receive_dir())
}

fn save_chunk_limited(c: remote_friend_common::FileChunk, max: u64, dir: std::path::PathBuf) -> Result<Option<String>> {
    use std::io::Write as _;

    const MAX_CHUNK: usize = 256 * 1024;
    if c.transfer_id == 0 {
        anyhow::bail!("invalid transfer ID");
    }
    let empty_file = c.total == 0 && c.data.is_empty() && c.offset == 0 && c.last;
    if (c.total == 0 && !empty_file) || c.total > max {
        anyhow::bail!("file size out of range: {}", c.total);
    }
    if (c.data.is_empty() && !empty_file) || c.data.len() > MAX_CHUNK {
        anyhow::bail!("file chunk size out of range: {}", c.data.len());
    }
    let end = c.offset
        .checked_add(c.data.len() as u64)
        .context("file offset overflow")?;
    if end > c.total {
        anyhow::bail!("file chunk exceeds the declared total");
    }
    // Reject a wrong last-chunk flag before creating/writing the file.
    // Otherwise an attacker could fill the transfer slots with invalid first chunks
    // for 10 minutes.
    let reached_end = end == c.total;
    if c.last != reached_end {
        anyhow::bail!("file last-chunk flag does not match the total size");
    }

    if c.offset == 0 {
        if dir == receive_dir() {
            std::fs::create_dir_all(&dir).with_context(|| format!("failed to create folder: {}", dir.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        anyhow::ensure!(dir.is_dir(), "the folder {} does not exist", dir.display());
    }

    let mut transfers = transfer_map().lock().unwrap();
    // Don't keep unfinished transfers open forever.
    let stale: Vec<u64> = transfers
        .iter()
        .filter(|(_, t)| t.updated.elapsed() > Duration::from_secs(600))
        .map(|(id, _)| *id)
        .collect();
    for id in stale {
        if let Some(t) = transfers.remove(&id) {
            let _ = std::fs::remove_file(t.part_path);
        }
    }

    if c.offset == 0 {
        if transfers.len() >= 8 {
            anyhow::bail!("too many concurrent file transfers");
        }
        if transfers.contains_key(&c.transfer_id) {
            anyhow::bail!("transfer ID already in use");
        }
        let safe = safe_file_name(&c.name);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let random = remote_friend_common::new_secret_hex();
        // Written under a hidden temporary name, renamed to the real name when complete.
        let part_path = dir.join(format!(".rf-{stamp}-{}.part", &random[..8]));
        let mut opts = std::fs::OpenOptions::new();
        opts.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(&part_path)?;
        transfers.insert(c.transfer_id, IncomingTransfer {
            file,
            part_path,
            dir: dir.clone(),
            name: safe,
            expected: 0,
            total: c.total,
            updated: Instant::now(),
        });
    }

    let complete = {
        let state = transfers
            .get_mut(&c.transfer_id)
            .context("transfer did not start with the first chunk")?;
        if state.total != c.total || state.expected != c.offset {
            anyhow::bail!("file chunks out of order or total size changed");
        }
        state.file.write_all(&c.data)?;
        state.expected = end;
        state.updated = Instant::now();
        debug_assert_eq!(state.expected == state.total, reached_end);
        reached_end
    };

    if complete {
        let mut state = transfers.remove(&c.transfer_id).expect("transfer existed a moment ago");
        state.file.flush()?;
        state.file.sync_all()?;
        drop(state.file);
        let final_path = unique_path(&state.dir, &state.name);
        std::fs::rename(&state.part_path, &final_path)?;
        tracing::info!("file complete: {} ({} bytes)", final_path.display(), state.total);
        return Ok(Some(final_path.display().to_string()));
    }
    tracing::debug!("file chunk: id={} {}/{}", c.transfer_id, end, c.total);
    Ok(None)
}


// ---- this computer's files, for viewers ----

/// What a viewer may reach (see the module comment).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Only the shared folder (Downloads/RemoteFriend).
    Shared,
    /// The user's folders and data disks, without system folders and hidden files.
    NoSystem,
    /// Everything.
    All,
}

impl Scope {
    /// For the viewer: "shared", "disk" or "all".
    pub(crate) fn name(self) -> &'static str {
        match self {
            Scope::Shared => "shared",
            Scope::NoSystem => "disk",
            Scope::All => "all",
        }
    }
}

/// One entry of a folder listing.
#[derive(serde::Serialize)]
pub(crate) struct Entry {
    /// Name (in the list of starting points: the whole path).
    pub n: String,
    /// Folder?
    pub d: bool,
    /// Size in bytes (files).
    pub s: u64,
}

fn home_dir() -> std::path::PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/"))
}

/// A path as viewers see it (Windows: without the \\?\ prefix canonicalize adds).
fn shown(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    #[cfg(windows)]
    let s = s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s);
    s
}

/// The shared folder, created if needed.
fn shared_root() -> Result<std::path::PathBuf> {
    let dir = receive_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create folder: {}", dir.display()))?;
    Ok(dir.canonicalize()?)
}

/// Where a trusted device may start: the user's folder and data disks (resolved paths).
fn starting_points() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    #[cfg(windows)]
    for l in b'A'..=b'Z' {
        let d = std::path::PathBuf::from(format!("{}:\\", l as char));
        if d.is_dir() {
            roots.push(d);
        }
    }
    #[cfg(not(windows))]
    {
        roots.push(home_dir());
        let user = std::env::var("USER").unwrap_or_default();
        let mut mounts = vec!["/Volumes".to_string(), "/mnt".to_string()];
        if !user.is_empty() {
            mounts.push(format!("/media/{user}"));
            mounts.push(format!("/run/media/{user}"));
        }
        for m in mounts {
            if let Ok(list) = std::fs::read_dir(&m) {
                roots.extend(list.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
            }
        }
    }
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    for r in roots.into_iter().filter_map(|r| r.canonicalize().ok()) {
        // The system disk itself (macOS lists it in /Volumes) is not a data disk.
        if !cfg!(windows) && r == std::path::Path::new("/") {
            continue;
        }
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

/// Hidden on this system: dot files, and on Windows the hidden/system attributes.
fn hidden(name: &str, meta: Option<&std::fs::Metadata>) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const HIDDEN: u32 = 0x2;
        const SYSTEM: u32 = 0x4;
        if meta.is_some_and(|m| m.file_attributes() & (HIDDEN | SYSTEM) != 0) {
            return true;
        }
    }
    #[cfg(not(windows))]
    let _ = meta;
    name.starts_with('$') && cfg!(windows)
}

/// Windows folders at the top of a drive that hold the system or programs.
#[cfg(windows)]
const SYSTEM_FOLDERS: &[&str] = &[
    "windows", "program files", "program files (x86)", "programdata", "recovery", "system volume information",
    "perflogs", "boot", "config.msi", "documents and settings", "msocache", "intel", "amd", "nvidia", "drivers",
    "onedrivetemp", "inetpub", "windows.old",
];

/// May a trusted device reach `p` (resolved)? Inside a starting point, no hidden part on
/// the way, no system folder, and on Windows only this user's folder and Public in Users.
fn reachable(p: &std::path::Path, roots: &[std::path::PathBuf]) -> bool {
    let Some(root) = roots.iter().find(|r| p.starts_with(r)) else { return false };
    let rest: Vec<String> = p
        .strip_prefix(root)
        .map(|r| r.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    let mut at = root.clone();
    for (i, part) in rest.iter().enumerate() {
        at.push(part);
        if hidden(part, std::fs::symlink_metadata(&at).ok().as_ref()) {
            return false;
        }
        #[cfg(windows)]
        {
            let lower = part.to_lowercase();
            if i == 0 && SYSTEM_FOLDERS.contains(&lower.as_str()) {
                return false;
            }
            if i == 1 && rest[0].eq_ignore_ascii_case("users") {
                let me = std::env::var("USERNAME").unwrap_or_default().to_lowercase();
                if lower != me && lower != "public" {
                    return false;
                }
            }
            if lower == "appdata" {
                return false;
            }
        }
        #[cfg(target_os = "macos")]
        if i == 0 && *root == home_dir() && part == "Library" {
            return false;
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        let _ = i;
    }
    true
}

/// Resolve a path a viewer named and check that it may reach it.
fn resolve(path: &str, scope: Scope) -> Result<std::path::PathBuf> {
    let p = std::path::PathBuf::from(path);
    let p = p.canonicalize().with_context(|| format!("cannot open {path}"))?;
    let ok = match scope {
        Scope::All => true,
        Scope::Shared => p.starts_with(shared_root()?),
        Scope::NoSystem => reachable(&p, &starting_points()),
    };
    anyhow::ensure!(ok, "{path} is not shared with you");
    Ok(p)
}

/// The folder a file a viewer sends is saved in: `wanted` if it may write there, otherwise
/// the shared folder.
pub(crate) fn upload_dir(wanted: Option<&str>, scope: Scope) -> std::path::PathBuf {
    if scope != Scope::Shared {
        if let Some(p) = wanted.filter(|w| !w.trim().is_empty()).and_then(|w| resolve(w, scope).ok()).filter(|p| p.is_dir()) {
            return p;
        }
    }
    receive_dir()
}

/// The shared folder as viewers see it.
pub(crate) fn shared_shown() -> String {
    shown(&receive_dir())
}

/// List a folder for a viewer. An empty path is where it starts: the shared folder, the
/// list of starting points (trusted) or the home folder (everything). Returns (path,
/// parent, entries): folders first, at most 2000 entries. A parent of "" goes back to the
/// starting points.
pub(crate) fn list_dir(path: &str, scope: Scope) -> Result<(String, Option<String>, Vec<Entry>)> {
    let roots = if scope == Scope::NoSystem { starting_points() } else { Vec::new() };
    if path.trim().is_empty() && scope == Scope::NoSystem {
        let entries = roots.iter().map(|r| Entry { n: shown(r), d: true, s: 0 }).collect();
        return Ok((String::new(), None, entries));
    }
    #[cfg(windows)]
    if path.trim().is_empty() && scope == Scope::All {
        let drives = (b'A'..=b'Z')
            .map(|l| format!("{}:\\", l as char))
            .filter(|d| std::path::Path::new(d).is_dir())
            .map(|d| Entry { n: d, d: true, s: 0 })
            .collect();
        return Ok((String::new(), None, drives));
    }
    let dir = match (path.trim().is_empty(), scope) {
        (true, Scope::Shared) => shared_root()?,
        (true, _) => home_dir().canonicalize()?,
        (false, _) => resolve(path, scope)?,
    };
    anyhow::ensure!(dir.is_dir(), "{} is not a folder", shown(&dir));
    let mut entries = Vec::new();
    for e in std::fs::read_dir(&dir).with_context(|| format!("cannot open {}", shown(&dir)))?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(meta) = e.metadata() else { continue };
        if scope != Scope::All && hidden(&name, Some(&meta)) {
            continue;
        }
        if scope == Scope::NoSystem && !reachable(&dir.join(&name), &roots) {
            continue;
        }
        entries.push(Entry { n: name, d: meta.is_dir(), s: if meta.is_file() { meta.len() } else { 0 } });
        if entries.len() >= 2000 {
            break;
        }
    }
    entries.sort_by(|a, b| b.d.cmp(&a.d).then_with(|| a.n.to_lowercase().cmp(&b.n.to_lowercase())));
    let parent = match scope {
        Scope::Shared if dir == shared_root()? => None,
        Scope::NoSystem if roots.contains(&dir) => Some(String::new()),
        _ => match dir.parent() {
            Some(p) => Some(shown(p)),
            None if cfg!(windows) => Some(String::new()),
            None => None,
        },
    };
    Ok((shown(&dir), parent, entries))
}

/// Open a regular file for a viewer to download: (file, name, size, path).
pub(crate) fn open_for_download(path: &str, scope: Scope) -> Result<(std::fs::File, String, u64, String)> {
    let p = resolve(path, scope)?;
    let meta = std::fs::metadata(&p).with_context(|| format!("cannot read {path}"))?;
    anyhow::ensure!(meta.is_file(), "only files can be downloaded");
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
    let file = std::fs::File::open(&p).with_context(|| format!("cannot open {path}"))?;
    Ok((file, name, meta.len(), shown(&p)))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_trusted_device_stays_out_of_hidden_and_outside_folders() {
        let base = std::env::temp_dir().join(format!("rf-files-{}", std::process::id()));
        let docs = base.join("docs");
        std::fs::create_dir_all(docs.join("sub")).unwrap();
        std::fs::create_dir_all(base.join(".ssh")).unwrap();
        std::fs::write(base.join(".ssh/id_ed25519"), "key").unwrap();
        std::fs::write(docs.join("a.txt"), "a").unwrap();
        std::os::unix::fs::symlink("/etc", docs.join("etc-link")).unwrap();
        let roots = vec![base.canonicalize().unwrap()];
        let r = |p: &std::path::Path| reachable(&p.canonicalize().unwrap(), &roots);
        assert!(r(&docs.join("sub")));
        assert!(r(&docs.join("a.txt")));
        assert!(!r(&base.join(".ssh/id_ed25519"))); // hidden folder on the way
        assert!(!r(&docs.join("etc-link"))); // a link that leads outside
        assert!(!r(std::path::Path::new("/etc/passwd")));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_guest_only_reaches_the_shared_folder() {
        let shared = std::env::temp_dir().join(format!("rf-shared-{}", std::process::id()));
        std::env::set_var("REMOTE_FRIEND_DIR", &shared);
        std::fs::create_dir_all(shared.join("inside")).unwrap();
        assert!(resolve(&shared.join("inside").display().to_string(), Scope::Shared).is_ok());
        assert!(resolve(&shared.join("..").display().to_string(), Scope::Shared).is_err());
        assert!(resolve("/etc/passwd", Scope::Shared).is_err());
        assert!(resolve(&home_dir().display().to_string(), Scope::Shared).is_err());
        assert!(resolve("/etc", Scope::All).is_ok());
        assert_eq!(upload_dir(Some("/etc"), Scope::Shared), receive_dir());
        assert_eq!(upload_dir(Some("/etc"), Scope::NoSystem), receive_dir());
        let (path, parent, _) = list_dir("", Scope::Shared).unwrap();
        assert_eq!(path, shared.canonicalize().unwrap().display().to_string());
        assert!(parent.is_none());
        std::fs::remove_dir_all(&shared).unwrap();
    }
}
