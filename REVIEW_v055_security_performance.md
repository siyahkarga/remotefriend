> Historical document (applies to an older version).

# RemoteFriend v0.5.5 — Performance and Security Review

**Review date:** October 2, 2026
**Package reviewed:** `remotefriend-v0.5.5-kaynak.zip`
**Fix prepared:** `remotefriend-v0.5.6-hotfix-kaynak`
**Original ZIP SHA-256:** `5e45870fbc92bf523f1e02f2116cf76e7b9541d6014fc7174c916365587fd4bd`

## 1. Summary

The 3–5 FPS problem does not have a single cause; it comes from several design flaws that amplify each other:

1. The original host is tuned for at most about **15 FPS** (`FPS_MS = 66`). On top of that, the 66 ms wait happens **after** capture and encode. So the real frame time is `capture + encode + network write + 66 ms`. If the processing part takes 130–250 ms, the result is naturally about 3–5 FPS.
2. The LAN browser page is usually opened via `http://IP:33201`. Because WebCodecs `VideoDecoder` requires a secure context, most browsers cannot use the H.264 path on a plain HTTP page at a LAN IP; the app falls back to the heavy JPEG mode.
3. Every viewer starts its own screen capture and its own H.264/JPEG encode loop. A second client can roughly double the CPU load.
4. The native client re-clones the same full image and uploads it to the GPU texture even when no new frame has arrived.
5. When the first frame arrives, the same `Mutex` lock is acquired a second time without being released. This is a real deadlock bug that can freeze on the first image.
6. Software OpenH264, RGBA → RGB → YUV conversions and full-frame copies all run on the CPU. At high resolutions this architecture hits its limits quickly.

The prepared v0.5.6 hotfix addresses the parts of these problems that can be fixed with low risk: 30 FPS target, fixed-rate pacing, a single shared capture/encode pipeline, a small broadcast queue that does not accumulate latency, texture updates only on new frames, the deadlock fix, a shared broadcast pipeline for JPEG, and browser decode backpressure.

**Expectation:** These changes should noticeably improve the 3–5 FPS situation. However, the final result at 1080p/30 depends on the host CPU, the screen capture backend, network bandwidth and software OpenH264 speed. The package could not be compiled because no Rust compiler was available in this review environment; therefore no "guaranteed 30 FPS" is promised.

---

## 2. Review method and limits

Done:

- All Rust, HTML/JavaScript, TOML, YAML, batch and systemd files were statically reviewed.
- The video capture → color conversion → encode → queue → network → decode → texture upload chain was traced.
- The LAN, browser, native client and VPS rendezvous/relay paths were reviewed separately.
- Authentication, the TLS verifier, WebSocket Origin, file transfer, message limits and session management were checked.
- Performance and security patches were applied to the code.
- A 25-item automated static verification was run; 25/25 passed.

Limits:

- The environment had no `cargo`, `rustc` or Rust dependency cache.
- Internet access was disabled, so a temporary Rust toolchain could not be downloaded.
- Therefore `cargo check`, `cargo test`, a real two-computer video test and a latency/FPS benchmark could not be run.
- A dependency vulnerability scan (`cargo audit`) could not be run.

---

## 3. Performance findings

### P1 — Incorrect frame timing

**Original code:** `crates/host/src/main.rs:17`, `:220–269`

```rust
pub(crate) const FPS_MS: u64 = 66; // ~15fps
...
capture + encode + send
...
sleep(66ms)
```

This not only caps the rate at 15 FPS; it adds the capture/encode time to the frame budget. For example:

- Capture + encode + send: 140 ms
- Extra sleep: 66 ms
- Total: 206 ms/frame
- Result: about 4.85 FPS

**Hotfix:** `RF_FPS=30` default and deadline-based fixed pacing. If processing is shorter than the target frame interval, only the remaining time is waited; if it is longer, no extra 33/66 ms is added.

### P2 — LAN browser falling back to JPEG instead of WebCodecs

**Original code:** `crates/common/src/webapp.html:201–219`

The original check was only `typeof VideoDecoder === 'undefined'`. But a plain HTTP page served from a LAN IP is not a secure context. Modern browsers restrict `VideoDecoder` to secure contexts. As a result the app picks the JPEG path or cannot open the H.264 decoder.

The JPEG path compresses each frame on the CPU as a single image and decodes it again as an image in the browser. With constantly changing high-resolution content such as a desktop, 3–5 FPS is entirely possible.

