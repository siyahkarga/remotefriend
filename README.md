# RemoteFriends

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

Download for your system from the [RemoteFriends page on blobidea.com](https://blobidea.com/products/remotefriend):

| System | Download | Install |
|---|---|---|
| Windows | [RemoteFriend-Setup.exe](https://blobidea.com/downloads/remotefriend/RemoteFriend-Setup.exe) | Run it. If SmartScreen appears: **More info → Run anyway**. |
| macOS | [RemoteFriend-macOS.dmg](https://blobidea.com/downloads/remotefriend/RemoteFriend-macOS.dmg) | Drag RemoteFriends to Applications. If macOS refuses to open it: *System Settings → Privacy & Security* → **Open Anyway** (macOS 14 and older: right-click → Open). |
| Ubuntu 24.04+ / Debian 13+ | [RemoteFriend-linux-amd64.deb](https://blobidea.com/downloads/remotefriend/RemoteFriend-linux-amd64.deb) | Double-click it, or `sudo apt install ./RemoteFriend-linux-amd64.deb` |
| Other Linux | [RemoteFriend-linux-x86_64.tar.gz](https://blobidea.com/downloads/remotefriend/RemoteFriend-linux-x86_64.tar.gz) | Extract, then run `./install.sh` (no root needed). |

Then open **RemoteFriends** from your applications menu. The installers are not code-signed
yet, which is why Windows and macOS ask for confirmation once. RemoteFriends updates itself:
when a new version is out it shows **Update now**, and — when nobody is connected — installs it
by itself after a 30-second notice (*Settings → General*). Updates are signed; anything that
does not match the signature is not installed. On Linux (.deb) the desktop asks for your password.

**Reachable from anywhere:** the first time RemoteFriends opens it asks for your email address and
then the 6-digit code it sends you — that's all; your computer can now be reached over the
internet. Free for 2 computers, for 1 year; on your second computer (or after reinstalling) sign
in with the same address. Connecting *to* other computers needs nothing, so *Skip for now* is fine
too (later: Home → *Sign in with email*).

## Use it

1. Open RemoteFriends on the computer you want to share. The left side shows **Your ID** and
   **Password**. Keep the app open while you want remote access.
2. On the other device:
   - **Computer:** open RemoteFriends, type the ID (or the local IP address) and password,
     click **Connect**.
   - **Phone / any browser:** open the **invite link** (copy it under *Invite link* in
     RemoteFriends, shown once the computer is online, and send it): it opens with the ID and
     password filled in and connects right away. They travel after “#”, which browsers never send
     to the server, and are removed from the address bar. Pasting the link into the desktop
     app's ID field fills in both fields as well.
3. On the shared computer, a window asks: **Allow**, **Always allow** or **Deny** (or **Block**).
   With *Always allow*, that phone, browser or computer becomes **trusted**: from then on it
   connects **without the password**, even after the password changed — the computer's user
   still confirms each connection (*Allow / Deny*). Recent computers that trust this device show
   “trusted”.

**Devices** (menu on the left) lists who may connect without the password:

| | Password | Asked every time |
|---|---|---|
| **My devices** — your own laptop, phone… | no | **no**: they connect even when nobody is at the computer |
| **Trusted** — people you share with often | no | yes |
| everyone else | yes | yes |
| **Blocked** | — | refused without asking |

A device becomes one of *My devices* only on this page (*Trusted → Make it my device*), never
from the question window, so nobody can talk you into it during a call.

The **password changes each time RemoteFriends starts** (*Settings → Security*: after every
session, or never); only devices connecting for the first time need it. The app and the phone
page remember the password for a computer until it changes.

## On the shared computer

While someone is connected, a small **session bar** stays on top of everything (even when the
window is minimized): how many are connected, for how long, which screen they see, and **Open**,
**Take back control** and **Disconnect**.

The RemoteFriends window shows:

- **Screens** (with several monitors): which one viewers see now (**Show** another one at any
  time), and a switch per screen: **on** = viewers with mouse & keyboard may switch to it by
  themselves, **off** = they can only ask, and you answer *Show* or *Keep the current screen*.
  Viewers see the list too, with the screens that need asking marked. On Wayland, pick all
  screens you want to offer in the desktop's sharing dialog (*Choose…*). A live preview shows
  what viewers see.
- **Connected now**: every viewer with its device name, since when, how far it is trusted, and
  buttons for what it may do — **Control** (mouse & keyboard), **Sound**, **Files**,
  **Clipboard** — plus **Disconnect** and, for another RemoteFriends app, **Switch sides** (you
  control their computer instead; they are asked first). Every file that came in or was taken
  is listed under the viewer, with where it is.
- **✋ Take back control**: stops mouse and keyboard of every viewer until you allow it again.
- **You come first:** when you move your mouse, viewers' mouse and keyboard wait until you have
  left it alone for 1.5 s (they are told). Windows, macOS and X11; Wayland doesn't tell apps
  where the pointer is.
- Viewers can never use RemoteFriends itself (their clicks and keys on its windows are ignored),
  in either view; only your own devices may.
- **History** and **Devices** in the menu on the left.

**What viewers see** (choose in the window): **Safe view** (default) keeps RemoteFriends itself
out of the picture — the window stays open for you, but viewers see it blacked out (Windows/macOS
also leave it out of the capture) — and blacks out
private apps such as password managers (*Settings → Privacy*: switch apps on in **Open apps** or
type words of their names). Windows are followed for every frame, so a moving window doesn't show
through. When only a browser tab is private (its title matches), the tab bar and address bar stay
visible so viewers can switch tabs. Linux **Wayland** desktops don't tell apps where windows are:
there private apps can't be blacked out (use an X11 session for that), and while someone watches
RemoteFriends shows only who is connected until you open it. **Everything** shows the screen exactly
as you see it. Apps that protect themselves from every screen capture (e.g. Signal's “Screen
security”) stay black in both modes until that option is turned off in the app. Closing
RemoteFriends asks first, because the computer is unreachable while it is closed.

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
**Files** and **Switch sides**. The file panel says what the computer lets you reach, then has
**Send to that computer** (with where the file goes) and **Get from that computer** (its folders;
downloads are saved to your Downloads/RemoteFriend). The phone page has the same file panel.

What a viewer reaches on the shared computer depends on how far it is trusted:

| | Sees and sends to | Asked |
|---|---|---|
| allowed once (password) | only the **shared folder**, Downloads/RemoteFriend | no |
| **Trusted** | the user's folders and data disks, **without system folders and hidden files** | once per session, at the first file action (*Allow for this session* / *Only the shared folder*) |
| **My devices** | everything | no |

Every path is checked on the shared computer (links that lead elsewhere included), and its user
sees every file that comes or goes.

If the shared computer closes RemoteFriends, viewers are told so and go back to the start; a
computer that stops answering for 15 s ends the session too.
Copy and paste work both ways: Ctrl+C on the remote computer puts the text on your clipboard,
Ctrl+V pastes your clipboard there (Cmd and Ctrl are translated between Mac and PC).

First start on **Linux Wayland**: the desktop asks to share the screen. Turn **ON “Allow Remote
Interaction”** and click **Share**. It is remembered. First start on **macOS**: allow
RemoteFriends under *System Settings → Privacy & Security → Screen Recording* and
*Accessibility*, then restart the app.

## Reachable from anywhere: the server and access keys

Within the same network everything works without a server. Over the internet, devices meet at
the RemoteFriends relay run by blobidea (built into the downloads from blobidea.com, with its
certificate pinned). The relay only forwards encrypted data and is not part of this repository;
builds from this source have no built-in server unless `RF_DEFAULT_SERVER`,
`RF_DEFAULT_SERVER_FP` and `RF_DEFAULT_WEB_URL` are set when compiling.

- **Connecting to** another computer needs nothing but its ID and password.
- **Making a computer reachable** needs an **access key**: enter it once in
  *Settings → Server → Access key*. Keys come from the server owner; each key has its own
  computer limit and can expire or be revoked. Without a key the computer still works on the
  local network and can still connect to others.
- *Settings → Server* can also point RemoteFriends at another relay, or turn the server off
  (local network only).
- The server's owner manages the keys in *Settings → Server → Manage access keys…* (create with
  Copy and Email buttons, revoke, delete); this needs the server's owner key.

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
  password; they are still asked about unless the computer's user made them one of *My devices*
  (only possible on the Devices page). Devices can be removed or blocked one by one; a blocked
  device is refused without a question, even with the right password.
- The password (format `abcde-23456`, ~50 bits; case and dashes don't matter) changes at every
  start of RemoteFriends by default (or after every session, or never — *Settings → Security*).
- The computer's user sees who is connected (also in the session bar on top), can disconnect them,
  take back control, and turn mouse/keyboard, sound, files and clipboard on or off per viewer.
- **Files by trust:** a viewer allowed once reaches only the shared folder; a trusted device its
  user's folders and data disks without system folders and hidden files, after a yes for that
  session; only your own devices reach everything. Viewers can't use RemoteFriends itself.
- Wrong passwords are rate limited per source (5 per minute, then 1/2/4/8/16-minute lockouts) plus a
  global cap; the server limits requests and open connections per IP (relay and nginx).
- **Local network access is off by default**: the computer is only reachable through the server.
  *Settings → Allow direct connections from the local network* turns it on (the desktop app is
  encrypted there too; the plain-http browser page on the local network is not).
- Every release comes with `SHA256SUMS.txt` on the download page.
- **Found a security problem?** Please write to **support@blobidea.com** (subject “RemoteFriends
  security”) instead of opening a public issue. Details: [SECURITY.md](SECURITY.md).

## Troubleshooting

| Problem | Fix |
|---|---|
| “Cannot reach server” in the app | Check your internet connection and Settings → Server (empty = the default server). |
| “No access key” / “this server needs an access key” | Enter the access key you got in Settings → Server → Access key. Connecting to other computers works without one. |
| Phone shows “ID not found” | The shared computer must be running RemoteFriends and show “Online”. |
| Black screen on Wayland | Click **Ask again** in the app and allow screen sharing. |
| Mouse/keyboard do nothing on Wayland | “Allow Remote Interaction” was off: click **Ask again** and turn it on. wlroots desktops (Sway, Hyprland) can only control XWayland windows. |
| Black screen / no control on macOS | Grant Screen Recording and Accessibility, then restart RemoteFriends. |
| Browser video is slow (“JPEG mode”) | Open the server's page over **HTTPS** (the 📱 address) in a current Chrome, Edge or Safari. |
| “version mismatch” | Update the app on both computers. |
| “This device is no longer trusted” | The computer removed this device (or forgot all devices): enter the current password once. |
| A computer drops off (“ID not found”) after sleep or a network change | Dead links are noticed within about a minute and the computer reconnects; update the app if it is older than v0.9. |
| No sound on the phone | Tap the 🔊 button (it shows 🔇 when off) and turn up the phone volume; on iPhone, also turn off silent mode. |
| No sound from a Mac | Sound needs macOS 14.6 or newer; allow **System Audio Recording** for RemoteFriends in System Settings → Privacy & Security. |

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

RemoteFriends (the app, the terminal host and the phone page) is free software: you can
redistribute it and/or modify it under the terms of the GNU General Public License as published
by the Free Software Foundation, either version 3 of the License, or (at your option) any later
version. It is distributed in the hope that it will be useful, but **without any warranty**;
without even the implied warranty of merchantability or fitness for a particular purpose. See
[LICENSE](LICENSE).
