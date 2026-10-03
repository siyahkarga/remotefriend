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
    let _guard = ASK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let rx = lines().lock().unwrap_or_else(|e| e.into_inner());
    while rx.try_recv().is_ok() {} // önceden yazılmış eski satırları at
    println!("\x07{question}");
    match rx.recv_timeout(timeout) {
        Ok(line) => yes(&line),
        Err(RecvTimeoutError::Timeout) => {
            println!("*** zaman aşımı: HAYIR sayıldı");
            false
        }
        Err(RecvTimeoutError::Disconnected) => {
            println!("*** terminal girişi yok: onay verilemedi (gözetimsiz kullanım için REMOTE_FRIEND_AUTO_ACCEPT=1)");
            false
        }
    }
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

/// Bağlantı isteğini operatöre sor (parola zaten doğrulandı). true = kabul.
pub(crate) fn ask(peer: &str) -> bool {
    if std::env::var("REMOTE_FRIEND_AUTO_ACCEPT").map(|v| v == "1").unwrap_or(false) {
        println!("*** {peer}: otomatik kabul (REMOTE_FRIEND_AUTO_ACCEPT=1)");
        return true;
    }
    notify_desktop(peer);
    let ok = prompt(
        &format!("*** Bağlantı isteği: {peer}\n*** Kabul ediyor musun? E = evet / H = hayır (30 sn, varsayılan HAYIR)"),
        Duration::from_secs(30),
    );
    println!("*** {peer}: {}", if ok { "KABUL" } else { "RET" });
    ok
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

/// Röle kimlik verisini (parola + isteğe bağlı belirteç) ayır.
pub(crate) fn split_relay_auth(auth: &str) -> (&str, Option<&str>) {
    if let Some(rest) = auth.strip_prefix(RESUME_PREFIX) {
        if let Some((token, password)) = rest.split_once(':') {
            if token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
                return (password, Some(token));
            }
        }
    }
    (auth, None)
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
        assert_eq!(split_relay_auth(&format!("rf-resume:{tok}:gizli:sifre")), ("gizli:sifre", Some(tok)));
        assert_eq!(split_relay_auth("normal-sifre"), ("normal-sifre", None));
        assert_eq!(split_relay_auth("rf-resume:kisa:x"), ("rf-resume:kisa:x", None));
    }
}
