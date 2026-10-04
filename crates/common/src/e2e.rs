//! End-to-end encryption between a viewer (browser or app) and the shared computer.
//!
//! The relay server only forwards ciphertext and never sees the password.
//!
//! Handshake (both sides prove they know the password; a relay without it cannot
//! read or inject anything, and can only guess the password offline at PBKDF2 cost):
//!
//! 1. host -> viewer: salt (16 B), host ECDH P-256 public key, PBKDF2 iterations,
//!    and whether the password is compared normalized (generated passwords).
//! 2. viewer -> host: viewer public key, MAC_c
//! 3. host -> viewer: MAC_h (or a refusal)
//!
//! ```text
//! pw_key     = PBKDF2-HMAC-SHA256(password, salt, iterations, 32)
//! shared     = ECDH(viewer, host)                      (x coordinate, 32 B)
//! transcript = "rf-e2e-v1" | salt | host_pub | viewer_pub
//! okm        = HKDF-SHA256(salt = pw_key, ikm = shared, info = "rf-e2e-v1 keys" | SHA-256(transcript), 128 B)
//!            = mac_c_key | mac_h_key | key_viewer_to_host | key_host_to_viewer
//! MAC_c      = HMAC-SHA256(mac_c_key, "client" | transcript)
//! MAC_h      = HMAC-SHA256(mac_h_key, "host" | transcript)
//! ```
//!
//! Messages are then AES-256-GCM with a 96-bit nonce = 4 zero bytes | u64 counter
//! (big endian), one counter per direction. A direct (peer-to-peer) path gets its own two
//! keys, `HKDF(..., info = "rf-e2e-v1 direct" | SHA-256(transcript), 64 B)`, so the two
//! paths keep independent counters. The browser implements the same with
//! WebCrypto (`webapp.html`).
//!
//! Trusted devices ("Always allow") log in without the computer's password, which changes
//! after every session: the device holds a random 256-bit token and the computer keeps
//! `pw_key = PBKDF2(token, device salt)` for it. The viewer names its device (`device_id`)
//! instead of answering step 1; the computer then sends a second step 1 with that device's
//! salt and the same handshake runs with the stored key in place of the password.

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use sha2::{Digest, Sha256};

pub const VERSION: u8 = 2;
pub const ITERATIONS: u32 = 300_000;
/// Device tokens are 256-bit random values: key stretching adds nothing, keep connects fast.
pub const DEVICE_ITERATIONS: u32 = 10_000;
const DEVICE_MAGIC: &[u8; 4] = b"RFDV";
/// Hello flags: the computer accepts trusted-device logins / this hello is for a device.
pub const FLAG_DEVICE_LOGIN: u8 = 1;
pub const FLAG_DEVICE_MODE: u8 = 2;
/// Step-3 error code: the named device is not trusted (any more).
pub const ERR_UNKNOWN_DEVICE: u8 = 3;
const MAGIC: &[u8; 4] = b"RFE2";
const LABEL: &[u8] = b"rf-e2e-v1";
pub const PUB_LEN: usize = 65;
pub const SALT_LEN: usize = 16;
pub const MAC_LEN: usize = 32;
/// Marker sent through the relay instead of a password.
pub const RELAY_AUTH_MARKER: &str = "rf-e2e";

type HmacSha256 = Hmac<Sha256>;

/// One direction of an encrypted channel.
pub struct Cipher {
    aead: Aes256Gcm,
    counter: u64,
}

impl Cipher {
    pub fn new(key: &[u8]) -> Self {
        Self { aead: Aes256Gcm::new_from_slice(key).expect("32-byte key"), counter: 0 }
    }

    fn nonce(&mut self) -> Result<[u8; 12]> {
        if self.counter == u64::MAX {
            bail!("nonce counter exhausted");
        }
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&self.counter.to_be_bytes());
        self.counter += 1;
        Ok(n)
    }

    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let n = self.nonce()?;
        self.aead
            .encrypt(Nonce::from_slice(&n), plaintext)
            .map_err(|_| anyhow::anyhow!("encryption failed"))
    }

    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let n = self.nonce()?;
        self.aead
            .decrypt(Nonce::from_slice(&n), ciphertext)
            .map_err(|_| anyhow::anyhow!("decryption failed (tampered or out of order)"))
    }
}