**Hotfix:** `window.isSecureContext` is now checked explicitly. In an insecure context the behavior is clearly marked as JPEG. The JPEG path defaults to 10 FPS / quality 68 and keeps only the newest frame.

**Recommended usage:** the native client for best performance; if a browser is required, an HTTPS/WSS reverse proxy.

### P3 — Repeated capture and encode per client

**Original code:**

- Native: `crates/host/src/main.rs:220–269`
- Local web H.264: `crates/host/src/web.rs:92–130`
- Relay H.264: `crates/host/src/web.rs:215–249`
- Relay JPEG: `crates/host/src/web.rs:270–298`

Every connection started a new capture and encoder loop, multiplying CPU load by the number of clients.

**Hotfix:** There is one shared H.264 producer and one shared JPEG producer. Clients share the same encoded frame. The broadcast queue is limited to 3 frames for H.264 and 2 for JPEG; a slow client does not hold the system back.

### P4 — Stale frames piling up and growing latency

The original native output channels were unbounded, and there was no backpressure on the browser decoder queue either. When the network or decoding slowed down briefly, the user could see not only "low FPS" but also a picture lagging seconds behind.

**Hotfix:**

- The native input queue is bounded.
- The video broadcast queue is small and uses "latest frame" semantics.
- A lagging client skips stale frames.
- When WebCodecs `decodeQueueSize > 2`, the decoder is reset and waits for the next IDR.
- If JPEG decoding is busy, only the most recent pending JPEG is kept.

### P5 — Unnecessary full-frame clone/GPU upload in the native client

**Original code:** `crates/client/src/main.rs:587–639`

On every UI repaint the `ColorImage` was cloned and the texture was `set()` again. The UI also repainted every 66 ms. This consumes a lot of memory bandwidth, especially with 1080p RGBA frames.

**Hotfix:** The UI consumes an incoming frame only once via `take()`; the texture is updated only when there is a new frame. Cloning a `TextureHandle` is just a cheap reference clone.

### P6 — Possible deadlock on the first frame

**Original code:** `crates/client/src/main.rs:248–265`

```rust
let mut s = shared.lock().unwrap();
...
shared.lock().unwrap().status = "connected".into();
```

The same thread tries to lock the same `std::sync::Mutex` again without releasing the first lock. With a non-reentrant mutex this deadlocks.

**Hotfix:** The status is updated through the existing lock, which is then explicitly released.

### P7 — Software encode and multiple color conversions

**Original code:** `crates/host/src/main.rs:534–565`

Every frame goes through:

1. RGBA from screen capture,
2. a copy into a new RGB buffer,
3. RGB → YUV420 conversion,
4. software H.264 encode.

**Hotfix:** OpenH264 auto-thread, 30 FPS, 6 Mbit/s, screen-content realtime and frame-skip settings were added. A real leap, however, requires a platform hardware encoder in a later version.

---

## 4. Security findings

| Severity | Finding | Impact | Hotfix status |
|---|---|---|---|
| **Critical** | The internet browser password was read and discarded by the relay | After host approval the browser session opened without password verification; very dangerous with auto-accept | The password is carried to the host via `ApprovalRequest.auth` and verified with a near-constant-time comparison |
| **Critical** | The custom rustls verifier unconditionally treated the TLS 1.2/1.3 handshake signature as "valid" | Could let an attacker who copied the pinned certificate but lacks the private key impersonate the server | Real `verify_tls12_signature` / `verify_tls13_signature` calls added |
| **Critical** | Rendezvous host registration was bound only to a guessable 9-digit ID | Risk of registering with the same ID and hijacking routing | Random 256-bit host secret stored on the device; register/connect-back bound to the secret |
| **High** | The relay is not truly end-to-end encrypted | The VPS operator / a compromised VPS can see the screen, keyboard/mouse input, password and files | **Remaining architectural risk**; use only a trusted VPS |
| **High** | LAN native TCP and LAN HTTP/WebSocket are plaintext | An attacker on the same network can read/modify traffic | Documented; do not open ports outside a trusted LAN/VPN |
| **High** | No WebSocket `Origin` validation | A malicious web page could try to connect from the user's browser to the local/relay WebSocket | Same-origin allowlist check added; optional `RF_ALLOWED_ORIGIN` |
| **High** | Default password `1234`, written openly on screen/in docs | Very easy to guess | Random default password; configured password not logged; weak-password warning |
| **High** | Messages, queues and session counts were largely unbounded | Memory/CPU DoS, long latency queues | Size limits, bounded channels, semaphores and timeouts added |
| **High** | File receive wrote to a predictable name like `/tmp/rf_<id>_<name>` with no size/offset limits | Symlink/local collision, disk filling, sparse files, partial file appearing complete | Private directory, random name, `.part`, 512 MiB limit, 256 KiB chunks, sequential offsets, atomic rename |
| **Medium** | The browser password was persisted in `localStorage` | Scripts/XSS running on the same origin can read the password | The password is no longer written to persistent storage |
| **Medium** | Relay tokens were a monotonic counter | Predictable session tokens and risk of mixing up connections | Random `u128` tokens and session IDs |
| **Medium** | When one relay direction closed, the other could hang | Resource exhaustion and ghost sessions | Both directions are cancelled together via `select!` |
| **Medium** | No security headers | Missing defense layers that reduce clickjacking/XSS impact | CSP, no-store, nosniff, no-referrer and frame-ancestors added |

