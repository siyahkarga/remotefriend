//! Shared protocol: all messages between host <-> client.
//! Encoded with bincode, sent length-prefixed over a TCP/TLS stream.

use serde::{Deserialize, Serialize};

pub mod identity;
pub mod io;
pub mod tls;
pub mod webapp;
// Note: `discovery` is defined inline in this file (below).

pub const PROTOCOL_VERSION: u32 = 3;
/// Browser video frame header version (must match webapp.html).
pub const WEB_FRAME_VERSION: u8 = 3;
/// Browser frame header length: [ver][flags][0][0][w u32][h u32][seq u32]
pub const WEB_FRAME_HEADER: usize = 16;
pub const WEB_FLAG_KEY: u8 = 1;
pub const WEB_FLAG_JPEG: u8 = 2;

/// Prepends the 16-byte header to a frame sent to the browser.
pub fn web_frame(w: u32, h: u32, seq: u64, key: bool, jpeg: bool, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(WEB_FRAME_HEADER + payload.len());
    let flags = if key { WEB_FLAG_KEY } else { 0 } | if jpeg { WEB_FLAG_JPEG } else { 0 };
    out.extend_from_slice(&[WEB_FRAME_VERSION, flags, 0, 0]);
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&(seq as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}
pub const DEFAULT_PORT: u16 = 33200;

/// First message when establishing a connection
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Handshake {
    pub version: u32,
    pub password: String, // visible in plaintext on the network without TLS/VPN; beware on the LAN native path
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
    /// encoder output (MVP: raw RGBA or H264 NAL)
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
    /// Scroll: number of lines, positive = down/right
    Scroll { dx: i32, dy: i32 },
    /// Full keyboard: separate press/release (including modifiers/F keys/arrows)
    Key { key: RemoteKey, down: bool },
    /// Type text (phone keyboard / paste). The host presses and releases each character.
    Text(String),
}

/// Host-independent key definition (client maps from egui, host maps to enigo/portal)
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// File transfer (phase 3, reserved ahead of time)
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
    /// Waiting for approval by the host operator
    WaitingForApproval,
    Video(VideoFrame),
    Input(InputEvent),
    File(FileChunk),
    /// The client decoded the frame with this sequence number (host uses it for latency/flow control).
    /// Older clients don't send it; in that case the host applies no window.
    Ack { seq: u64 },
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

// ---- LAN auto-discovery + transport generation ----

/// UDP discovery port (broadcast beacons)
pub const DISCOVERY_PORT: u16 = 33201;
/// Native TCP port (same as DEFAULT_PORT)
pub const NATIVE_PORT: u16 = DEFAULT_PORT;
#[deprecated(note = "QUIC is not used; use NATIVE_PORT")]
pub const QUIC_PORT: u16 = NATIVE_PORT;
/// Transport/protocol generation; used to tell incompatible old clients apart.
pub const TRANSPORT_GEN: u32 = 4;

/// Identity the host broadcasts on the LAN
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Beacon {
    pub v: u32,          // must be TRANSPORT_GEN
    pub name: String,    // computer name
    pub port: u16,       // native TCP port
    pub fp: String,      // TLS sertifika SHA256 fingerprint (TOFU)
    pub proto: u32,      // PROTOCOL_VERSION
}

/// Certificate DER -> short fingerprint (12 hex characters, shown in the UI)
pub fn fingerprint(cert_der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(cert_der);
    hex::encode(&h.finalize()[..6])
}

/// Certificate DER -> full fingerprint (connection verification)
pub fn fingerprint_full(cert_der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(cert_der);
    hex::encode(h.finalize())
}

// ---- v0.5.0: internet rendezvous (VPS relay) ----

/// Default rendezvous port (TCP+TLS)
pub const RENDEZVOUS_PORT: u16 = 33202;

/// Persistent host ID: a 9-digit code (displayed as "123 456 789").
/// Generated on first run and stored in ~/.config/remotefriend/host_id.
pub fn new_host_id() -> String {
    // The ID alone is not a secret, but it still should not be predictable.
    let n = rand::random::<u32>() % 1_000_000_000;
    format!("{n:09}")
}

/// 256-bit CSPRNG secret, lowercase hex.
pub fn new_secret_hex() -> String {
    let bytes = rand::random::<[u8; 32]>();
    hex::encode(bytes)
}

/// Unambiguous characters (no 0/o, 1/l/i): easy to read and type on a phone.
const PASSWORD_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// Generate a password for this run if none is set via environment: "abcde-23456".
/// 10 characters x 31 symbols ~= 49.5 bits; the host also rate-limits attempts.
pub fn new_session_password() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let mut out = String::with_capacity(11);
    for i in 0..10 {
        if i == 5 {
            out.push('-');
        }
        let idx = rng.random_range(0..PASSWORD_ALPHABET.len());
        out.push(PASSWORD_ALPHABET[idx] as char);
    }
    out
}