/// Both directions after a successful handshake.
pub struct Channel {
    pub tx: Cipher,
    pub rx: Cipher,
    /// Separate keys for a direct (peer-to-peer) path.
    pub direct: DirectPair,
}

/// Ciphers for the direct path (send, receive).
pub struct DirectPair {
    pub tx: Cipher,
    pub rx: Cipher,
}

fn random_secret() -> SecretKey {
    loop {
        let bytes: [u8; 32] = rand::random();
        if let Ok(k) = SecretKey::from_slice(&bytes) {
            return k;
        }
    }
}

fn pub_bytes(k: &SecretKey) -> [u8; PUB_LEN] {
    let ep = k.public_key().to_encoded_point(false);
    let mut out = [0u8; PUB_LEN];
    out.copy_from_slice(ep.as_bytes());
    out
}

fn ecdh(secret: &SecretKey, peer_pub: &[u8]) -> Result<[u8; 32]> {
    let peer = PublicKey::from_sec1_bytes(peer_pub).context("invalid public key")?;
    let shared = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), peer.as_affine());
    let mut out = [0u8; 32];
    out.copy_from_slice(shared.raw_secret_bytes());
    Ok(out)
}

/// Normalize like the host does for generated passwords (case, dashes, spaces ignored).
pub fn prepare_password(password: &str, normalize: bool) -> String {
    if normalize {
        crate::normalize_password(password)
    } else {
        password.trim().to_string()
    }
}

pub fn password_key(password: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, iterations, &mut out);
    out
}

struct Derived {
    mac_c: [u8; 32],
    mac_h: [u8; 32],
    k_c2h: [u8; 32],
    k_h2c: [u8; 32],
    d_c2h: [u8; 32],
    d_h2c: [u8; 32],
    transcript: Vec<u8>,
}

fn derive(pw_key: &[u8; 32], shared: &[u8; 32], salt: &[u8], host_pub: &[u8], client_pub: &[u8]) -> Derived {
    let mut transcript = Vec::with_capacity(LABEL.len() + salt.len() + 2 * PUB_LEN);
    transcript.extend_from_slice(LABEL);
    transcript.extend_from_slice(salt);
    transcript.extend_from_slice(host_pub);
    transcript.extend_from_slice(client_pub);
    let mut info = b"rf-e2e-v1 keys".to_vec();
    info.extend_from_slice(&Sha256::digest(&transcript));
    let hk = hkdf::Hkdf::<Sha256>::new(Some(pw_key), shared);
    let mut okm = [0u8; 128];
    hk.expand(&info, &mut okm).expect("valid HKDF length");
    let mut direct_info = b"rf-e2e-v1 direct".to_vec();
    direct_info.extend_from_slice(&Sha256::digest(&transcript));
    let mut dkm = [0u8; 64];
    hk.expand(&direct_info, &mut dkm).expect("valid HKDF length");
    let mut d = Derived {
        mac_c: [0; 32],
        mac_h: [0; 32],
        k_c2h: [0; 32],
        k_h2c: [0; 32],
        d_c2h: [0; 32],
        d_h2c: [0; 32],
        transcript,
    };
    d.mac_c.copy_from_slice(&okm[0..32]);
    d.mac_h.copy_from_slice(&okm[32..64]);
    d.k_c2h.copy_from_slice(&okm[64..96]);
    d.k_h2c.copy_from_slice(&okm[96..128]);
    d.d_c2h.copy_from_slice(&dkm[0..32]);
    d.d_h2c.copy_from_slice(&dkm[32..64]);
    d
}

fn mac(key: &[u8], role: &[u8], transcript: &[u8]) -> [u8; 32] {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("any key length");
    m.update(role);
    m.update(transcript);
    m.finalize().into_bytes().into()
}

fn mac_ok(key: &[u8], role: &[u8], transcript: &[u8], got: &[u8]) -> bool {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("any key length");
    m.update(role);
    m.update(transcript);
    m.verify_slice(got).is_ok()
}

