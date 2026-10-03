//! Operatör onayı (terminalde E/H). Tek stdin okuyucu: zaman aşımına uğrayan bir
//! sorunun bekleyen okuma thread'i sonraki cevabı "çalamaz".

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

static LINES: OnceLock<Mutex<Receiver<String>>> = OnceLock::new();
static ASK_LOCK: Mutex<()> = Mutex::new(());

fn lines() -> &'static Mutex<Receiver<String>> {
    LINES.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("rf-stdin".into())
            .spawn(move || {
                let stdin = std::io::stdin();
                let mut line = String::new();
                loop {
                    line.clear();
                    match stdin.read_line(&mut line) {
                        Ok(0) | Err(_) => break, // stdin yok (servis olarak çalışıyor)
                        Ok(_) => {
                            if tx.send(line.trim().to_string()).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .expect("stdin thread başlatılamadı");
        Mutex::new(rx)
    })
}

fn yes(s: &str) -> bool {
    matches!(s.to_lowercase().as_str(), "e" | "evet" | "y" | "yes")
}

/// Terminalde evet/hayır sor. Zaman aşımında ya da stdin yoksa HAYIR.
pub(crate) fn prompt(question: &str, timeout: Duration) -> bool {
    prompt_line(question, timeout).is_some_and(|l| yes(&l))
}

fn notify_desktop(peer: &str) {
    // Mesaj komut satırına/AppleScript'e gider: tırnak ve ters bölü temizlenir.
    let peer: String = peer.chars().filter(|c| !matches!(c, '"' | '\\' | '\'' | '`' | '$')).collect();
    let msg = format!("{peer} bağlanmak istiyor. Onay için host terminalinde E tuşuna bas.");
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("notify-send")
        .args(["-u", "critical", "-a", "RemoteFriend", "RemoteFriend: bağlantı isteği", &msg])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("osascript")
        .args(["-e", &format!("display notification \"{msg}\" with title \"RemoteFriend\"")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = msg;
}

/// Operatörün kararı.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Deny,
    /// Yalnızca bu bağlantı.
    Once,
    /// Bu cihaz bundan sonra onaysız bağlanabilir (parola yine gerekir).
    Always,
}

/// Bağlantı isteğini operatöre sor (parola zaten doğrulandı).
/// `can_remember`: cihaz kalıcı izin belirtecini saklayabiliyorsa (tarayıcı) "K" seçeneği sunulur.
pub(crate) fn ask(peer: &str, can_remember: bool) -> Decision {
    if std::env::var("REMOTE_FRIEND_AUTO_ACCEPT").map(|v| v == "1").unwrap_or(false) {
        println!("*** {peer}: otomatik kabul (REMOTE_FRIEND_AUTO_ACCEPT=1)");
        return Decision::Once;
    }
    notify_desktop(peer);
    let options = if can_remember {
        "E = bu sefer / K = KALICI (bu cihaz bir daha sormadan bağlanır) / H = hayır"
    } else {
        "E = evet / H = hayır"
    };
    let answer = prompt_line(
        &format!("*** Bağlantı isteği: {peer}\n*** {options}  (30 sn, varsayılan HAYIR)"),
        Duration::from_secs(30),
    );
    let decision = match answer.as_deref().map(|s| s.to_lowercase()) {
        Some(a) if can_remember && matches!(a.as_str(), "k" | "kalıcı" | "kalici" | "a" | "always") => Decision::Always,
        Some(a) if yes(&a) || matches!(a.as_str(), "k" | "kalıcı" | "kalici") => Decision::Once,
        _ => Decision::Deny,
    };
    println!(
        "*** {peer}: {}",
        match decision {
            Decision::Deny => "RET",
            Decision::Once => "KABUL (bu sefer)",
            Decision::Always => "KABUL (kalıcı: bu cihaz bir daha sormadan bağlanır)",
        }
    );
    decision
}

/// Satırı döndüren soru (zaman aşımı / stdin yok -> None).
fn prompt_line(question: &str, timeout: Duration) -> Option<String> {
    let _guard = ASK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let rx = lines().lock().unwrap_or_else(|e| e.into_inner());
    while rx.try_recv().is_ok() {}
    println!("\x07{question}");
    match rx.recv_timeout(timeout) {
        Ok(line) => Some(line),
        Err(RecvTimeoutError::Timeout) => {
            println!("*** zaman aşımı: HAYIR sayıldı");
            None
        }
        Err(RecvTimeoutError::Disconnected) => {
            println!("*** terminal girişi yok: onay verilemedi (güvenilir cihaz ya da REMOTE_FRIEND_AUTO_ACCEPT=1)");
            None
        }
    }
}

// ---- güvenilir cihazlar ----
//
// Operatör bir tarayıcıyı "K" ile kalıcı onaylarsa cihaza 256 bit belirteç verilir.
// Host yalnızca belirtecin SHA-256 özetini saklar (~/.config/remotefriend/trusted_devices.json, 0600).
// Belirteç parolanın yerine geçmez; yalnızca terminal onayını atlar.

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Device {
    hash: String,
    label: String,
    added: u64,
}

static DEVICES_LOCK: Mutex<()> = Mutex::new(());

fn devices_path() -> std::path::PathBuf {
    remote_friend_common::identity::config_dir().join("trusted_devices.json")
}

fn load_devices() -> Vec<Device> {
    std::fs::read(devices_path())
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default()
}

fn save_devices(list: &[Device]) {
    let data = serde_json::to_vec_pretty(list).unwrap_or_default();
    if let Err(e) = remote_friend_common::identity::write_private(&devices_path(), &data) {
        tracing::warn!("güvenilir cihaz listesi yazılamadı: {e}");
    }
}

/// Yeni güvenilir cihaz ekle, istemciye verilecek belirteci döndür.
pub(crate) fn trust_device(label: &str) -> String {
    let token = remote_friend_common::new_secret_hex();
    let _g = DEVICES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = load_devices();
    if list.len() >= 32 {
        list.remove(0);
    }
    list.push(Device {
        hash: remote_friend_common::fingerprint_full(token.as_bytes()),
        label: label.chars().take(80).collect(),
        added: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    });
    save_devices(&list);
    token
}

/// Belirteç güvenilir bir cihaza mı ait?
pub(crate) fn is_trusted(token: &str) -> bool {
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let hash = remote_friend_common::fingerprint_full(token.to_ascii_lowercase().as_bytes());
    let _g = DEVICES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load_devices()
        .iter()
        .fold(false, |found, d| found | remote_friend_common::constant_time_eq(d.hash.as_bytes(), hash.as_bytes()))
}

pub(crate) fn trusted_count() -> usize {
    load_devices().len()
}

/// Tüm güvenilir cihazları unut. Silinen sayıyı döndürür.
pub(crate) fn forget_devices() -> usize {
    let _g = DEVICES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let n = load_devices().len();
    let _ = std::fs::remove_file(devices_path());
    n
}

// ---- yeniden bağlanma belirteci ----
//
// Onaylanmış bir oturum kısa süre kopunca (telefon ağ değiştirdi, sekme arka plana gitti)
// host başında birinin tekrar E'ye basması gerekmesin. Belirteç tek kullanımlıktır,
// 10 dk geçerlidir ve parolayla BİRLİKTE gerekir (parolanın yerine geçmez).

static RESUME: Mutex<Vec<(String, std::time::Instant)>> = Mutex::new(Vec::new());
const RESUME_TTL: Duration = Duration::from_secs(600);
/// Röle yolunda belirteç parolanın önüne eklenir: "rf-resume:<32 hex>:<parola>".
const RESUME_PREFIX: &str = "rf-resume:";
const DEVICE_PREFIX: &str = "rf-dev:";

pub(crate) fn grant_resume() -> String {
    let token = remote_friend_common::new_secret_hex()[..32].to_string();
    let mut list = RESUME.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|(_, t)| t.elapsed() < RESUME_TTL);
    if list.len() >= 16 {
        list.remove(0);
    }
    list.push((token.clone(), std::time::Instant::now()));
    token
}

/// Geçerliyse belirteci tüketir ve true döner.
pub(crate) fn consume_resume(token: &str) -> bool {
    if token.len() != 32 {
        return false;
    }
    let mut list = RESUME.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|(_, t)| t.elapsed() < RESUME_TTL);
    let pos = list
        .iter()
        .position(|(t, _)| remote_friend_common::constant_time_eq(t.as_bytes(), token.as_bytes()));
    match pos {
        Some(i) => {
            list.remove(i);
            true
        }
        None => false,
    }
}

/// Röle kimlik verisi: [rf-resume:<32hex>:][rf-dev:<64hex>:]<parola>
pub(crate) struct RelayAuth<'a> {
    pub password: &'a str,
    pub resume: Option<&'a str>,
    pub device: Option<&'a str>,
}

