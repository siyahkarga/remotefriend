# Windows setup

## Recommended: installer

Download and run `RemoteFriend-Setup.exe` from the latest release. It installs RemoteFriend with a few clicks;
this is the recommended way for most users.

## Alternative (advanced): prebuilt binaries + starter script

Put `remote-friend-host.exe`, `remote-friend-client.exe` and, if needed, `start_remotefriend_win64.bat` in the same folder.

Open the firewall only on the network profiles you need. In an administrator PowerShell:

```powershell
netsh advfirewall firewall add rule name="RemoteFriend Native LAN" dir=in action=allow protocol=TCP localport=33200 profile=private
netsh advfirewall firewall add rule name="RemoteFriend Web LAN" dir=in action=allow protocol=TCP localport=33201 profile=private
```

Do not forward ports `33200` and `33201` to the internet on your router.

Run `start_remotefriend_win64.bat` (it updates itself when a new release is out). If `REMOTE_FRIEND_PASS` is not set beforehand, the host shows a password like `abcde-23456`. Use that password on the client, then approve the connection on the host (**Allow** or **Always allow**).

From a phone or browser, the smoothest video comes from the VPS's HTTPS address (H.264). The LAN browser page (`http://IP:33201`) is plain HTTP, so it falls back to JPEG mode; for smooth video on the LAN, use `remote-friend-client.exe`.

## Quality

While connected, in the browser choose ⚙ → Fast / Balanced / Sharp. To change the default:

```bat
set RF_QUALITY=fast
start_remotefriend_win64.bat
```

If you use a fixed password, choose at least 16 random characters:

```bat
set REMOTE_FRIEND_PASS=your_strong_random_password_here
start_remotefriend_win64.bat
```

Be careful not to leave the real password in your command history or in a `.bat` file.