/// Public device id for a device token (the token itself never leaves the device).
pub fn device_id(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"rf-device");
    h.update(token.trim().to_ascii_lowercase().as_bytes());
    hex::encode(&h.finalize()[..16])
}

/// What the computer stores for a trusted device: salt and `PBKDF2(token, salt)`.
pub fn device_key(token: &str) -> ([u8; SALT_LEN], [u8; 32]) {
    let salt: [u8; SALT_LEN] = rand::random();
    let key = password_key(&token.trim().to_ascii_lowercase(), &salt, DEVICE_ITERATIONS);
    (salt, key)
}

/// Viewer -> host instead of a reply: "log me in as this trusted device".
pub fn device_request(token: &str) -> Vec<u8> {
    let mut out = DEVICE_MAGIC.to_vec();
    out.extend_from_slice(&hex::decode(device_id(token)).expect("hex"));
    out
}

/// Host: is this step-2 message a device request? Returns the device id.
pub fn parse_device_request(b: &[u8]) -> Option<String> {
    (b.len() == 20 && &b[..4] == DEVICE_MAGIC).then(|| hex::encode(&b[4..]))
}

/// Host side of the handshake.
pub struct HostHandshake {
    secret: SecretKey,
    pub host_pub: [u8; PUB_LEN],
    pub salt: [u8; SALT_LEN],
    pub normalize: bool,
    pub iterations: u32,
    /// Hello flags (FLAG_DEVICE_LOGIN, FLAG_DEVICE_MODE).
    pub flags: u8,
}

/// Viewer's reply (step 2).
pub struct ViewerReply {
    pub viewer_pub: Vec<u8>,
    pub mac: Vec<u8>,
}

impl HostHandshake {
    /// Password login; trusted devices may switch to a device login instead.
    pub fn new(normalize: bool) -> Self {
        let secret = random_secret();
        let host_pub = pub_bytes(&secret);
        Self { secret, host_pub, salt: rand::random(), normalize, iterations: ITERATIONS, flags: FLAG_DEVICE_LOGIN }
    }

    /// Second step 1 for a trusted device, with that device's stored salt.
    pub fn for_device(salt: [u8; SALT_LEN]) -> Self {
        let secret = random_secret();
        let host_pub = pub_bytes(&secret);
        Self { secret, host_pub, salt, normalize: false, iterations: DEVICE_ITERATIONS, flags: FLAG_DEVICE_MODE }
    }

    /// Step 1 as bytes (native protocol).
    pub fn hello_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(11 + SALT_LEN + PUB_LEN);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(self.normalize as u8);
        out.extend_from_slice(&self.iterations.to_be_bytes());
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&self.host_pub);
        out.push(self.flags);
        out
    }

    /// Verify the viewer (slow: PBKDF2). Ok((channel, MAC_h)) or Err if the password is wrong.
    pub fn finish(self, password: &str, reply: &ViewerReply) -> Result<(Channel, [u8; MAC_LEN])> {
        let pw = prepare_password(password, self.normalize);
        let pw_key = password_key(&pw, &self.salt, self.iterations);
        self.finish_with_key(&pw_key, reply)
    }

    /// Verify the viewer against an already derived key (trusted device).
    pub fn finish_with_key(self, pw_key: &[u8; 32], reply: &ViewerReply) -> Result<(Channel, [u8; MAC_LEN])> {
        if reply.viewer_pub.len() != PUB_LEN || reply.mac.len() != MAC_LEN {
            bail!("malformed handshake");
        }
        let shared = ecdh(&self.secret, &reply.viewer_pub)?;
        let d = derive(pw_key, &shared, &self.salt, &self.host_pub, &reply.viewer_pub);
        if !mac_ok(&d.mac_c, b"client", &d.transcript, &reply.mac) {
            bail!("wrong password");
        }
        let mac_h = mac(&d.mac_h, b"host", &d.transcript);
        let direct = DirectPair { tx: Cipher::new(&d.d_h2c), rx: Cipher::new(&d.d_c2h) };
        Ok((Channel { tx: Cipher::new(&d.k_h2c), rx: Cipher::new(&d.k_c2h), direct }, mac_h))
    }
}

