#!/usr/bin/env bash
# RemoteFriend VPS setup/update (single command, safe to re-run).
#   curl -sL https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/setup-vps.sh | sudo bash
#
# What it does: service user, binary, TLS certificate (33202, preserved), systemd service,
# nginx + automatic HTTPS (Let's Encrypt), firewall, permission repair.
#
# Options (environment variables):
#   SERVER_IP=1.2.3.4           if the public IP can't be detected automatically
#   DOMAIN=remote.example.com   your own domain (otherwise <ip>.sslip.io is used, no setup needed)
#   EMAIL=you@example.com       for Let's Encrypt notifications (optional)
#   NO_TLS=1                    skip HTTPS (not recommended: slow video on phones, password sent in the clear)
#   VERSION=v0.6.2              specific release
set -euo pipefail

REPO_RAW="https://raw.githubusercontent.com/siyahkarga/remotefriend/main"
REPO="https://github.com/siyahkarga/remotefriend"
INSTALL_DIR="/opt/remotefriend"
DATA_DIR="/var/lib/remotefriend"
SERVICE_FILE="/etc/systemd/system/remotefriend.service"
NGINX_SITE="/etc/nginx/sites-enabled/remotefriend"
ACME_ROOT="/var/www/remotefriend-acme"

if [ "$(id -u)" -ne 0 ]; then
  echo "Root required: run with sudo." >&2
  exit 1
fi

VERSION="${VERSION:-$(curl -s --max-time 10 https://api.github.com/repos/siyahkarga/remotefriend/releases/latest | grep '"tag_name"' | cut -d'"' -f4 || true)}"
if [ -z "$VERSION" ]; then
  echo "Could not find the latest release (GitHub reachable?). Run with VERSION=vX.Y.Z." >&2
  exit 1
fi
SERVER_IP="${SERVER_IP:-$(curl -s -4 --max-time 10 https://ifconfig.me 2>/dev/null || true)}"
if [ -z "$SERVER_IP" ]; then
  echo "Could not detect the public IP. Run it like this: curl -sL ... | sudo SERVER_IP=x.x.x.x bash" >&2
  exit 1
fi
DOMAIN="${DOMAIN:-${SERVER_IP//./-}.sslip.io}"

echo "=== 1/7 packages ==="
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq nginx curl openssl ufw ca-certificates > /dev/null
if [ "${NO_TLS:-0}" != "1" ]; then
  apt-get install -y -qq certbot > /dev/null
fi

echo "=== 2/7 user and directories ==="
id remotefriend >/dev/null 2>&1 || useradd --system --home "$DATA_DIR" --shell /usr/sbin/nologin remotefriend
install -d -o root -g remotefriend -m 0750 "$INSTALL_DIR"
install -d -o remotefriend -g remotefriend -m 0700 "$DATA_DIR"
# Repair broken permissions left over from manual changes.
chown -R remotefriend:remotefriend "$DATA_DIR"
chmod 0700 "$DATA_DIR"

# Don't leave the relay stopped if a step fails (or the script exits early).
trap 'systemctl is-active --quiet remotefriend 2>/dev/null || systemctl start remotefriend 2>/dev/null || true' EXIT

echo "=== 3/7 binary ($VERSION) ==="
TMP_BIN="$(mktemp "$INSTALL_DIR/.rv.XXXXXX")"
if ! curl -fsSL --retry 3 --max-time 600 "$REPO/releases/download/$VERSION/remote-friend-rendezvous" -o "$TMP_BIN"; then
  rm -f "$TMP_BIN"
  echo "DOWNLOAD ERROR: check the release name ($VERSION) or internet access." >&2
  exit 1
fi
if [ "$(stat -c%s "$TMP_BIN")" -lt 1000000 ]; then
  rm -f "$TMP_BIN"
  echo "DOWNLOAD ERROR: file is too small (corrupt download)." >&2
  exit 1
fi
# Verify against the release checksums (SHA256SUMS.txt is published with every release).
SUMS="$(curl -fsSL --retry 3 --max-time 60 "$REPO/releases/download/$VERSION/SHA256SUMS.txt" 2>/dev/null || true)"
if [ -n "$SUMS" ]; then
  WANT="$(printf '%s\n' "$SUMS" | awk '$2=="remote-friend-rendezvous"{print $1}')"
  GOT="$(sha256sum "$TMP_BIN" | awk '{print $1}')"
  if [ -z "$WANT" ] || [ "$WANT" != "$GOT" ]; then
    rm -f "$TMP_BIN"
    echo "CHECKSUM ERROR: the downloaded server program does not match SHA256SUMS.txt; not installed." >&2
    exit 1
  fi
  echo "Checksum verified."
