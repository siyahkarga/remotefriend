//! "Start RemoteFriend when I log in" (per user, no admin rights needed).
//!
//! - Linux: ~/.config/autostart/remotefriend.desktop
//! - macOS: ~/Library/LaunchAgents/io.remotefriend.app.plist
//! - Windows: HKCU\Software\Microsoft\Windows\CurrentVersion\Run\RemoteFriend

#[cfg(not(windows))]
fn home() -> std::path::PathBuf {
    std::env::var("HOME").map(std::path::PathBuf::from).unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn entry_path() -> std::path::PathBuf {
    home().join(".config/autostart/remotefriend.desktop")
}

#[cfg(target_os = "macos")]
fn entry_path() -> std::path::PathBuf {
    home().join("Library/LaunchAgents/io.remotefriend.app.plist")
}

pub fn is_enabled() -> bool {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    return entry_path().exists();
    #[cfg(windows)]
    return reg(&["query", RUN_KEY, "/v", "RemoteFriend"]).is_ok();
    #[allow(unreachable_code)]
    false
}

pub fn set(enabled: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    {
        let path = entry_path();
        if !enabled {
            let _ = std::fs::remove_file(&path);
            return Ok(());
        }
        std::fs::create_dir_all(path.parent().expect("has parent")).map_err(|e| e.to_string())?;
        let body = format!(
            "[Desktop Entry]\nType=Application\nName=RemoteFriend\nExec=\"{}\"\nIcon=remotefriend\nX-GNOME-Autostart-enabled=true\n",
            exe.display()
        );
        return std::fs::write(&path, body).map_err(|e| e.to_string());
    }
    #[cfg(target_os = "macos")]
    {
        let path = entry_path();
        if !enabled {
            let _ = std::fs::remove_file(&path);
            return Ok(());
        }
        std::fs::create_dir_all(path.parent().expect("has parent")).map_err(|e| e.to_string())?;
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>io.remotefriend.app</string>
  <key>ProgramArguments</key><array><string>{}</string></array>
  <key>RunAtLoad</key><true/>
</dict></plist>
"#,
            exe.display()
        );
        return std::fs::write(&path, body).map_err(|e| e.to_string());
    }
    #[cfg(windows)]
    {
        if !enabled {
            let _ = reg(&["delete", RUN_KEY, "/v", "RemoteFriend", "/f"]);
            return Ok(());
        }
        let value = format!("\"{}\"", exe.display());
        return reg(&["add", RUN_KEY, "/v", "RemoteFriend", "/t", "REG_SZ", "/d", &value, "/f"]);
    }
    #[allow(unreachable_code)]
    {
        let _ = (exe, enabled);
        Err("not supported on this system".into())
    }
}

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(windows)]
fn reg(args: &[&str]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("reg")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}
