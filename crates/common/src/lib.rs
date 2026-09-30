//! Ortak protokol: host <-> client arası tüm mesajlar.
//! MVP: bincode ile encode, QUIC stream üzerinden length-prefixed gönderim.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_PORT: u16 = 33200;

/// Bağlantı kurarken ilk mesaj
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Handshake {
    pub version: u32,
    pub password: String, // MVP: düz parola, sonra PAKE/Noise
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
    Ok(bincode::serialize(packet)?)
}

pub fn decode(buf: &[u8]) -> anyhow::Result<Packet> {
    Ok(bincode::deserialize(buf)?)
}

// ---- v0.3.0: şifreli QUIC + LAN otomatik bulma ----

/// UDP discovery portu (broadcast beacon'lar)
pub const DISCOVERY_PORT: u16 = 33201;
/// QUIC portu (DEFAULT_PORT ile aynı)
pub const QUIC_PORT: u16 = DEFAULT_PORT;
/// Protokol nesli: v0.3.0 QUIC'e geçti, eski TCP clientlar reddedilir
pub const TRANSPORT_GEN: u32 = 3;

/// Host'un LAN'a yayınladığı kimlik
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Beacon {
    pub v: u32,          // TRANSPORT_GEN olmalı
    pub name: String,    // bilgisayar adı
    pub port: u16,       // QUIC portu
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
