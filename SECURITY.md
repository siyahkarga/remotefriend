# RemoteFriend Security Notes

## Reporting a vulnerability

Please write to **support@blobidea.com** with the subject “RemoteFriend security” instead of
opening a public issue. Describe what you found, how to reproduce it and which version you used;
do not include passwords, access keys, host secrets, TLS private keys, screenshots or real
personal files. We answer within 7 days, fix confirmed problems as fast as we can and credit you
in the release notes if you like. Security releases are announced in the app itself: every older
version shows a red “security update” notice. Contact details are also published at
<https://blobidea.com/.well-known/security.txt>.

## Threat model

RemoteFriend gives a client that knows the password, and that the host approves, access to the screen, input and file transfer. In practice this is as powerful as sitting at the computer. Don't share the password, don't leave the host running unattended, and don't enable `REMOTE_FRIEND_AUTO_ACCEPT=1` for everyday use.

## Which links are encrypted?

| Path | Encryption | Notes |
|---|---|---|
| Browser → relay → computer (`https://remote.blobidea.com`) | End-to-end (AES-256-GCM) inside HTTPS/TLS | The relay only forwards ciphertext |
| App → relay → computer (`33202`) | End-to-end (AES-256-GCM) inside TLS with the relay's certificate pinned in the app | The relay only forwards ciphertext |
| App → computer on the local network (`33200`) | End-to-end (AES-256-GCM) | Off by default |
| Browser → computer on the local network (`33201`) | Plain HTTP/WebSocket (browsers block WebCrypto on plain http) | Off by default; trusted networks only |

Since v0.7.0 the relay never sees the password, the screen, the input or the files. It still sees
metadata: which computer IDs are used, when, from which IP addresses and how much data flows.

## v0.12.0: access keys, built-in server, update notices

- The relay decides with **access keys** which computers may register: one owner key plus a key
  per person with its own computer limit and expiry. Keys are stored on the relay only as
  SHA-256 hashes; revoking, deleting or expiry takes the key's computers offline within 15
  seconds and refuses them from then on. Connecting *to* a computer never needs a key.
- The app has the blobidea relay and the SHA-256 fingerprint of its certificate built in, so
  there is no “trust this server?” question to get wrong; another relay can still be set.
- Twice a day the app asks blobidea.com for the current version (only the request itself; it can
  be turned off) and shows a red notice when the installed version has a known security problem.
- The relay keeps its logs (IP addresses, IDs, times) for 30 days.
- The app (client, terminal host, phone page) is open source under the GPL-3.0; the relay is not.

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

## v0.11.0: password policy, remembered passwords, one pointer

- The password changes **when RemoteFriend starts** by default; "after every session" and
  "never" remain options. Approval of new devices is unchanged.
- Viewers remember the last password that worked for a computer: the app in `recents.json`
  (0600), the browser in local storage. A wrong password deletes it. Trusted-device keys are
  still preferred (no password stored at all).
- Wayland: the pointer is left out of the picture when the desktop allows it, so viewers show
  only their own pointer. Windows: the pointer is placed in physical pixels (correct on scaled
  and secondary monitors).

## v0.10.0: direct connections, downloads, safe view

- **Direct (peer-to-peer) path**: a WebRTC data channel (ICE, DTLS 1.2, SCTP; `str0m` with
  pure-Rust crypto on the computer, the browser's own WebRTC on phones). The offer/answer —
  including the DTLS fingerprints — travels inside the end-to-end encrypted session, so the relay
  can't redirect it. Our AES-256-GCM encryption still applies on top, with separate keys derived
  in the same handshake. Trade-off: the two devices see each other's IP addresses; the setting
  *Direct connections* turns it off. The relay's STUN service (UDP 3478) only reflects the sender's
  address and is rate limited per IP.
- **Downloads**: a viewer allowed to use files can list folders (hidden files are left out) and
  download files the logged-in user can read; the computer shows who downloads what. Uploads keep
  their real file name (`Downloads/RemoteFriend`, numbered on conflict) and can be canceled
  (the partial file is deleted).
- **Safe view** (default) hides RemoteFriend and blacks out windows of private apps in the picture
  (Windows, macOS, X11 — Wayland gives no window positions). It is a privacy aid, not a security
  boundary: a viewer with mouse and keyboard can still open those apps' data in other ways.

## v0.9.0: rotating password, trusted-device keys, session control

- **The password changes after every session** that used it (immediately after a goodbye, otherwise
  after a 60 s grace for reconnects) and at every start; it can be kept fixed in Settings.
- **Trusted devices log in without the password**: on "Always allow" the device receives a
  random 256-bit token; the computer stores its SHA-256 and `PBKDF2(token, salt)`
  (`trusted_devices.json`, 0600). The viewer names its device id, the computer answers with that
  device's salt, and the normal handshake runs with the stored key — still end-to-end, the relay
  learns nothing. Devices can be removed individually. The app keeps its tokens in
  `recents.json` (0600), the browser in local storage.
- **Switching sides** uses a one-time grant: valid for 3 minutes, consumed by the first login,
  never written to disk.
- The computer's user sees every session and can disconnect it, pause all input ("Take back
  control") or turn off mouse/keyboard, sound, files and clipboard per viewer; the viewer is told.
- While a session runs, the RemoteFriend window is minimized and (Windows, macOS) excluded from
  screen capture; closing it asks first.
- Dead connections are detected: TCP keepalive on every link, relay heartbeat echoes (a computer
  reconnects after 70 s without an answer), a 90 s idle limit on relayed sessions and a 45 s idle
  limit on sessions (viewers ping every 2 s). Per-IP limits on the relay were raised (64 open
  connections, 30 requests per minute) because a household shares one address.

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
- Releases publish `SHA256SUMS.txt` (blobidea.com download page); `setup-vps.sh` refuses a server binary that does not match.
- `deploy/harden-vps.sh`: unattended security updates, fail2ban for SSH, and key-only SSH when a key
  is installed.

Remaining limits: the relay still learns metadata (which IDs connect, when, from which IP, and how
much data); the local-network browser page over plain http (only if you enable local network
access) is not encrypted; there has been no independent security audit.

## Using it safely

- Keep local network access off unless you need it; never forward ports `33200` and `33201` to the internet.
- Treat your access key like a password; if it leaks, ask the server owner to revoke it and get a new one.
- Remove trusted devices you no longer use (*Settings → Security*).
- Keep RemoteFriend up to date; a red notice in the app means a security fix.
- If `~/.config/remotefriend/host_secret` is lost, the same ID cannot register again; the server
  owner has to remove the old entry.

## Known remaining risks

- The relay sees connection metadata (IDs, computer names, times, IP addresses, traffic volume).
- There has been no independent security audit yet.
- The local-network browser page (plain http, only when local network access is enabled) is not encrypted.
- A malicious relay can try passwords offline against one recorded handshake; the generated password
  (~50 bits) and PBKDF2 with 300,000 iterations make that expensive, but a weak custom password would not.
- The lockout applies to the whole host: someone who knows the ID can deliberately make failed attempts and briefly lock out
  the legitimate user (denial of service). This is a deliberate trade-off against guessing attacks.
- Screen capture, input injection and file transfer run with the privileges of the OS user.
- On Windows, sensitive file permissions are not enforced with exact Unix `0600` semantics; the NTFS ACLs of the user profile must be kept intact.

When reporting a vulnerability, do not include passwords, host secrets, TLS private keys, screenshots or real personal files.
