# RemoteFriend Security Notes

## Threat model

RemoteFriend gives a client that knows the password, and that the host approves, access to the screen, input and file transfer. In practice this is as powerful as sitting at the computer. Don't share the password, don't leave the host running unattended, and don't enable `REMOTE_FRIEND_AUTO_ACCEPT=1` for everyday use.

## Which links are encrypted?

| Path | Encryption | Notes |
|---|---|---|
| Browser → VPS → computer (`https://your-domain`) | End-to-end (AES-256-GCM) inside HTTPS/TLS | The relay only forwards ciphertext |
| App → VPS → computer (`33202`) | End-to-end (AES-256-GCM) inside TLS with fingerprint pinning | The relay only forwards ciphertext |
| App → computer on the local network (`33200`) | End-to-end (AES-256-GCM) | Off by default |
| Browser → computer on the local network (`33201`) | Plain HTTP/WebSocket (browsers block WebCrypto on plain http) | Off by default; trusted networks only |

Since v0.7.0 the relay never sees the password, the screen, the input or the files. It still sees
metadata: which computer IDs are used, when, from which IP addresses and how much data flows.

## High-risk issues fixed in v0.5.6

- The password on the browser → VPS path is now forwarded to the host and verified.
- The custom rustls verifier now actually verifies TLS 1.2/1.3 handshake signatures.
- The 9-digit ID alone is no longer enough to register a host; a persistent 256-bit host secret is required.
- The default `1234` password was removed; if no password is given, a random session password is generated.
- WebSocket `Origin` validation was added.
- Limits on message, connection, file size, chunk, concurrent transfer and session counts were added.
- Incoming files are no longer written to predictable `/tmp` names or at arbitrary offsets; sequential chunks, safe names, `.part` files and atomic renames are used.
- The browser password is not stored in `localStorage`.
- On Unix, the host registration secret and settings are written with `0600` permissions and the config directory with `0700`.

## Added in v0.6.2

- Failed-password lockout (on the host, for all paths): 5 failures within 60 s → 1 min lockout, then 2/4/8/16 min on repeat.
  During a lockout the password is not evaluated at all.
- Relay server limit of 12 connection requests per minute per IP (behind nginx, `X-Real-IP` is accepted only
  from the local proxy).
- Automatic HTTPS in the VPS setup (Let's Encrypt; sslip.io if there is no domain): on the browser path the password and
  video no longer travel over the internet in plaintext.
- The TLS key is provided via systemd `LoadCredential=`; the key file does not have to be readable by the service user.
- On Wayland, input goes through the desktop's RemoteDesktop portal (user consent, revocable).
- Keys and mouse buttons still held when a session ends are released on the host automatically.
- Strict CSP (`default-src 'none'`), `X-Frame-Options: DENY`, `Permissions-Policy`.
- Approval prompts go through a single stdin reader (a prompt that times out can't steal the next answer).
- Generated passwords are readable (`abcde-23456`, ~50 bits); with the lockout, online guessing is impractical.
  The password persists in `~/.config/remotefriend/password` (0600); regenerate it with `--new-password`.
- Trusted devices: if the operator approves with **Always allow**, the browser receives a 256-bit token and the host stores only its
  SHA-256 hash (`trusted_devices.json`, 0600). The token does not replace the password; it only skips the approval prompt.
  `--forget-devices` revokes all of them.
- A single-use reconnect token, valid for 10 minutes, for dropped sessions (the password is still required).

## v0.8.0: sound

The computer's sound travels inside the same end-to-end encrypted channel as the picture
(message kind 2 in the browser protocol, `Packet::Audio` natively). It is recorded only while a
connected viewer has sound turned on and stops a few seconds after the last listener leaves; it
records the system output (what the speakers play), never a microphone. The unencrypted
local-network page gets no sound.

## v0.7.0: end-to-end encryption and a locked-down server

- **End-to-end encryption** between the viewer (browser or app) and the shared computer:
  ECDH P-256 key exchange, keys derived with HKDF-SHA256 from the shared secret salted with
  PBKDF2-SHA256(password, random salt, 300,000 iterations); both sides prove the password with
  HMAC-SHA256 over the handshake transcript; then AES-256-GCM with per-direction counter nonces
  (replayed, reordered or modified messages are rejected). Implementation:
  `crates/common/src/e2e.rs` (Rust) and `crates/common/src/webapp.html` (WebCrypto).
- The password is **never sent to the relay**. A relay that tampers with the handshake cannot read
  or inject anything; at most it can try to guess the password offline at PBKDF2 cost
  (~2^50 guesses x 300k iterations for a generated password).
- **Server key**: the relay only lets computers that know its registration key register new IDs, so
  strangers cannot use your VPS. Computers that were already registered keep working with their
  host secret.
- Per-IP limits on the relay (open connections, requests per minute) and in nginx (requests/s,
  concurrent connections).
- **Local network access is off by default** (listeners bind to 127.0.0.1); it can be enabled in
  the app. The native protocol is end-to-end encrypted on the local network as well.
- Releases publish `SHA256SUMS.txt`; `setup-vps.sh` refuses a server binary that does not match.
- `deploy/harden-vps.sh`: unattended security updates, fail2ban for SSH, and key-only SSH when a key
  is installed.

Remaining limits: the relay still learns metadata (which IDs connect, when, from which IP, and how
much data); the local-network browser page over plain http (only if you enable local network
access) is not encrypted; there has been no independent security audit.

## Secure deployment

- Keep local network access off unless you need it; never forward ports `33200` and `33201` to the WAN.
- Keep the server key (`/opt/remotefriend/register.key` is read via systemd `LoadCredential`) private.
- Run `deploy/harden-vps.sh` (automatic security updates, fail2ban, key-only SSH).
- Keep the VPS web UI on `127.0.0.1:33203` and publish it behind Nginx/HTTPS.
- Pin the SHA-256 fingerprint of the `33202` TLS certificate on the host and the native client.
- On the VPS, use a dedicated `remotefriend` system user, `UMask=0077` and systemd hardening.
- Back up the host registry file (`/var/lib/remotefriend/hosts.json`) and keep it private.
- If `~/.config/remotefriend/host_secret` on the host device is lost, the same ID cannot register again; the VPS administrator must remove the corresponding registry entry manually.

## Known remaining risks

- The relay sees connection metadata (IDs, times, IP addresses, traffic volume).
- The local-network browser page (plain http, only when local network access is enabled) is not encrypted.
- A malicious relay can try passwords offline against one recorded handshake; the generated password
  (~50 bits) and PBKDF2 with 300,000 iterations make that expensive, but a weak custom password would not.
- The lockout applies to the whole host: someone who knows the ID can deliberately make failed attempts and briefly lock out
  the legitimate user (denial of service). This is a deliberate trade-off against guessing attacks.
- Screen capture, input injection and file transfer run with the privileges of the OS user.
- On Windows, sensitive file permissions are not enforced with exact Unix `0600` semantics; the NTFS ACLs of the user profile must be kept intact.

When reporting a vulnerability, do not include passwords, host secrets, TLS private keys, screenshots or real personal files.