pub(crate) fn split_relay_auth(auth: &str) -> RelayAuth<'_> {
    let mut out = RelayAuth { password: auth, resume: None, device: None };
    loop {
        let rest = out.password;
        let hex_ok = |t: &str, n: usize| t.len() == n && t.bytes().all(|b| b.is_ascii_hexdigit());
        if let Some(r) = rest.strip_prefix(RESUME_PREFIX) {
            if let Some((t, p)) = r.split_once(':') {
                if hex_ok(t, 32) && out.resume.is_none() {
                    out.resume = Some(t);
                    out.password = p;
                    continue;
                }
            }
        }
        if let Some(r) = rest.strip_prefix(DEVICE_PREFIX) {
            if let Some((t, p)) = r.split_once(':') {
                if hex_ok(t, 64) && out.device.is_none() {
                    out.device = Some(t);
                    out.password = p;
                    continue;
                }
            }
        }
        return out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_tokens_are_single_use() {
        let t = grant_resume();
        assert!(consume_resume(&t));
        assert!(!consume_resume(&t));
        assert!(!consume_resume("00000000000000000000000000000000"));
    }

    #[test]
    fn relay_auth_split() {
        let tok = "0123456789abcdef0123456789abcdef";
        let dev = "ab".repeat(32);
        let raw = format!("rf-resume:{tok}:rf-dev:{dev}:gizli:sifre");
        let a = split_relay_auth(&raw);
        assert_eq!((a.password, a.resume, a.device), ("gizli:sifre", Some(tok), Some(dev.as_str())));
        let b = split_relay_auth("normal-sifre");
        assert_eq!((b.password, b.resume, b.device), ("normal-sifre", None, None));
        let c = split_relay_auth("rf-resume:kisa:x");
        assert_eq!((c.password, c.resume), ("rf-resume:kisa:x", None));
    }
}