/// For comparing generated passwords: no spaces/dashes, lowercase.
pub fn normalize_password(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Constant-time byte comparison (no early return for equal lengths).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

pub fn format_id(id: &str) -> String {
    if id.len() == 9 {
        format!("{} {} {}", &id[0..3], &id[3..6], &id[6..9])
    } else {
        id.to_string()
    }
}

/// VPS rendezvous messages (inside the TLS tunnel, length-prefixed bincode).
/// IMPORTANT: TLS terminates on the VPS; whoever runs the relay can technically see the traffic.
/// True end-to-end encryption would require a separate Noise/PAKE layer.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RvMsg {
    // host -> server (persistent uplink)
    Register { id: String, name: String, secret: String },
    Heartbeat,
    ApprovalAnswer { client: String, allow: bool },
    ToClient { client: String, payload: Vec<u8> },
    /// New connection for an approved client (token must match)
    ConnectBack { token: u128, id: String, secret: String },
    // server -> host (over the uplink)
    RegisteredOk,
    RegisterError(String),
    ApprovalRequest { client: String, addr: String, kind: String, token: u128, auth: Option<String> },
    FromClient { client: String, payload: Vec<u8> },
    // client -> server (new connection)
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

/// UDP LAN discovery (run on separate threads, std blocking socket).
pub mod discovery {
    use super::Beacon;
    use std::net::UdpSocket;
    use std::time::Duration;

    /// Host: broadcast identity on the LAN (every 2 s). Never returns.
    pub fn broadcast_loop(name: String, port: u16) {
        let sock = match UdpSocket::bind("0.0.0.0:0") {
            Ok(s) => s,
            Err(e) => {
                eprintln!("discovery bind error: {e}");
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

    /// Client: listen for beacons and send each one found as (ip, beacon). Never returns.
    /// Note: if the port is already taken on this machine (a 2nd client), it can't listen; that's fine.
    pub fn listen_loop(tx: std::sync::mpsc::Sender<(String, Beacon)>) {
        let sock = match UdpSocket::bind(format!("0.0.0.0:{}", super::DISCOVERY_PORT)) {
            Ok(s) => s,
            Err(_) => return, // another client is listening; exit quietly
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
            other => panic!("unexpected packet: {other:?}"),
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
        assert_eq!(password.len(), 11);
        assert_eq!(password.as_bytes()[5], b'-');
        assert_eq!(normalize_password(&password).len(), 10);
        assert_eq!(normalize_password(" AbCdE-23456 "), "abcde23456");
    }

    #[test]
    fn web_frame_header_layout() {
        let f = web_frame(1920, 1080, 7, true, false, &[9, 9]);
        assert_eq!(f.len(), WEB_FRAME_HEADER + 2);
        assert_eq!(f[0], WEB_FRAME_VERSION);
        assert_eq!(f[1], WEB_FLAG_KEY);
        assert_eq!(u32::from_le_bytes(f[4..8].try_into().unwrap()), 1920);
        assert_eq!(u32::from_le_bytes(f[12..16].try_into().unwrap()), 7);
    }

    #[test]
    fn id_formatting_is_stable() {
        assert_eq!(format_id("123456789"), "123 456 789");
        assert_eq!(format_id("invalid"), "invalid");
    }
}
