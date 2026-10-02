//! Ortak protokol: host <-> client arası tüm mesajlar.
//! Bincode ile encode, TCP/TLS akışında length-prefixed gönderim.

use serde::{Deserialize, Serialize};

pub mod identity;
pub mod io;
pub mod tls;
pub mod webapp;
// Not: `discovery` bu dosyada inline tanımlı (aşağıda).

pub const PROTOCOL_VERSION: u32 = 2;
pub const DEFAULT_PORT: u16 = 33200;

/// Bağlantı kurarken ilk mesaj
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Handshake {
    pub version: u32,
    pub password: String, // TLS/VPN dışında ağda düz görünür; LAN native yolunda dikkat
    pub want_video: bool,
    pub want_input: bool,
}

/// Host -> Client: video
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VideoFrame {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub codec: VideoCodec,
    /// encoder çıktısı (MVP: raw RGBA veya H264 NAL)
    pub data: Vec<u8>,
    pub timestamp_ms: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    RawRgba,
    H264,
    Jpeg,
}

/// Client -> Host: input
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum InputEvent {
    MouseMove { x: u32, y: u32 },
    MouseDown { button: MouseButton },
    MouseUp { button: MouseButton },
    /// Kaydırma: satır sayısı, pozitif = aşağı/sağa
    Scroll { dx: i32, dy: i32 },
    /// Tam klavye: basma/bırakma ayrı (modifier/F-tuş/oklar dahil)
    Key { key: RemoteKey, down: bool },
}