impl ViewerReply {
    /// Step 2 as bytes (native protocol).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.viewer_pub.clone();
        out.extend_from_slice(&self.mac);
        out
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        if b.len() != PUB_LEN + MAC_LEN {
            bail!("malformed handshake reply");
        }
        Ok(Self { viewer_pub: b[..PUB_LEN].to_vec(), mac: b[PUB_LEN..].to_vec() })
    }
}

/// Viewer side, waiting for the host's MAC (step 3).
pub struct ViewerPending {
    mac_h_key: [u8; 32],
    transcript: Vec<u8>,
    channel: Channel,
}

impl ViewerPending {
    /// Check that the host knows the password too (protects against a fake host/relay).
    pub fn finish(self, mac_h: &[u8]) -> Result<Channel> {
        if !mac_ok(&self.mac_h_key, b"host", &self.transcript, mac_h) {
            bail!("the remote computer could not prove it knows the password");
        }
        Ok(self.channel)
    }
}

/// The host's step 1, parsed.
pub struct HostHello<'a> {
    normalize: bool,
    iterations: u32,
    salt: &'a [u8],
    host_pub: &'a [u8],
    pub flags: u8,
}

impl HostHello<'_> {
    /// The computer accepts trusted-device logins.
    pub fn device_login(&self) -> bool {
        self.flags & FLAG_DEVICE_LOGIN != 0
    }
}

pub fn parse_hello(hello: &[u8]) -> Result<HostHello<'_>> {
    if hello.len() < 10 || &hello[..4] != MAGIC {
        bail!("the remote computer does not support encrypted connections (update it)");
    }
    match hello[4] {
        VERSION if hello.len() == 11 + SALT_LEN + PUB_LEN => {}
        1 => bail!("the remote computer runs an older RemoteFriend: update it"),
        v if v > VERSION => bail!("the remote computer runs a newer RemoteFriend: update this app"),
        _ => bail!("malformed handshake"),
    }
    let iterations = u32::from_be_bytes(hello[6..10].try_into().expect("4 bytes"));
    if !(10_000..=5_000_000).contains(&iterations) {
        bail!("unreasonable key derivation cost");
    }
    Ok(HostHello {
        normalize: hello[5] != 0,
        iterations,
        salt: &hello[10..10 + SALT_LEN],
        host_pub: &hello[10 + SALT_LEN..10 + SALT_LEN + PUB_LEN],
        flags: hello[10 + SALT_LEN + PUB_LEN],
    })
}

/// Viewer: answer the host's hello (native bytes) with the password, or with the device
/// token after a device request. Slow (PBKDF2).
pub fn viewer_respond(hello: &[u8], password: &str) -> Result<(ViewerReply, ViewerPending)> {
    let h = parse_hello(hello)?;
    let (normalize, iterations, salt, host_pub) = (h.normalize, h.iterations, h.salt, h.host_pub);
    let secret = random_secret();
    let viewer_pub = pub_bytes(&secret);
    let shared = ecdh(&secret, host_pub)?;
    let pw = prepare_password(password, normalize);
    let pw_key = password_key(&pw, salt, iterations);
    let d = derive(&pw_key, &shared, salt, host_pub, &viewer_pub);
    let reply = ViewerReply { viewer_pub: viewer_pub.to_vec(), mac: mac(&d.mac_c, b"client", &d.transcript).to_vec() };
    let pending = ViewerPending {
        mac_h_key: d.mac_h,
        transcript: d.transcript,
        channel: Channel {
            tx: Cipher::new(&d.k_c2h),
            rx: Cipher::new(&d.k_h2c),
            direct: DirectPair { tx: Cipher::new(&d.d_c2h), rx: Cipher::new(&d.d_h2c) },
        },
    };
    Ok((reply, pending))
}

/// Host's step-3 message (native): 0 + MAC_h, or 1/2 + reason.
pub fn host_result_ok(mac_h: &[u8; MAC_LEN]) -> Vec<u8> {
    let mut out = vec![0u8];
    out.extend_from_slice(mac_h);
    out
}