else
  echo "WARNING: this release has no SHA256SUMS.txt; checksum not verified."
fi
chmod 0755 "$TMP_BIN"
chown root:remotefriend "$TMP_BIN"
systemctl stop remotefriend 2>/dev/null || true
mv -f "$TMP_BIN" "$INSTALL_DIR/remote-friend-rendezvous"

echo "=== 4/7 relay TLS certificate (kept if present) ==="
if [ -f "$INSTALL_DIR/cert.pem" ] && [ -f "$INSTALL_DIR/key.pem" ]; then
  echo "Existing certificate kept (the fingerprint on your hosts stays valid)."
else
  openssl req -x509 -newkey rsa:3072 -sha256 \
    -keyout "$INSTALL_DIR/key.pem" -out "$INSTALL_DIR/cert.pem" \
    -days 3650 -nodes -subj "/CN=remotefriend-relay" 2>/dev/null
  echo "New certificate generated."
fi
chown root:remotefriend "$INSTALL_DIR/key.pem" "$INSTALL_DIR/cert.pem"
chmod 0640 "$INSTALL_DIR/key.pem"
chmod 0644 "$INSTALL_DIR/cert.pem"

# Server key: only computers that know it can register on this server (kept if present).
if [ ! -s "$INSTALL_DIR/register.key" ]; then
  openssl rand -hex 16 > "$INSTALL_DIR/register.key"
  echo "New server key generated."
fi
chown root:remotefriend "$INSTALL_DIR/register.key"
chmod 0640 "$INSTALL_DIR/register.key"
SERVER_KEY="$(tr -d '[:space:]' < "$INSTALL_DIR/register.key")"

echo "=== 5/7 service ==="
# Internal web port (127.0.0.1 only, behind nginx). If another application uses it
# (e.g. another site on the same VPS on 8080), the next free port is chosen.
port_busy() { ss -ltnH "sport = :$1" 2>/dev/null | grep -q .; }
WEB_PORT="${WEB_PORT:-33203}"
while port_busy "$WEB_PORT"; do
  echo "port $WEB_PORT is used by another application; trying the next one"
  WEB_PORT=$((WEB_PORT + 1))
  [ "$WEB_PORT" -gt 33299 ] && { echo "no free local port found" >&2; exit 1; }
done
echo "internal web port: 127.0.0.1:$WEB_PORT"
TMP_UNIT="$(mktemp)"
curl -fsSL "$REPO_RAW/deploy/remotefriend.service" -o "$TMP_UNIT"
grep -q "ExecStart=/opt/remotefriend/remote-friend-rendezvous" "$TMP_UNIT" || { echo "service file download is corrupt" >&2; exit 1; }
sed -i "s/^Environment=RF_WEB_PORT=.*/Environment=RF_WEB_PORT=$WEB_PORT/" "$TMP_UNIT"
install -m 0644 "$TMP_UNIT" "$SERVICE_FILE"
rm -f "$TMP_UNIT"
systemctl daemon-reload
systemctl enable remotefriend >/dev/null 2>&1
systemctl restart remotefriend
sleep 2

echo "=== 6/7 nginx + HTTPS ($DOMAIN) ==="
install -d -m 0755 "$ACME_ROOT"
proxy_block() {
  echo "  location /.well-known/acme-challenge/ { root $ACME_ROOT; }"
  echo "  location / {"
  echo "    proxy_pass http://127.0.0.1:$WEB_PORT;"
  cat <<'EOF'
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 3600s;
    proxy_send_timeout 3600s;
    proxy_buffering off;
    limit_req zone=rf_req burst=40 nodelay;
    limit_conn rf_conn 20;
  }
