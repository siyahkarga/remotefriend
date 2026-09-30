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
