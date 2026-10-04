//! Files: incoming chunks (ordered, size/count limits, safe names, atomic completion), and
//! browsing/reading this computer's files for a viewer that downloads them.

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

/// Write one chunk of a file a viewer sends. Returns the final path once it is complete.
pub(crate) fn save_chunk(c: remote_friend_common::FileChunk) -> Result<Option<String>> {
    save_chunk_limited(c, max_file_bytes())
}

/// Write one chunk of a file downloaded from another computer (the user asked for it, so
/// the size limit for unrequested files does not apply).
pub fn save_download_chunk(c: remote_friend_common::FileChunk) -> Result<Option<String>> {
    save_chunk_limited(c, 1 << 40)
}

fn save_chunk_limited(c: remote_friend_common::FileChunk, max: u64) -> Result<Option<String>> {
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

    let dir = receive_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create folder: {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
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


// ---- this computer's files, for viewers that download ----

/// One entry of a folder listing.
#[derive(serde::Serialize)]
pub(crate) struct Entry {
    /// Name.
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

/// List a folder (empty path: the home folder). Returns (path, parent, entries): folders
/// first, hidden files left out, at most 2000 entries. On Windows the parent of a drive is
/// "" which lists the drives.
pub(crate) fn list_dir(path: &str) -> Result<(String, Option<String>, Vec<Entry>)> {
    #[cfg(windows)]
    if path == "" {
        let drives = (b'A'..=b'Z')
            .map(|l| format!("{}:\\", l as char))
            .filter(|d| std::path::Path::new(d).is_dir())
            .map(|d| Entry { n: d, d: true, s: 0 })
            .collect();
        return Ok(("".into(), None, drives));
    }
    let dir = if path.trim().is_empty() { home_dir() } else { std::path::PathBuf::from(path) };
    let dir = dir.canonicalize().with_context(|| format!("cannot open {}", dir.display()))?;
    let mut entries = Vec::new();
    for e in std::fs::read_dir(&dir).with_context(|| format!("cannot open {}", dir.display()))?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        entries.push(Entry { n: name, d: meta.is_dir(), s: if meta.is_file() { meta.len() } else { 0 } });
        if entries.len() >= 2000 {
            break;
        }
    }
    entries.sort_by(|a, b| b.d.cmp(&a.d).then_with(|| a.n.to_lowercase().cmp(&b.n.to_lowercase())));
    let parent = match dir.parent() {
        Some(p) => Some(p.display().to_string()),
        None if cfg!(windows) => Some(String::new()),
        None => None,
    };
    let shown = dir.display().to_string();
    #[cfg(windows)]
    let shown = shown.trim_start_matches(r"\\?\").to_string();
    Ok((shown, parent.map(|p| p.trim_start_matches(r"\\?\").to_string()), entries))
}

/// Open a regular file for download: (file, name, size).
pub(crate) fn open_for_download(path: &str) -> Result<(std::fs::File, String, u64)> {
    let p = std::path::Path::new(path);
    let meta = std::fs::metadata(p).with_context(|| format!("cannot read {path}"))?;
    anyhow::ensure!(meta.is_file(), "only files can be downloaded");
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
    Ok((std::fs::File::open(p).with_context(|| format!("cannot open {path}"))?, name, meta.len()))
}