pub fn host_result_err(code: u8, reason: &str) -> Vec<u8> {
    let mut out = vec![code.max(1)];
    out.extend_from_slice(reason.as_bytes());
    out
}

/// Viewer: parse the host's step-3 message.
pub fn viewer_result(b: &[u8]) -> Result<&[u8]> {
    match b.split_first() {
        Some((0, mac)) if mac.len() == MAC_LEN => Ok(mac),
        Some((_, reason)) => bail!("{}", String::from_utf8_lossy(reason)),
        None => bail!("empty handshake result"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_device_logs_in_without_the_password() {
        let token = crate::new_secret_hex();
        let (salt, key) = device_key(&token);
        let first = HostHandshake::new(true);
        assert!(parse_hello(&first.hello_bytes()).unwrap().device_login());
        // viewer names its device instead of answering
        let req = device_request(&token);
        assert_eq!(parse_device_request(&req).unwrap(), device_id(&token));
        let host = HostHandshake::for_device(salt);
        let (reply, pending) = viewer_respond(&host.hello_bytes(), &token).unwrap();
        let (mut host_ch, mac_h) = host.finish_with_key(&key, &reply).unwrap();
        let mut viewer_ch = pending.finish(viewer_result(&host_result_ok(&mac_h)).unwrap()).unwrap();
        let ct = viewer_ch.tx.encrypt(b"hi").unwrap();
        assert_eq!(host_ch.rx.decrypt(&ct).unwrap(), b"hi");
        // another token does not match the stored key
        let host = HostHandshake::for_device(salt);
        let (reply, _) = viewer_respond(&host.hello_bytes(), &crate::new_secret_hex()).unwrap();
        assert!(host.finish_with_key(&key, &reply).is_err());
    }

    fn run(host_pw: &str, viewer_pw: &str, normalize: bool) -> Result<(Channel, Channel)> {
        // Fewer iterations than production to keep tests fast (viewers accept >= 10k).
        let host = HostHandshake { iterations: 20_000, ..HostHandshake::new(normalize) };
        let hello = host.hello_bytes();
        let (reply, pending) = viewer_respond(&hello, viewer_pw)?;
        let wire = ViewerReply::from_bytes(&reply.to_bytes())?;
        let (host_ch, mac_h) = host.finish(host_pw, &wire)?;
        let res = host_result_ok(&mac_h);
        let viewer_ch = pending.finish(viewer_result(&res)?)?;
        Ok((host_ch, viewer_ch))
    }

    #[test]
    fn handshake_and_messages_round_trip() {
        let (mut host, mut viewer) = run("abcde-23456", " ABCDE 23456 ", true).unwrap();
        let ct = viewer.tx.encrypt(b"hello host").unwrap();
        assert_eq!(host.rx.decrypt(&ct).unwrap(), b"hello host");
        let ct2 = host.tx.encrypt(b"frame").unwrap();
        assert_eq!(viewer.rx.decrypt(&ct2).unwrap(), b"frame");
        // the direct path has its own keys and counters
        let d1 = viewer.direct.tx.encrypt(b"direct").unwrap();
        assert_eq!(host.direct.rx.decrypt(&d1).unwrap(), b"direct");
        assert!(host.rx.decrypt(&d1).is_err());
        let d2 = host.direct.tx.encrypt(b"back").unwrap();
        assert_eq!(viewer.direct.rx.decrypt(&d2).unwrap(), b"back");
        // replay / reordering is rejected
        assert!(host.rx.decrypt(&ct).is_err());
    }

    #[test]
    fn wrong_password_is_rejected() {
        assert!(run("abcde-23456", "abcde-23457", true).is_err());
        // custom passwords are case sensitive
        assert!(run("MySecret!", "mysecret!", false).is_err());
    }

    #[test]
    fn tampering_is_detected() {
        let (mut host, mut viewer) = run("pw-123456789", "pw-123456789", false).unwrap();
        let mut ct = viewer.tx.encrypt(b"click").unwrap();
        ct[0] ^= 1;
        assert!(host.rx.decrypt(&ct).is_err());
    }
}
