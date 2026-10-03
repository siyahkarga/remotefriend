# RemoteFriend Security Notes

## Threat model

RemoteFriend gives a client that knows the password, and that the host approves, access to the screen, input and file transfer. In practice this is as powerful as sitting at the computer. Don't share the password, don't leave the host running unattended, and don't enable `REMOTE_FRIEND_AUTO_ACCEPT=1` for everyday use.

## Which links are encrypted?

| Path | Encryption | Use |
|---|---|---|
| LAN native `33200` | None | Trusted LAN or VPN only |
| LAN web `33201` | HTTP/WS in the default setup | Trusted LAN only; an HTTPS reverse proxy is preferred |
| Native internet `33202` | Client/host → VPS TLS + fingerprint pinning | Requires a trusted VPS; not E2E |
| VPS web | The setup script installs Let's Encrypt HTTPS; host dial-back TLS terminates on the VPS | Requires a trusted VPS; not E2E |

The VPS relay terminates TLS before forwarding traffic. The relay server can therefore see the password, screen, input and files. If the server is compromised, confidentiality is lost. True end-to-end confidentiality is out of scope for this release.

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

## Secure deployment

- Do not forward ports `33200` and `33201` to the WAN.
- Keep the VPS web UI on `127.0.0.1:33203` and publish it behind Nginx/HTTPS.
- Pin the SHA-256 fingerprint of the `33202` TLS certificate on the host and the native client.
- On the VPS, use a dedicated `remotefriend` system user, `UMask=0077` and systemd hardening.
- Back up the host registry file (`/var/lib/remotefriend/hosts.json`) and keep it private.
- If `~/.config/remotefriend/host_secret` on the host device is lost, the same ID cannot register again; the VPS administrator must remove the corresponding registry entry manually.

## Known remaining risks

- The relay is not end-to-end encrypted.
- LAN native and default LAN web traffic are not encrypted.
- The lockout applies to the whole host: someone who knows the ID can deliberately make failed attempts and briefly lock out
  the legitimate user (denial of service). This is a deliberate trade-off against guessing attacks.
- Screen capture, input injection and file transfer run with the privileges of the OS user.
- On Windows, sensitive file permissions are not enforced with exact Unix `0600` semantics; the NTFS ACLs of the user profile must be kept intact.

When reporting a vulnerability, do not include passwords, host secrets, TLS private keys, screenshots or real personal files.
