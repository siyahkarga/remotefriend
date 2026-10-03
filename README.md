# RemoteFriend

Remote desktop for your own computers. Share a computer with one click and control it from
another computer or **any phone browser** — no app needed on the phone. Video is H.264,
mouse, keyboard, touch and file transfer are supported, and you can run your own relay server.

| Shared computer | Status |
|---|---|
| **Windows** 10/11 | ✔ screen + mouse/keyboard (high-DPI displays included) |
| **macOS** 11+ (Apple Silicon and Intel) | ✔ needs Screen Recording + Accessibility permission |
| **Linux Wayland** (GNOME, KDE) | ✔ via the desktop's screen-sharing portal (asked once) |
| **Linux X11** | ✔ |

## Install

Download for your system from the [latest release](https://github.com/siyahkarga/remotefriend/releases/latest):

| System | Download | Install |
|---|---|---|
| Windows | [RemoteFriend-Setup.exe](https://github.com/siyahkarga/remotefriend/releases/latest/download/RemoteFriend-Setup.exe) | Run it. If SmartScreen appears: **More info → Run anyway**. |
| macOS | [RemoteFriend-macOS.dmg](https://github.com/siyahkarga/remotefriend/releases/latest/download/RemoteFriend-macOS.dmg) | Drag RemoteFriend to Applications. First start: **right-click → Open**. |
| Ubuntu / Debian | [RemoteFriend-linux-amd64.deb](https://github.com/siyahkarga/remotefriend/releases/latest/download/RemoteFriend-linux-amd64.deb) | Double-click it, or `sudo apt install ./RemoteFriend-linux-amd64.deb` |
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
printed address. The first connection asks you to confirm the fingerprint. Details:
[deploy/SETUP_VPS.md](deploy/SETUP_VPS.md).

## Phone controls

| Mode | Gestures |
|---|---|
| **Touchpad** (default) | Drag: move the pointer · Tap: click · Two-finger tap: right click · Two-finger drag: scroll · Press and hold, then drag: drag & drop |
| **Touchscreen** | Tap where you want to click · Press and hold: right click · Drag: drag & drop · Two fingers: scroll / pan when zoomed |

Pinch to zoom in both modes. The toolbar has the phone keyboard, special keys (Ctrl, Alt, Win,
Esc, arrows, F-keys, copy/paste shortcuts, type the clipboard), quality (Fast / Balanced /
Sharp), file upload and full screen. If the connection drops briefly, the page reconnects by itself.

## Security

- Every connection needs the **password**. New devices also need **approval on the shared
  computer**, unless they were allowed with *Always allow* (the computer stores only a hash of
  that device token).
- The password is created once and kept (format `abcde-23456`, ~50 bits; case and dashes don't
  matter). Settings → **New password** replaces it.
- Wrong passwords are rate limited per source (5 per minute, then 1/2/4/8/16-minute lockouts)
  plus a global cap; the relay also limits requests per IP.
- Internet traffic is encrypted (browser ↔ server HTTPS, computer ↔ server TLS with a pinned
  certificate). The relay is **not end-to-end encrypted**: whoever runs the server can technically
  see the traffic — use your own server.
- The local network path (ports 33200/33201) is not encrypted; don't forward these ports to the internet.

More: [SECURITY.md](SECURITY.md).

## Troubleshooting

| Problem | Fix |
|---|---|
| “Cannot reach server” in the app | Check Settings → Server (`IP:33202`) and that port 33202/TCP is open on the VPS. |
| Phone shows “ID not found” | The shared computer must be running RemoteFriend and show “Online”. |
| Black screen on Wayland | Click **Ask again** in the app and allow screen sharing. |
| Mouse/keyboard do nothing on Wayland | “Allow Remote Interaction” was off: click **Ask again** and turn it on. wlroots desktops (Sway, Hyprland) can only control XWayland windows. |
| Black screen / no control on macOS | Grant Screen Recording and Accessibility, then restart RemoteFriend. |
| Browser video is slow (“JPEG mode”) | Open the page over **HTTPS** (your server's address) in a current Chrome, Edge or Safari. |
| VPS service fails with `Permission denied (os error 13)` | Run the VPS setup command again. See [SETUP_VPS.md](deploy/SETUP_VPS.md#troubleshooting). |
| “version mismatch” | Update the app on both computers and run the VPS setup again. |

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

Protocol version 3: the app, terminal host and relay server must be updated together.
