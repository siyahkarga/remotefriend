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
//! (big endian), one counter per direction. The browser implements the same with
//! WebCrypto (`webapp.html`).

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use sha2::{Digest, Sha256};

pub const VERSION: u8 = 1;
pub const ITERATIONS: u32 = 300_000;
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
    fn new(key: &[u8]) -> Self {
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
    let mut d = Derived { mac_c: [0; 32], mac_h: [0; 32], k_c2h: [0; 32], k_h2c: [0; 32], transcript };
    d.mac_c.copy_from_slice(&okm[0..32]);
    d.mac_h.copy_from_slice(&okm[32..64]);
    d.k_c2h.copy_from_slice(&okm[64..96]);
    d.k_h2c.copy_from_slice(&okm[96..128]);
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

/// Host side of the handshake.
pub struct HostHandshake {
    secret: SecretKey,
    pub host_pub: [u8; PUB_LEN],
    pub salt: [u8; SALT_LEN],
    pub normalize: bool,
    pub iterations: u32,
}

/// Viewer's reply (step 2).
pub struct ViewerReply {
    pub viewer_pub: Vec<u8>,
    pub mac: Vec<u8>,
}

impl HostHandshake {
    pub fn new(normalize: bool) -> Self {
        let secret = random_secret();
        let host_pub = pub_bytes(&secret);
        Self { secret, host_pub, salt: rand::random(), normalize, iterations: ITERATIONS }
    }

    /// Step 1 as bytes (native protocol).
    pub fn hello_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(10 + SALT_LEN + PUB_LEN);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(self.normalize as u8);
        out.extend_from_slice(&self.iterations.to_be_bytes());
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&self.host_pub);
        out
    }

    /// Verify the viewer (slow: PBKDF2). Ok((channel, MAC_h)) or Err if the password is wrong.
    pub fn finish(self, password: &str, reply: &ViewerReply) -> Result<(Channel, [u8; MAC_LEN])> {
        if reply.viewer_pub.len() != PUB_LEN || reply.mac.len() != MAC_LEN {
            bail!("malformed handshake");
        }
        let shared = ecdh(&self.secret, &reply.viewer_pub)?;
        let pw = prepare_password(password, self.normalize);
        let pw_key = password_key(&pw, &self.salt, self.iterations);
        let d = derive(&pw_key, &shared, &self.salt, &self.host_pub, &reply.viewer_pub);
        if !mac_ok(&d.mac_c, b"client", &d.transcript, &reply.mac) {
            bail!("wrong password");
        }
        let mac_h = mac(&d.mac_h, b"host", &d.transcript);
        Ok((Channel { tx: Cipher::new(&d.k_h2c), rx: Cipher::new(&d.k_c2h) }, mac_h))
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

/// Viewer: answer the host's hello (native bytes). Slow (PBKDF2).
pub fn viewer_respond(hello: &[u8], password: &str) -> Result<(ViewerReply, ViewerPending)> {
    if hello.len() != 10 + SALT_LEN + PUB_LEN || &hello[..4] != MAGIC {
        bail!("the remote computer does not support encrypted connections (update it)");
    }
    if hello[4] != VERSION {
        bail!("unsupported encryption version {}", hello[4]);
    }
    let normalize = hello[5] != 0;
    let iterations = u32::from_be_bytes(hello[6..10].try_into().expect("4 bytes"));
    if !(10_000..=5_000_000).contains(&iterations) {
        bail!("unreasonable key derivation cost");
    }
    let salt = &hello[10..10 + SALT_LEN];
    let host_pub = &hello[10 + SALT_LEN..];
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
        channel: Channel { tx: Cipher::new(&d.k_c2h), rx: Cipher::new(&d.k_h2c) },
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
