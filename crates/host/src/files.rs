//! Incoming files: ordered chunks, size/count limits, safe names, atomic completion.

use anyhow::{Context, Result};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct IncomingTransfer {
    file: std::fs::File,
    part_path: std::path::PathBuf,
    final_path: std::path::PathBuf,
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
        if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() || out == "." || out == ".." {
        "file".into()
    } else {
        out
    }
}

/// Write one chunk. Returns the final path once the file is complete.
pub(crate) fn save_chunk(c: remote_friend_common::FileChunk) -> Result<Option<String>> {
    use std::io::Write as _;

    const MAX_CHUNK: usize = 256 * 1024;
    if c.transfer_id == 0 {
        anyhow::bail!("invalid transfer ID");
    }
    if c.total == 0 || c.total > max_file_bytes() {
        anyhow::bail!("file size out of range: {}", c.total);
    }
    if c.data.is_empty() || c.data.len() > MAX_CHUNK {
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
        let final_path = dir.join(format!("rf_{stamp}_{}_{}", &random[..8], safe));
        let part_path = final_path.with_extension("part");
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
            final_path,
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
        std::fs::rename(&state.part_path, &state.final_path)?;
        tracing::info!("file complete: {} ({} bytes)", state.final_path.display(), state.total);
        crate::status::notice(format!("File received: {}", state.final_path.display()));
        return Ok(Some(state.final_path.display().to_string()));
    }
    tracing::debug!("file chunk: id={} {}/{}", c.transfer_id, end, c.total);
    Ok(None)
}