### Critical detail: the browser password was not actually used

**Original rendezvous:** `crates/rendezvous/src/main.rs:365–395`

Although `password` was sent in the Hello JSON, the parser only read the `id` and `jpeg` fields. The `ApprovalRequest` then sent to the host contained no password.

**Original host relay web session:** `crates/host/src/main.rs:393–430`, `crates/host/src/web.rs:215–317`

As soon as the host operator answered "E" (yes), the H.264/JPEG session started directly; the native `Handshake.password` check never ran on the web path.

### Critical detail: TLS signature verification bypass

**Original:** `crates/common/src/tls.rs:49–64`

Both the TLS 1.2 and TLS 1.3 callbacks returned `HandshakeSignatureValid::assertion()` directly. Per the rustls contract, this value must only be returned if the signature was actually verified.

**Hotfix:** Certificate fingerprint pinning is kept, and the real handshake signature is now also verified with the certificate's public key.

---

## 5. Main changes in the hotfix

### Performance

- `RF_FPS`: 5–30, default 30.
- `RF_MAX_WIDTH`: default 1920.
- `RF_BITRATE_BPS`: default 6,000,000.
- Deadline-based frame pacing that accounts for processing time.
- Shared H.264 capture/encode pipeline for all native/web viewers.
- Shared JPEG capture/encode pipeline for all JPEG viewers.
- Small broadcast queues; a slow client skips stale frames.
- Encoder reset mechanism so a new viewer receives IDR/SPS/PPS.
- Capture/encode/FPS stats log every 5 seconds.
- PipeWire: take the newest buffer; fewer unnecessary clones.
- Native UI: texture upload only on new frames.
- WebCodecs/JPEG decode backpressure.
- Bounded `try_send` for mouse movement; the input queue can no longer choke video/the app.

### Security

- Protocol `v2`, transport generation `4`; silent old/new mixing prevented.
- Random 256-bit host secret and random u128 relay tokens.
- Real host-side verification of the browser password.
- Real TLS 1.2/1.3 handshake signature verification.
- WebSocket Origin check.
- Random default session password.
- Message, blob, kmsg, file and session limits.
- Files: `.part` + `sync_all` + atomic rename.
- Config/secret files 0600 on Unix, directories 0700.
- Connection/handshake timeouts and session semaphores.
- Security headers, and the password removed from `localStorage`.

---

## 6. Usage settings

### Best starting settings — native client

In a terminal on the Windows host:

```bat
set RF_FPS=30
set RF_MAX_WIDTH=1920
set RF_BITRATE_BPS=6000000
HOST_BASLAT.bat
```

If CPU usage is high or frames cannot keep up:

```bat
set RF_FPS=20
set RF_MAX_WIDTH=1600
set RF_BITRATE_BPS=4500000
HOST_BASLAT.bat
```

Weaker host / low upload bandwidth:

```bat
set RF_FPS=20
set RF_MAX_WIDTH=1280
set RF_BITRATE_BPS=3000000
HOST_BASLAT.bat
```

### Browser client

- With `http://LAN-IP:33201` you will most likely get JPEG.
- If the codec indicator in the UI shows **JPEG**, do not expect 30 FPS.
- Use HTTPS/WSS for H.264/WebCodecs.
- The native client is the better choice over the browser for quality and latency.

### Reading the log

Roughly every 5 seconds the host prints stats like:

```text
video stats: 27.8 fps, capture 8.2 ms, encode 18.5 ms, ...
```

- `capture + encode < 33 ms`: 30 FPS is theoretically possible.
- `encode > 33 ms`: lower the resolution or switch to a hardware encoder.
- If the host produces 30 FPS but the client shows less, the problem is on the network/decode/UI side.
- If the codec is JPEG, first fix the HTTPS/native client issue.

