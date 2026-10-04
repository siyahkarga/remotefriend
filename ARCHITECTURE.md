# How RemoteFriend works

```text
 Viewer (phone browser or desktop app)                 Shared computer (desktop app)
 ───────────────────────────────────                   ─────────────────────────────
  login: ID + password, or device key                   capture: PipeWire portal (Wayland),
  decode: H.264 (WebCodecs / openh264), Opus              xcap (X11, Windows, macOS)
  input, clipboard, files                              encode: H.264 (openh264), Opus (libopus)
          │                                            inject: portal (Wayland), enigo
          │  1. TLS to the relay, "connect to ID"               │
          ▼                                                     │ keeps one TLS link open
   ┌──────────────── relay server (remote.blobidea.com) ───────────────┐
   │ finds the computer by ID, asks it to dial back, then splices the  │
   │ two links. Access keys decide which computers may register,       │
   │ per-IP limits, STUN on UDP 3478. nginx serves the phone page.     │
   └───────────────────────────────────────────────────────────────────┘
          │  2. end-to-end handshake through the relay (ECDH P-256 +
          │     PBKDF2 password or device key, HMAC proofs both ways)
          │  3. session: AES-256-GCM messages (relay sees only ciphertext)
          │  4. WebRTC offer/answer inside the session → direct data channel
          ▼     (ICE/DTLS/SCTP; our AES-256-GCM on top) when the network allows
     Viewer ◄══════════════ direct, peer-to-peer ══════════════► Computer
```

## Pieces

| Part | Technology |
|---|---|
| Language | Rust (workspace: `common`, `host`, `app`; the relay is a separate, closed-source crate) |
| Desktop UI | egui / eframe |
| Phone page | one HTML file, WebCrypto, WebCodecs, WebRTC, Web Audio |
| Video | H.264 (openh264), JPEG fallback; ack-based flow control, adaptive bitrate |
| Sound | Opus 48 kHz stereo (libopus), PCM fallback; PipeWire / WASAPI / CoreAudio loopback (cpal) |
| Encryption | ECDH P-256, PBKDF2-SHA256, HKDF-SHA256, HMAC-SHA256, AES-256-GCM (RustCrypto) |
| Transport | TLS 1.3 (rustls) to the relay, WebSocket for browsers, WebRTC data channel (str0m) direct |
| Server | Rust relay (closed source) + nginx (Let's Encrypt), systemd sandbox, fail2ban, unattended upgrades |
| Packages | .deb, tar.gz, NSIS installer, macOS .dmg; built by GitHub Actions, published on blobidea.com with SHA256SUMS |

## Security in one paragraph

Nobody in the middle — not the relay, not the network — can see or change the screen, input,
sound or files: everything is end-to-end encrypted with keys only the two devices derive, and
the password itself never leaves the devices. To get in, an attacker needs the current password
(changes after every session, about 50 bits, rate limited) **and** approval on the computer, or
a trusted device's key (stored on that device). The realistic risks are the endpoints: malware
on the computer or on a trusted phone, someone who sees the password and is approved, or a
stolen unlocked phone with a trusted browser. The relay learns metadata (which IDs connect,
when, from which IP, how much data); with a direct connection the two devices learn each
other's IP. There has been no independent audit.
