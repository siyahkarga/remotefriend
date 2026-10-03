# RemoteFriend

Remote desktop for your own computers. Share a computer with one click and control it from
another computer or **any phone browser** — no app needed on the phone. Video is H.264,
the computer's **sound** is streamed too (Opus), mouse, keyboard, touch and file transfer are
supported, and you can run your own relay server.

| Shared computer | Status |
|---|---|
| **Windows** 10/11 | ✔ screen + mouse/keyboard (high-DPI displays included) |
| **macOS** 11+ (Apple Silicon and Intel) | ✔ needs Screen Recording + Accessibility permission (sound: macOS 14.6+) |
| **Linux Wayland** (GNOME, KDE) | ✔ via the desktop's screen-sharing portal (asked once) |
| **Linux X11** | ✔ |

## Install

Download for your system from the [latest release](https://github.com/siyahkarga/remotefriend/releases/latest):

| System | Download | Install |
|---|---|---|
| Windows | [RemoteFriend-Setup.exe](https://github.com/siyahkarga/remotefriend/releases/latest/download/RemoteFriend-Setup.exe) | Run it. If SmartScreen appears: **More info → Run anyway**. |
| macOS | [RemoteFriend-macOS.dmg](https://github.com/siyahkarga/remotefriend/releases/latest/download/RemoteFriend-macOS.dmg) | Drag RemoteFriend to Applications. First start: **right-click → Open**. |
| Ubuntu 24.04+ / Debian 13+ | [RemoteFriend-linux-amd64.deb](https://github.com/siyahkarga/remotefriend/releases/latest/download/RemoteFriend-linux-amd64.deb) | Double-click it, or `sudo apt install ./RemoteFriend-linux-amd64.deb` |
| Other Linux | [RemoteFriend-linux-x86_64.tar.gz](https://github.com/siyahkarga/remotefriend/releases/latest/download/RemoteFriend-linux-x86_64.tar.gz) | Extract, then run `./install.sh` (no root needed). |

Then open **RemoteFriend** from your applications menu. The installers are not code-signed
yet, which is why Windows and macOS ask for confirmation once.

## Use it

1. Open RemoteFriend on the computer you want to share. The left side shows **Your ID** and
   **Password**. Keep the app open while you want remote access.
2. On the other device:
   - **Computer:** open RemoteFriend, type the ID (or the local IP address) and password,
     click **Connect**.
   - **Phone / any browser:** open your server's web address (see below), type the ID and
     password.
3. On the shared computer, a window asks: **Allow**, **Always allow this device** or **Deny**.
   With *Always allow*, that phone/browser connects with just the password from then on — no
   one needs to be at the computer.

First start on **Linux Wayland**: the desktop asks to share the screen. Turn **ON “Allow Remote
Interaction”** and click **Share**. It is remembered. First start on **macOS**: allow
RemoteFriend under *System Settings → Privacy & Security → Screen Recording* and
*Accessibility*, then restart the app.

## Connect from anywhere (your own server)

Within the same network everything works out of the box. To connect over the internet you need a
small relay server (any Ubuntu/Debian VPS). On the VPS run:

```bash
curl -sL https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/setup-vps.sh | sudo bash
```

With your own domain (create a DNS A record pointing to the VPS first):

```bash
curl -sL https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/setup-vps.sh | sudo DOMAIN=remote.example.com bash
```

The script installs the relay, gets a free HTTPS certificate and prints the **web address** and a
**fingerprint**. Without a domain it uses `<your-ip>.sslip.io`, a public DNS name that points to
your server's IP. Then in the app: **Settings → Server** = `<VPS IP>:33202`, **Web address** = the
printed address, **Server key** = the printed key. The first connection asks you to confirm the
fingerprint. Details:
[deploy/SETUP_VPS.md](deploy/SETUP_VPS.md).

## Phone controls

| Mode | Gestures |
|---|---|
| **Touchpad** (default) | Drag: move the pointer · Tap: click · Two-finger tap: right click · Two-finger drag: scroll · Press and hold, then drag: drag & drop |
| **Touchscreen** | Tap where you want to click · Press and hold: right click · Drag: drag & drop · Two fingers: scroll / pan when zoomed |

Pinch to zoom in both modes. The toolbar has the phone keyboard, special keys (Ctrl, Alt, Win,
Esc, arrows, F-keys, copy/paste shortcuts, type the clipboard), quality (Fast / Balanced /
Sharp), **sound on/off** (🔊), file upload and full screen. If the connection drops briefly, the
page reconnects by itself.

**Sound:** what the shared computer plays is sent to the viewer (Opus, 96 kbit/s, about 0.1 s
delay), end-to-end encrypted like the picture. It is only recorded while someone is connected
with sound on, and nothing is sent while the computer is silent. The desktop app has a
**Sound on / off** button in its toolbar.

## Security

- **End-to-end encrypted.** The phone/app and the shared computer agree on keys with an ECDH
  exchange bound to the password (PBKDF2-SHA256, 300k iterations), both sides prove they know the
  password, and everything after that is AES-256-GCM. **The password never goes to the server**, and
  the server only forwards ciphertext: whoever runs it cannot see the screen, the input or the files.
- **Server key.** Only computers that know your server's key can register on it, so nobody else can
  use your VPS as a relay. The setup script prints the key; enter it once in *Settings → Server key*.
- Every connection needs the **password**; new devices also need **approval on the shared computer**
  unless they were allowed with *Always allow* (the computer stores only a hash of that device token).
- The password is created once and kept (format `abcde-23456`, ~50 bits; case and dashes don't
  matter). *Settings → New password* replaces it.
- Wrong passwords are rate limited per source (5 per minute, then 1/2/4/8/16-minute lockouts) plus a
  global cap; the server limits requests and open connections per IP (relay and nginx).
- **Local network access is off by default**: the computer is only reachable through your server.
  *Settings → Allow direct connections from the local network* turns it on (the desktop app is
  encrypted there too; the plain-http browser page on the local network is not).
- Releases include `SHA256SUMS.txt`; the VPS setup script verifies the server program against it.
- VPS hardening (automatic security updates, fail2ban, key-only SSH):
  ```bash
  curl -sL https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/harden-vps.sh | sudo bash
  ```

More: [SECURITY.md](SECURITY.md).

## Troubleshooting

| Problem | Fix |
|---|---|
| “Cannot reach server” in the app | Check Settings → Server (`IP:33202`) and that port 33202/TCP is open on the VPS. |
| “only accepts computers with its server key” | Enter the key printed by the VPS setup in Settings → Server key. |
| Phone shows “ID not found” | The shared computer must be running RemoteFriend and show “Online”. |
| Black screen on Wayland | Click **Ask again** in the app and allow screen sharing. |
| Mouse/keyboard do nothing on Wayland | “Allow Remote Interaction” was off: click **Ask again** and turn it on. wlroots desktops (Sway, Hyprland) can only control XWayland windows. |
| Black screen / no control on macOS | Grant Screen Recording and Accessibility, then restart RemoteFriend. |
| Browser video is slow (“JPEG mode”) | Open the page over **HTTPS** (your server's address) in a current Chrome, Edge or Safari. |
| VPS service fails with `Permission denied (os error 13)` | Run the VPS setup command again. See [SETUP_VPS.md](deploy/SETUP_VPS.md#troubleshooting). |
| “version mismatch” | Update the app on both computers and run the VPS setup again. |
| No sound on the phone | Tap the 🔊 button (it shows 🔇 when off) and turn up the phone volume; on iPhone, also turn off silent mode. The VPS must run v0.8.0 or newer. |
| No sound from a Mac | Sound needs macOS 14.6 or newer; allow **System Audio Recording** for RemoteFriend in System Settings → Privacy & Security. |

## Advanced

**Terminal host** (servers, SSH, scripts): `remote-friend-host` is installed next to the app
(`/usr/bin/remote-friend-host` on Linux). It prints the ID and password and asks for approval in
the terminal (`A` allow, `P` allow permanently, `N` deny). Options: `--new-password`,
`--forget-devices`. Starter scripts that download and run it: `start_remotefriend_linux.sh`,
`start_remotefriend_macos.command`, `start_remotefriend_win64.bat`.

Environment variables: `RF_RV_SERVER` (relay `host:port`), `RF_WEB_URL`, `RF_QUALITY=fast|balanced|sharp`,
`RF_FPS` (5–60), `RF_MAX_WIDTH`, `RF_BITRATE_BPS`, `REMOTE_FRIEND_PASS` (own password),
`REMOTE_FRIEND_AUTO_ACCEPT=1` (no approval at all), `RF_EPHEMERAL_PASSWORD=1`,
`REMOTE_FRIEND_DIR` (where received files go; default `Downloads/RemoteFriend`).

**Build from source**

```bash
# Linux needs: libpipewire-0.3-dev libspa-0.2-dev clang libclang-dev libxkbcommon-dev libgtk-3-dev
cargo build --release -p remotefriend -p remote-friend-host -p remote-friend-rendezvous
cargo test -p remote-friend-common -p remote-friend-host
```

For development: `RF_TEST_PATTERN=1` (synthetic screen, no permissions needed), `RF_INPUT_DRY=1`
(log input instead of applying it), `RF_NATIVE_PORT` / `RF_HTTP_PORT` (run a second instance).

Protocol version 4 (end-to-end encrypted): the app, terminal host and relay server must be updated together.