EOF
}
# Request/connection limits per client IP (protects the server from floods).
cat > /etc/nginx/conf.d/remotefriend-limits.conf <<'EOF'
limit_req_zone $binary_remote_addr zone=rf_req:10m rate=10r/s;
limit_conn_zone $binary_remote_addr zone=rf_conn:10m;
EOF
write_http_site() {
  { echo "server {"; echo "  listen 80;"; echo "  server_name $DOMAIN $SERVER_IP;"; proxy_block; echo "}"; } > "$NGINX_SITE"
}
write_https_site() {
  local live="/etc/letsencrypt/live/$DOMAIN"
  {
    echo "server {"
    echo "  listen 80;"
    echo "  server_name $DOMAIN $SERVER_IP;"
    echo "  location /.well-known/acme-challenge/ { root $ACME_ROOT; }"
    echo "  location / { return 301 https://$DOMAIN\$request_uri; }"
    echo "}"
    echo "server {"
    echo "  listen 443 ssl;"
    echo "  server_name $DOMAIN;"
    echo "  ssl_certificate $live/fullchain.pem;"
    echo "  ssl_certificate_key $live/privkey.pem;"
    echo "  ssl_protocols TLSv1.2 TLSv1.3;"
    echo "  add_header Strict-Transport-Security \"max-age=31536000\" always;"
    proxy_block
    echo "}"
  } > "$NGINX_SITE"
}

WEB_URL="http://$SERVER_IP"
LIVE_CERT="/etc/letsencrypt/live/$DOMAIN/fullchain.pem"
# If HTTPS was set up before (HSTS was sent), never fall back to plain HTTP.
if [ -f "$LIVE_CERT" ]; then write_https_site; else write_http_site; fi
nginx -t >/dev/null 2>&1 && systemctl reload nginx
ufw allow 80/tcp >/dev/null 2>&1 || true
ufw allow 443/tcp >/dev/null 2>&1 || true
ufw allow 33202/tcp >/dev/null 2>&1 || true
if [ "${NO_TLS:-0}" != "1" ]; then
  EMAIL_ARGS=(--register-unsafely-without-email)
  [ -n "${EMAIL:-}" ] && EMAIL_ARGS=(-m "$EMAIL")
  if certbot certonly --webroot -w "$ACME_ROOT" -d "$DOMAIN" -n --agree-tos "${EMAIL_ARGS[@]}" \
       --keep-until-expiring --deploy-hook "systemctl reload nginx" >/tmp/rf-certbot.log 2>&1; then
    write_https_site
    WEB_URL="https://$DOMAIN"
    echo "HTTPS ready: $WEB_URL"
  elif [ -f "$LIVE_CERT" ]; then
    WEB_URL="https://$DOMAIN"
    echo "WARNING: could not renew the certificate (/tmp/rf-certbot.log); HTTPS continues with the existing certificate."
  else
    echo "WARNING: could not obtain an HTTPS certificate (is port 80 reachable from outside?). Details: /tmp/rf-certbot.log"
    echo "         Continuing with plain HTTP for now; video on phones will be slow."
  fi
fi
nginx -t
systemctl reload nginx

echo "=== 7/7 status ==="
if systemctl is-active --quiet remotefriend; then
  echo "service: RUNNING"
else
  echo "service: ERROR"
  journalctl -u remotefriend -n 40 --no-pager
  exit 1
fi
# Verify the page really is RemoteFriend (checks for port conflicts / wrong site).
if curl -s --max-time 5 "http://127.0.0.1:$WEB_PORT/info" | grep -q '"role":"relay"'; then
  echo "web (127.0.0.1:$WEB_PORT): RemoteFriend OK"
else
  echo "ERROR: 127.0.0.1:$WEB_PORT is not responding as RemoteFriend."
  journalctl -u remotefriend -n 30 --no-pager
  exit 1
fi
if [ "${WEB_URL#https://}" != "$WEB_URL" ]; then
  if curl -s --max-time 10 "$WEB_URL/info" | grep -q '"role":"relay"'; then
    echo "external ($WEB_URL): RemoteFriend OK"
  else
    echo "WARNING: $WEB_URL does not show RemoteFriend from outside (port 443 may be served by another server)."
  fi
fi
FP="$(openssl x509 -in "$INSTALL_DIR/cert.pem" -outform der | sha256sum | awk '{print $1}')"
cat <<EOF

=============================================================
 Setup complete.

 From your phone or browser:   $WEB_URL
   (sign in with computer ID + password)

 In the RemoteFriend app on the computer to share
 (Settings -> Server):
   Server       : $SERVER_IP:33202
   Server key   : $SERVER_KEY
   Web address  : $WEB_URL
 Only computers with this server key can use your server.

 On first connection the app asks to trust this fingerprint; it must match:
   $FP
=============================================================
EOF