/// Host'tan bağımsız tuş tanımı (client egui'den, host enigo'ya çevirir)
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteKey {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    Shift,
    Ctrl,
    Alt,
    Meta,
    CapsLock,
    NumLock,
    PrintScreen,
    Pause,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Dosya transferi (faz 3, şimdiden rezerve)
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileChunk {
    pub transfer_id: u64,
    pub name: String,
    pub offset: u64,
    pub total: u64,
    pub data: Vec<u8>,
    pub last: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Packet {
    Handshake(Handshake),
    Accept,
    Reject(String),
    /// Host operatör onayı bekleniyor (terminalde E/H sorulur)
    WaitingForApproval,
    Video(VideoFrame),
    Input(InputEvent),
    File(FileChunk),
}

pub fn encode(packet: &Packet) -> anyhow::Result<Vec<u8>> {
    use bincode::Options as _;
    Ok(bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .serialize(packet)?)
}

pub fn decode(buf: &[u8]) -> anyhow::Result<Packet> {
    use bincode::Options as _;
    Ok(bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(24_000_000)
        .reject_trailing_bytes()
        .deserialize(buf)?)
}

// ---- LAN otomatik bulma + transport nesli ----

/// UDP discovery portu (broadcast beacon'lar)
pub const DISCOVERY_PORT: u16 = 33201;
/// Native TCP portu (DEFAULT_PORT ile aynı)
pub const NATIVE_PORT: u16 = DEFAULT_PORT;
#[deprecated(note = "QUIC kullanılmıyor; NATIVE_PORT kullan")]
pub const QUIC_PORT: u16 = NATIVE_PORT;
/// Transport/protokol nesli; uyumsuz eski istemcileri ayırmak için.
pub const TRANSPORT_GEN: u32 = 4;

/// Host'un LAN'a yayınladığı kimlik
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Beacon {
    pub v: u32,          // TRANSPORT_GEN olmalı
    pub name: String,    // bilgisayar adı
    pub port: u16,       // native TCP portu
    pub fp: String,      // TLS sertifika SHA256 fingerprint (TOFU)
    pub proto: u32,      // PROTOCOL_VERSION
}

/// Sertifika DER -> kısa fingerprint (12 hex karakter, UI'da gösterilir)
pub fn fingerprint(cert_der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(cert_der);
    hex::encode(&h.finalize()[..6])
}

/// Sertifika DER -> tam fingerprint (bağlantı doğrulama)
pub fn fingerprint_full(cert_der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(cert_der);
    hex::encode(h.finalize())
}

// ---- v0.5.0: internet rendezvous (VPS relay) ----

/// Rendezvous varsayılan portu (TCP+TLS)
pub const RENDEZVOUS_PORT: u16 = 33202;

/// Kalıcı host kimliği: 9 haneli kod ("123 456 789" diye gösterilir).
/// İlk çalışmada üretilir, ~/.config/remotefriend/host_id dosyasında saklanır.
pub fn new_host_id() -> String {
    // ID tek başına bir kimlik sırrı değildir; yine de öngörülebilir olmaması gerekir.
    let n = rand::random::<u32>() % 1_000_000_000;
    format!("{n:09}")
}

/// 256-bit CSPRNG sır, küçük harf hex.
pub fn new_secret_hex() -> String {
    let bytes = rand::random::<[u8; 32]>();
    hex::encode(bytes)
}

/// Ortam değişkeni verilmediyse bu çalıştırma için güçlü bir parola üret.
pub fn new_session_password() -> String {
    let bytes = rand::random::<[u8; 12]>();
    hex::encode(bytes)
}

pub fn format_id(id: &str) -> String {
    if id.len() == 9 {
        format!("{} {} {}", &id[0..3], &id[3..6], &id[6..9])
    } else {
        id.to_string()
    }
}

/// VPS rendezvous mesajları (TLS tünel içinde, length-prefixed bincode).
/// ÖNEMLİ: TLS VPS'te sonlanır; relay işleten kişi trafiği teknik olarak görebilir.
/// Gerçek uçtan uca şifreleme ayrı bir Noise/PAKE katmanı gerektirir.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RvMsg {
    // host -> server (kalıcı uplink)
    Register { id: String, name: String, secret: String },
    Heartbeat,
    ApprovalAnswer { client: String, allow: bool },
    ToClient { client: String, payload: Vec<u8> },
    /// Onaylanan client için yeni bağlantı (token eşleşmeli)
    ConnectBack { token: u128, id: String, secret: String },
    // server -> host (uplink üzerinden)
    RegisteredOk,
    RegisterError(String),
    ApprovalRequest { client: String, addr: String, kind: String, token: u128, auth: Option<String> },
    FromClient { client: String, payload: Vec<u8> },
    // client -> server (yeni bağlantı)
    Hello { id: String, auth: Option<String> },
    ToHost { payload: Vec<u8> },
    // server -> client
    WaitApproval,
    Rejected(String),
    Accepted,
    FromHost { payload: Vec<u8> },
}

pub fn rv_encode(m: &RvMsg) -> anyhow::Result<Vec<u8>> {
    use bincode::Options as _;
    Ok(bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .serialize(m)?)
}

pub fn rv_decode(buf: &[u8]) -> anyhow::Result<RvMsg> {
    use bincode::Options as _;
    Ok(bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(1_000_000)
        .reject_trailing_bytes()
        .deserialize(buf)?)
}

/// UDP LAN discovery (ayrı thread'lerde çalıştır, std blocking socket).
pub mod discovery {
    use super::Beacon;
    use std::net::UdpSocket;
    use std::time::Duration;

    /// Host: kimliğini LAN'a yayınla (2 sn'de bir). Dönmez.
    pub fn broadcast_loop(name: String, port: u16) {
        let sock = match UdpSocket::bind("0.0.0.0:0") {
            Ok(s) => s,
            Err(e) => {
                eprintln!("discovery bind hatası: {e}");
                return;
            }
        };
        let _ = sock.set_broadcast(true);
        let beacon = Beacon {
            v: super::TRANSPORT_GEN,
            name,
            port,
            fp: String::new(),
            proto: super::PROTOCOL_VERSION,
        };
        let msg = serde_json::to_vec(&beacon).unwrap_or_default();
        let target = format!("255.255.255.255:{}", super::DISCOVERY_PORT);
        loop {
            let _ = sock.send_to(&msg, &target);
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    /// Client: beacon dinle, bulunanları (ip, beacon) olarak gönder. Dönmez.
    /// Not: port makinede doluysa (2. client) dinleyemez, sorun değil.
    pub fn listen_loop(tx: std::sync::mpsc::Sender<(String, Beacon)>) {
        let sock = match UdpSocket::bind(format!("0.0.0.0:{}", super::DISCOVERY_PORT)) {
            Ok(s) => s,
            Err(_) => return, // başka client dinliyor, sessiz çık
        };
        let _ = sock.set_read_timeout(Some(Duration::from_secs(1)));
        let mut buf = [0u8; 2048];
        loop {
            match sock.recv_from(&mut buf) {
                Ok((n, addr)) => {
                    if let Ok(b) = serde_json::from_slice::<Beacon>(&buf[..n]) {
                        if b.v == super::TRANSPORT_GEN {
                            let _ = tx.send((addr.ip().to_string(), b));
                        }
                    }
                }
                Err(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_round_trip_and_trailing_bytes_rejected() {
        let packet = Packet::Handshake(Handshake {
            version: PROTOCOL_VERSION,
            password: "test-password".into(),
            want_video: true,
            want_input: true,
        });
        let encoded = encode(&packet).expect("encode");
        let decoded = decode(&encoded).expect("decode");
        match decoded {
            Packet::Handshake(h) => {
                assert_eq!(h.version, PROTOCOL_VERSION);
                assert_eq!(h.password, "test-password");
            }
            other => panic!("beklenmeyen paket: {other:?}"),
        }

        let mut with_trailing = encoded;
        with_trailing.push(0);
        assert!(decode(&with_trailing).is_err());
    }

    #[test]
    fn generated_identity_material_has_expected_shape() {
        let id = new_host_id();
        assert_eq!(id.len(), 9);
        assert!(id.bytes().all(|b| b.is_ascii_digit()));

        let secret = new_secret_hex();
        assert_eq!(secret.len(), 64);
        assert!(secret.bytes().all(|b| b.is_ascii_hexdigit()));

        let password = new_session_password();
        assert_eq!(password.len(), 24);
        assert!(password.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn id_formatting_is_stable() {
        assert_eq!(format_id("123456789"), "123 456 789");
        assert_eq!(format_id("invalid"), "invalid");
    }
}
