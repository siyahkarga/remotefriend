# RemoteFriend

Remote desktop for your own computers. Share a computer with one click and control it from
another computer or **any phone browser** — no app needed on the phone. Video is H.264,
the computer's **sound** is streamed too (Opus), mouse, keyboard, touch and file transfer are
supported, and every session is end-to-end encrypted. Free software under the
[GPL-3.0](LICENSE); made by [blobidea](https://blobidea.com/products/remotefriend).

| Shared computer | Status |
|---|---|
| **Windows** 10/11 | ✔ screen + mouse/keyboard (high-DPI displays included) |
| **macOS** 11+ (Apple Silicon and Intel) | ✔ needs Screen Recording + Accessibility permission (sound: macOS 14.6+) |
| **Linux Wayland** (GNOME, KDE) | ✔ via the desktop's screen-sharing portal (asked once) |
| **Linux X11** | ✔ |

## Install

Download for your system from the [RemoteFriend page on blobidea.com](https://blobidea.com/products/remotefriend):

| System | Download | Install |
|---|---|---|
| Windows | [RemoteFriend-Setup.exe](https://blobidea.com/downloads/remotefriend/RemoteFriend-Setup.exe) | Run it. If SmartScreen appears: **More info → Run anyway**. |
| macOS | [RemoteFriend-macOS.dmg](https://blobidea.com/downloads/remotefriend/RemoteFriend-macOS.dmg) | Drag RemoteFriend to Applications. If macOS refuses to open it: *System Settings → Privacy & Security* → **Open Anyway** (macOS 14 and older: right-click → Open). |
| Ubuntu 24.04+ / Debian 13+ | [RemoteFriend-linux-amd64.deb](https://blobidea.com/downloads/remotefriend/RemoteFriend-linux-amd64.deb) | Double-click it, or `sudo apt install ./RemoteFriend-linux-amd64.deb` |
| Other Linux | [RemoteFriend-linux-x86_64.tar.gz](https://blobidea.com/downloads/remotefriend/RemoteFriend-linux-x86_64.tar.gz) | Extract, then run `./install.sh` (no root needed). |

Then open **RemoteFriend** from your applications menu. The installers are not code-signed
yet, which is why Windows and macOS ask for confirmation once. RemoteFriend tells you when a
new version is out (*Settings → General* turns the check off).

## Use it

1. Open RemoteFriend on the computer you want to share. The left side shows **Your ID** and
   **Password**. Keep the app open while you want remote access.
2. On the other device:
   - **Computer:** open RemoteFriend, type the ID (or the local IP address) and password,
     click **Connect**.
   - **Phone / any browser:** open the address shown next to 📱 in RemoteFriend
     (`https://remote.blobidea.com`), type the ID and password.
3. On the shared computer, a window asks: **Allow**, **Always allow this device** or **Deny**.
   With *Always allow*, that phone, browser or computer connects from then on **without the
   password and without asking** — no one needs to be at the computer. Recent computers that
   trust this device show “✔ connects without password”.

The **password changes each time RemoteFriend starts** (*Settings → Security*: after every
session, or never). The app and the phone page remember the password for a computer until it
changes, so a dropped connection or a reload doesn't ask again. Trusted devices don't need it.

## On the shared computer

While someone is connected, the RemoteFriend window shows:

- **Shared screen**: a live preview of what viewers see, and buttons to switch to another
  monitor at any time (viewers can switch too, if they may control the computer). On Wayland,
  pick all screens you want to offer in the desktop's sharing dialog (*Choose screens…*).
- **Connected now**: every viewer with its device name, since when, and checkboxes for what it
  may do — **mouse & keyboard, sound, files, clipboard** — plus **Disconnect** and, for another
  RemoteFriend app, **Switch sides** (you control their computer instead; they are asked first).
- **✋ Take back control**: stops mouse and keyboard of every viewer until you allow it again.
- **Recent connections** and **Trusted devices** (remove one to require password and approval again).

**What viewers see** (choose in the window): **Safe view** (default) hides RemoteFriend itself —
the window is minimized and, on Windows/macOS, left out of the picture — and blacks out private
apps such as password managers (list in *Settings → Private apps*; needs window positions, so
on Wayland only RemoteFriend is hidden). **Everything (full control)** shows the screen exactly
as you see it. Apps that protect themselves from every screen capture (e.g. Signal's “Screen
security”) stay black in both modes until that option is turned off in the app. Closing
RemoteFriend asks first, because the computer is unreachable while it is closed.

## Direct connections (peer-to-peer)

Every session starts through the server. The viewer then offers a direct WebRTC connection
inside the encrypted session; if the two devices can reach each other (same network, or most
home routers), picture, sound, input and files go straight between them — still end-to-end
encrypted, with separate keys — and the server only keeps a small heartbeat. Where that is
impossible (some mobile networks), everything stays on the server. The toolbar shows **Direct**
or **Via server**. The devices learn each other's internet address; *Settings → Direct
connections* turns this off. The server answers STUN on UDP 3478 so devices can find their
public address.

## The desktop viewer

Toolbar: Disconnect, sound on/off, **Keys** (Ctrl+Alt+Del, Win/⌘, Alt+Tab, Alt+F4, Task
Manager, lock, Print Screen…), quality, **Screen** (when the computer has several), full screen,
**Files** and **Switch sides**. *Files* sends files (progress, Cancel; the computer's Downloads/
RemoteFriend folder, and you are told the exact path) and browses the remote computer's folders
to download files (progress, Cancel, saved to your Downloads/RemoteFriend). The phone page has
the same file panel.
Copy and paste work both ways: Ctrl+C on the remote computer puts the text on your clipboard,
Ctrl+V pastes your clipboard there (Cmd and Ctrl are translated between Mac and PC).

First start on **Linux Wayland**: the desktop asks to share the screen. Turn **ON “Allow Remote
Interaction”** and click **Share**. It is remembered. First start on **macOS**: allow
RemoteFriend under *System Settings → Privacy & Security → Screen Recording* and
*Accessibility*, then restart the app.

## Reachable from anywhere: the server and access keys

Within the same network everything works without a server. Over the internet, devices meet at
the RemoteFriend relay run by blobidea (`remote.blobidea.com`; its certificate is built into the
app). The relay only forwards encrypted data and is not part of this repository.

- **Connecting to** another computer needs nothing but its ID and password.
- **Making a computer reachable** needs an **access key**: enter it once in
  *Settings → Server → Access key*. Keys come from the server owner; each key has its own
  computer limit and can expire or be revoked. Without a key the computer still works on the
  local network and can still connect to others.
- *Settings → Server* can also point RemoteFriend at another relay, or turn the server off
  (local network only).

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
- **Access keys.** Only computers with a valid access key can register on the relay; keys are stored
  there only as hashes, and a revoked or expired key takes its computers offline within seconds.
- New devices need the **password** and **approval on the shared computer**. Devices allowed with
  *Always allow* get their own 256-bit device key and log in end-to-end encrypted without the
  password; the computer can remove them one by one.
- The password (format `abcde-23456`, ~50 bits; case and dashes don't matter) changes at every
  start of RemoteFriend by default (or after every session, or never — *Settings → Security*).
- The computer's user sees who is connected, can disconnect them, take back control, and turn
  mouse/keyboard, sound, files and clipboard on or off per viewer.
- Wrong passwords are rate limited per source (5 per minute, then 1/2/4/8/16-minute lockouts) plus a
  global cap; the server limits requests and open connections per IP (relay and nginx).
- **Local network access is off by default**: the computer is only reachable through the server.
  *Settings → Allow direct connections from the local network* turns it on (the desktop app is
  encrypted there too; the plain-http browser page on the local network is not).
- Every release comes with `SHA256SUMS.txt` on the download page.
- **Found a security problem?** Please write to **support@blobidea.com** (subject “RemoteFriend
  security”) instead of opening a public issue. Details: [SECURITY.md](SECURITY.md).

## Troubleshooting

| Problem | Fix |
|---|---|
| “Cannot reach server” in the app | Check your internet connection and Settings → Server (empty = the default server). |
| “No access key” / “this server needs an access key” | Enter the access key you got in Settings → Server → Access key. Connecting to other computers works without one. |
| Phone shows “ID not found” | The shared computer must be running RemoteFriend and show “Online”. |
| Black screen on Wayland | Click **Ask again** in the app and allow screen sharing. |
| Mouse/keyboard do nothing on Wayland | “Allow Remote Interaction” was off: click **Ask again** and turn it on. wlroots desktops (Sway, Hyprland) can only control XWayland windows. |
| Black screen / no control on macOS | Grant Screen Recording and Accessibility, then restart RemoteFriend. |
| Browser video is slow (“JPEG mode”) | Open the page over **HTTPS** (`https://remote.blobidea.com`) in a current Chrome, Edge or Safari. |
| “version mismatch” | Update the app on both computers. |
| “This device is no longer trusted” | The computer removed this device (or forgot all devices): enter the current password once. |
| A computer drops off (“ID not found”) after sleep or a network change | Dead links are noticed within about a minute and the computer reconnects; update the app if it is older than v0.9. |
| No sound on the phone | Tap the 🔊 button (it shows 🔇 when off) and turn up the phone volume; on iPhone, also turn off silent mode. |
| No sound from a Mac | Sound needs macOS 14.6 or newer; allow **System Audio Recording** for RemoteFriend in System Settings → Privacy & Security. |

## Advanced

**Terminal host** (servers, SSH, scripts): `remote-friend-host` is installed next to the app
(`/usr/bin/remote-friend-host` on Linux). It prints the ID and password and asks for approval in
the terminal (`A` allow, `P` allow permanently, `N` deny). Options: `--new-password`,
`--forget-devices`. Starter scripts that download and run it: `start_remotefriend_linux.sh`,
`start_remotefriend_macos.command`, `start_remotefriend_win64.bat`.

Environment variables: `RF_RV_SERVER` (relay `host:port`), `RF_RV_FP` (its certificate fingerprint),
`RF_REGISTER_KEY` (access key), `RF_WEB_URL`, `RF_QUALITY=fast|balanced|sharp`,
`RF_FPS` (5–60), `RF_MAX_WIDTH`, `RF_BITRATE_BPS`, `REMOTE_FRIEND_PASS` (own password),
`REMOTE_FRIEND_AUTO_ACCEPT=1` (no approval at all), `RF_EPHEMERAL_PASSWORD=1`,
`REMOTE_FRIEND_DIR` (where received files go; default `Downloads/RemoteFriend`).

**Build from source**

```bash
# Linux needs: libpipewire-0.3-dev libspa-0.2-dev clang libclang-dev libxkbcommon-dev libgtk-3-dev
cargo build --release -p remotefriend -p remote-friend-host
cargo test -p remote-friend-common -p remote-friend-host -p remotefriend
```

For development: `RF_TEST_PATTERN=1` (synthetic screen, no permissions needed), `RF_INPUT_DRY=1`
(log input instead of applying it), `RF_NATIVE_PORT` / `RF_HTTP_PORT` (run a second instance).

Protocol version 6: the app and the terminal host must be updated together (the phone page comes
from the relay, which blobidea keeps current).

## License

RemoteFriend (the app, the terminal host and the phone page) is free software: you can
redistribute it and/or modify it under the terms of the GNU General Public License as published
by the Free Software Foundation, either version 3 of the License, or (at your option) any later
version. It is distributed in the hope that it will be useful, but **without any warranty**;
without even the implied warranty of merchantability or fitness for a particular purpose. See
[LICENSE](LICENSE).
