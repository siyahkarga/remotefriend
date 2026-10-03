# VPS setup (Ubuntu/Debian)

> Since v0.7.0 connections are end-to-end encrypted: the relay only forwards ciphertext and never sees
> the password, screen, input or files. It does see metadata (IDs, times, IP addresses, data volume).
> Keep the app, the computers and the relay on the same version (v0.8.0 = protocol 5; the phone page with sound comes from the relay).

## One command (install and update)

```bash
curl -sL https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/setup-vps.sh | sudo bash
```

The script does the following (it is safe to re-run):

1. Installs `nginx`, `certbot` and `ufw`; creates the `remotefriend` system user.
2. Downloads the latest `remote-friend-rendezvous` binary, **checks it against the release's
   `SHA256SUMS.txt`** (refuses a mismatch), then stops the service and replaces it atomically.
3. **Keeps** the relay TLS certificate (`/opt/remotefriend/cert.pem`, port 33202); generates one if missing.
4. Repairs file permissions on every run.
5. Installs the systemd service. The service receives the certificate via `LoadCredential=`: systemd
   reads the file as root and hands it to the service, so file permissions can't take the service down.
6. Sets up nginx and obtains **Let's Encrypt HTTPS**. If you have no domain, `<ip-with-dashes>.sslip.io`
   is used (e.g. `169-58-37-61.sslip.io`, no setup needed). The certificate renews automatically.
7. Creates the **server key** (`/opt/remotefriend/register.key`, kept on re-runs): only computers that
   know it can register on this server, so strangers cannot use your VPS as a relay.
8. Adds nginx request and connection limits per IP (the relay itself also limits connections per IP).
9. Opens 80, 443 and 33202 in the firewall; at the end it prints the **Server**, **Server key**,
   **Web address** and the certificate fingerprint.

Options:

```bash
# your own domain (its A record must point to this VPS)
curl -sL .../setup-vps.sh | sudo DOMAIN=remote.example.com EMAIL=you@example.com bash
# if the IP can't be detected automatically
curl -sL .../setup-vps.sh | sudo SERVER_IP=1.2.3.4 bash
# specific release
curl -sL .../setup-vps.sh | sudo VERSION=v0.8.0 bash
```

Your cloud provider's security group must also allow **80, 443 and 33202/TCP** (80 is needed to obtain the certificate).

## Hardening (recommended)

```bash
curl -sL https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/harden-vps.sh | sudo bash
```

Turns on automatic security updates and fail2ban for SSH, and switches SSH to key-only login if an SSH
key is already installed (otherwise it prints how to install one and leaves password login on, so you
cannot lock yourself out). It does not touch the firewall rules of other sites on the VPS.

## Computer setup

In the RemoteFriend app open **Settings** and enter **Server** (`VPS_IP:33202`), **Server key** and
**Web address** exactly as printed at the end of the setup, then *Save and reconnect*. On the first
connection the app asks you to confirm the relay fingerprint; accept it only if it matches the one
printed by the setup. The terminal host uses the key saved by the app, or `RF_REGISTER_KEY=...` in its environment.
On your phone, open the `https://…` address printed by the setup script.

Show the key again later: `sudo cat /opt/remotefriend/register.key`.

## Troubleshooting

### Service keeps restarting with `Permission denied (os error 13)`

Cause: the service user (`remotefriend`) can't read the TLS key or the registry file
(e.g. the certificate was regenerated/copied by hand and left owned by `root` with mode `600`).

The easiest fix is to re-run the setup script. To fix it by hand, run **each command on its own line**:

```bash
sudo curl -fsSL https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/remotefriend.service -o /etc/systemd/system/remotefriend.service
sudo chown root:remotefriend /opt/remotefriend /opt/remotefriend/cert.pem /opt/remotefriend/key.pem
sudo chmod 750 /opt/remotefriend
sudo chmod 640 /opt/remotefriend/key.pem
sudo chown -R remotefriend:remotefriend /var/lib/remotefriend
sudo chmod 700 /var/lib/remotefriend
sudo systemctl daemon-reload
sudo systemctl restart remotefriend
sudo journalctl -u remotefriend -n 30 --no-pager
```

To see which file can't be read, look at the **whole** log (`-n 30`); above the error line you'll see
`could not open certificate: …`, `could not open key: …` or `could not read host registry file: …`.

### HTTPS could not be obtained

Check `/tmp/rf-certbot.log`. The most common cause: port 80 is closed in the cloud security group,
or another web server is using port 80. Fix it and re-run the script.

### "this server only accepts computers with its server key"

The computer is new to this server and has no (or a wrong) server key. Enter the key from
`sudo cat /opt/remotefriend/register.key` in Settings → Server key.

### "this ID is registered to a different host key"

The host's `~/.config/remotefriend/host_secret` file changed or was deleted. Remove the old entry:

```bash
sudo systemctl stop remotefriend
sudo nano /var/lib/remotefriend/hosts.json   # delete the line with the 9-digit ID in question
sudo systemctl start remotefriend
```

## Maintenance

- Keep `/var/lib/remotefriend/hosts.json` private and back it up (it contains host identity secrets).
- To revoke the server key: write a new one to `/opt/remotefriend/register.key` (`openssl rand -hex 16`),
  `sudo systemctl restart remotefriend`, and remove unwanted IDs from `hosts.json`.
- If the relay certificate changes, hosts will ask for the fingerprint again; verify the new fingerprint over a trusted channel.
- Do not use `RF_PLAIN_OK=1` on a production server.
