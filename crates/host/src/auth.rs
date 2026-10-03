//! Parola doğrulama + kaba kuvvet sınırı (tüm yollar: LAN web, native, internet).
//!
//! - Kaynak (IP) başına: 60 sn içinde 5 hata → o kaynak 1 dk kilitli; tekrarında 2/4/8/16 dk.
//!   1 saat hatasız geçince kilit kademesi sıfırlanır. Böylece ID'yi bilen biri kendi IP'sini
//!   kilitler, sahibini değil.
//! - Genel üst sınır (dağıtık deneme): 10 dk içinde 30 hata → herkes 5 dk kilitli.
//! - Kilit süresince parola HİÇ değerlendirilmez (aksi halde kilit, denemeye açık bir kâhin olurdu).

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

struct Secret {
    value: String,
    /// Üretilmiş parola: tire/boşluk/büyük-küçük harf farkı yok sayılır.
    generated: bool,
}

static SECRET: OnceLock<Secret> = OnceLock::new();

#[derive(Default)]
struct Source {
    fails: VecDeque<Instant>,
    locked_until: Option<Instant>,
    locks: u32,
    last_fail: Option<Instant>,
}

struct Limiter {
    sources: HashMap<String, Source>,
    global_fails: VecDeque<Instant>,
    global_until: Option<Instant>,
}

static LIMITER: OnceLock<Mutex<Limiter>> = OnceLock::new();

fn limiter() -> &'static Mutex<Limiter> {
    LIMITER.get_or_init(|| Mutex::new(Limiter { sources: HashMap::new(), global_fails: VecDeque::new(), global_until: None }))
}

const WINDOW: Duration = Duration::from_secs(60);
const MAX_FAILS: usize = 5;
const DECAY: Duration = Duration::from_secs(3600);
const GLOBAL_WINDOW: Duration = Duration::from_secs(600);
const GLOBAL_MAX_FAILS: usize = 30;
const GLOBAL_LOCK: Duration = Duration::from_secs(300);

pub(crate) enum Auth {
    Ok,
    Bad,
    /// Kalan kilit süresi (sn).
    Locked(u64),
}

/// Parolayı bir kez ayarla. Döner: (gösterilecek parola, ortamdan mı geldi).
pub(crate) fn init() -> (String, bool) {
    let configured = std::env::var("REMOTE_FRIEND_PASS").ok().filter(|s| !s.trim().is_empty());
    let secret = match &configured {
        Some(p) => Secret { value: p.trim().to_string(), generated: false },
        None => Secret { value: remote_friend_common::new_session_password(), generated: true },
    };
    if !secret.generated && secret.value.chars().count() < 10 {
        tracing::warn!("zayıf REMOTE_FRIEND_PASS: en az 10 karakter kullan");
    }
    let shown = secret.value.clone();
    let _ = SECRET.set(secret);
    (shown, configured.is_some())
}

fn matches(candidate: &str) -> bool {
    let Some(secret) = SECRET.get() else { return false };
    if secret.generated {
        let a = remote_friend_common::normalize_password(candidate);
        let b = remote_friend_common::normalize_password(&secret.value);
        remote_friend_common::constant_time_eq(a.as_bytes(), b.as_bytes())
    } else {
        remote_friend_common::constant_time_eq(candidate.trim().as_bytes(), secret.value.as_bytes())
    }
}

/// Kaynak anahtarı: "1.2.3.4:5678" -> "1.2.3.4"; diğer metinler olduğu gibi.
pub(crate) fn source_key(peer: &str) -> String {
    peer.parse::<std::net::SocketAddr>()
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| peer.chars().take(80).collect())
}

pub(crate) fn check_password(candidate: &str, source: &str) -> Auth {
    let now = Instant::now();
    let key = source_key(source);
    let mut lim = limiter().lock().unwrap_or_else(|e| e.into_inner());

    if let Some(until) = lim.global_until {
        if until > now {
            return Auth::Locked((until - now).as_secs().max(1));
        }
        lim.global_until = None;
    }
    {
        let src = lim.sources.entry(key.clone()).or_default();
        if src.last_fail.is_some_and(|t| now.duration_since(t) > DECAY) {
            *src = Source::default();
        }
        if let Some(until) = src.locked_until {
            if until > now {
                return Auth::Locked((until - now).as_secs().max(1));
            }
            src.locked_until = None;
        }
    }

    if matches(candidate) {
        if let Some(src) = lim.sources.get_mut(&key) {
            src.fails.clear();
        }
        return Auth::Ok;
    }

    // Hatalı deneme: kaynak + genel sayaç.
    let src = lim.sources.entry(key.clone()).or_default();
    while src.fails.front().is_some_and(|t| now.duration_since(*t) > WINDOW) {
        src.fails.pop_front();
    }
    src.fails.push_back(now);
    src.last_fail = Some(now);
    if src.fails.len() >= MAX_FAILS {
        let mins = 1u64 << src.locks.min(4);
        src.locks += 1;
        src.fails.clear();
        src.locked_until = Some(now + Duration::from_secs(60 * mins));
        tracing::warn!("{key}: çok fazla hatalı parola; {mins} dk kilit");
        println!("!!! {key}: çok fazla hatalı parola denemesi; bu kaynak {mins} dakika kilitlendi.");
    }
    while lim.global_fails.front().is_some_and(|t| now.duration_since(*t) > GLOBAL_WINDOW) {
        lim.global_fails.pop_front();
    }
    lim.global_fails.push_back(now);
    if lim.global_fails.len() >= GLOBAL_MAX_FAILS {
        lim.global_fails.clear();
        lim.global_until = Some(now + GLOBAL_LOCK);
        println!("!!! Çok sayıda kaynaktan hatalı parola: tüm bağlantılar 5 dakika kilitlendi.");
    }
    // Eski kayıtları temizle (bellek sınırı).
    if lim.sources.len() > 1024 {
        lim.sources.retain(|_, s| s.locked_until.is_some_and(|u| u > now) || s.last_fail.is_some_and(|t| now.duration_since(t) < WINDOW));
    }
    Auth::Bad
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_source_lockout_does_not_lock_others() {
        let _ = SECRET.set(Secret { value: "abcde-23456".into(), generated: true });
        for _ in 0..MAX_FAILS {
            assert!(matches!(check_password("yanlis", "10.0.0.1:1000"), Auth::Bad));
        }
        // Saldırganın kaynağı kilitli: doğru parola bile değerlendirilmez.
        assert!(matches!(check_password("abcde-23456", "10.0.0.1:2000"), Auth::Locked(_)));
        // Başka kaynak etkilenmez; üretilmiş parolada biçim farkı önemsiz.
        assert!(matches!(check_password(" ABCDE 23456 ", "10.0.0.2:1000"), Auth::Ok));
    }

    #[test]
    fn source_key_strips_port() {
        assert_eq!(source_key("1.2.3.4:5678"), "1.2.3.4");
        assert_eq!(source_key("[::1]:80"), "::1");
        assert_eq!(source_key("tarayıcı 1.2.3.4"), "tarayıcı 1.2.3.4");
    }
}