---

## 7. Remaining risks and open items

1. **The relay is not end-to-end encrypted.** The hotfix fixes the TLS verifier but does not stop the VPS from seeing plaintext.
2. **LAN traffic is not encrypted.** Do not forward ports 33200/33201 to the internet.
3. **No hardware encoder.** The RTX 4090's NVENC is not used; OpenH264 runs on the CPU.
4. If H.264 and JPEG viewers are connected at the same time, two separate capture producers may compete.
5. Native decode and YUV → RGBA conversion run in the network task; a separate decode worker would be better.
6. At least one copy of the encoded `Vec` may still be made per native viewer.
7. No persistent IP-based rate limiter/account lockout; only delays, timeouts and concurrency limits.
8. `REMOTE_FRIEND_AUTO_ACCEPT=1` greatly enlarges the attack surface; not recommended.
9. The relay registry keeps host secrets on the server's disk. File permissions are strict, but the VPS must still be trusted.
10. Because of inline HTML, the CSP contains `unsafe-inline`. Moving scripts/styles to separate files would allow a stricter CSP.
11. GitHub Actions versions are pinned to mutable tags instead of commit SHAs; supply-chain hardening is incomplete.
12. No compiler run or real end-to-end test was done.

---

## 8. Next version for a better architecture

### Phase 1 — Hardware encode

On a host with an RTX 4090, encoding should be offloaded from the CPU via NVENC H.264/HEVC/AV1. The NVIDIA Video Codec SDK provides hardware-accelerated encode/decode on Windows and Linux. For low-latency desktop streaming use an H.264 low-latency preset, B-frames off, short GOP/periodic IDR and, where possible, a zero-copy path from the capture surface to the encoder.

Expected gains:

- a large drop in CPU load,
- a better chance at 1080p60/1440p60,
- more consistent quality at the same bitrate,
- lower jitter in encode time.

### Phase 2 — WebRTC transport

If browsers are a target, WebRTC is the better long-term solution than a custom WebSocket video protocol:

- media encryption via DTLS-SRTP,
- congestion control,
- packet loss/jitter handling,
- ICE/STUN/TURN for NAT traversal,
- more natural integration with browsers' hardware decode paths.

This still does not automatically mean "the VPS can never see anything"; TURN, signaling and authentication must be designed correctly. But it provides a more solid foundation than the current TLS-terminate-and-forward relay.

### Phase 3 — True end-to-end session key

- Device identity key,
- ephemeral X25519 key exchange,
- AEAD for host screen/input/file payloads,
- the relay forwarding only encrypted packets,
- a short security code the user can verify on both ends.

### Phase 4 — Zero-copy capture/encode

A path such as Desktop Duplication / Windows Graphics Capture → D3D11 texture → NVENC on Windows, and PipeWire DMA-BUF → VAAPI/NVENC on Linux. This is where the largest remaining performance gain lies.

---

## 9. Verification result

Automated static check: **25 passed, 0 warnings, 0 errors**.

Checks included:

- Parsing all Cargo TOML files.
- `Cargo.lock` consistency for local packages/rand/rustls.
- GitHub Actions YAML parse.
- HTML parse and duplicate `id` check.
- `node --check` for browser JavaScript.
- JavaScript DOM ID references matching the HTML.
- Comment/string-aware delimiter balance in all Rust files.
- Absence of old insecure patterns:
  - unconditional TLS signature assertion,
  - `unbounded_channel`,
  - hard-coded `1234`,
  - password in `localStorage`,
  - old 66 ms frame sleep,
  - predictable `/tmp` receive path.
- Presence of the new protections:
  - protocol v2,
  - host secret,
  - real TLS signature verify,
  - Origin check,
  - bounded channel,
  - H.264/JPEG broadcast,
  - atomic file rename.

**Compiler verification:** Not done. The package must be verified on a real machine, first with `cargo check --workspace --locked`, then with `cargo test --workspace --locked`.

---

## 10. Technical references

- MDN Web Docs — `VideoDecoder`: available only in secure contexts.
- rustls `ServerCertVerifier` documentation: `HandshakeSignatureValid` must be returned only if the signature is actually valid.
- OWASP WebSocket Security Cheat Sheet: recommends `Origin` allowlist validation on every WebSocket handshake.
- NVIDIA Video Codec SDK: NVENC/NVDEC hardware-accelerated video encode/decode APIs.
- W3C WebRTC and IETF RFC 8827: DTLS-based WebRTC security and media transport architecture.
