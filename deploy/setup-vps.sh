#!/usr/bin/env bash
# RemoteFriend VPS setup/update (runs once, does everything).
# Usage: curl -sL https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/setup-vps.sh | sudo bash
# Existing certificate is kept if present (client fingerprints stay valid).
set -euo pipefail

VERSION="${VERSION:-$(curl -s --max-time 10 https://api.github.com/repos/siyahkarga/remotefriend/releases/latest | grep '"tag_name"' | cut -d'"' -f4)}"
VERSION="${VERSION:-v0.5.6}"
REPO="https://github.com/siyahkarga/remotefriend"
# Public IP of this VPS is auto-detected (no IP stored in repo).
# If detection fails: SERVER_IP=x.x.x.x curl ... | sudo bash
SERVER_IP="${SERVER_IP:-$(curl -s --max-time 10 https://ifconfig.me 2>/dev/null || true)}"
if [ -z "$SERVER_IP" ]; then
  echo "Public IP not detected. Run as:" >&2
  echo "  curl -sL ... | sudo SERVER_IP=x.x.x.x bash" >&2
  exit 1
fi
INSTALL_DIR="/opt/remotefriend"
DATA_DIR="/var/lib/remotefriend"
SERVICE_FILE="/etc/systemd/system/remotefriend.service"
NGINX_SITE="/etc/nginx/sites-enabled/remotefriend"

if [ "$(id -u)" -ne 0 ]; then
  echo "root required: run with sudo." >&2
  exit 1
fi

echo "=== 1/6 packages ==="
apt-get update -qq
apt-get install -y -qq nginx curl openssl ufw ca-certificates > /dev/null

echo "=== 2/6 user and directories ==="
useradd --system --home "$DATA_DIR" --shell /usr/sbin/nologin remotefriend 2>/dev/null || true
install -d -o root -g remotefriend -m 0750 "$INSTALL_DIR"
install -d -o remotefriend -g remotefriend -m 0700 "$DATA_DIR"

echo "=== 3/6 binary ($VERSION) ==="
echo "(70MB civari, birkaç dakika sürebilir...)"
curl -sSL --retry 3 --max-time 600 "$REPO/releases/download/$VERSION/remote-friend-rendezvous" -o "$INSTALL_DIR/remote-friend-rendezvous" \
  || { echo "DOWNLOAD FAILED: interneti veya release adını kontrol et." >&2; exit 1; }
if [ "$(stat -c%s "$INSTALL_DIR/remote-friend-rendezvous")" -lt 1000000 ]; then
  echo "DOWNLOAD FAILED: dosya çok küçük, indirme bozuk." >&2
  exit 1
fi
chmod +x "$INSTALL_DIR/remote-friend-rendezvous"
chown root:remotefriend "$INSTALL_DIR/remote-friend-rendezvous"

echo "=== 4/6 TLS certificate (kept if exists) ==="
if [ -f "$INSTALL_DIR/cert.pem" ] && [ -f "$INSTALL_DIR/key.pem" ]; then
  echo "Existing certificate kept."
else
  openssl req -x509 -newkey rsa:3072 \
    -keyout "$INSTALL_DIR/key.pem" -out "$INSTALL_DIR/cert.pem" \
    -days 825 -nodes -subj "/CN=remotefriend-relay"
  echo "New certificate generated."
fi
chown root:remotefriend "$INSTALL_DIR/key.pem" "$INSTALL_DIR/cert.pem"
chmod 0640 "$INSTALL_DIR/key.pem"
chmod 0644 "$INSTALL_DIR/cert.pem"

echo "=== 5/6 service ==="
curl -sL "https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/remotefriend.service" -o "$SERVICE_FILE"
systemctl daemon-reload
systemctl enable --now remotefriend
sleep 2
systemctl restart remotefriend
sleep 2

echo "=== 6/6 nginx + firewall ==="
if [ ! -f "$NGINX_SITE" ]; then
  # ONLY requests to this IP land here; other sites are untouched.
  cat > "$NGINX_SITE" <<EOF
server {
  listen 80;
  server_name $SERVER_IP;
  location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Host \$host;
    proxy_set_header X-Forwarded-Proto http;
    proxy_set_header Upgrade \$http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 3600s;
    proxy_send_timeout 3600s;
  }
}
EOF
  echo "nginx site written."
else
  echo "nginx site already exists, untouched."
fi
nginx -t
systemctl reload nginx
ufw allow 80/tcp > /dev/null 2>&1 || true
ufw allow 33202/tcp > /dev/null 2>&1 || true

echo
echo "=== STATUS ==="
systemctl is-active --quiet remotefriend && echo "service: RUNNING" || { echo "service: FAILED"; journalctl -u remotefriend -n 20 --no-pager; exit 1; }
curl -s -o /dev/null -w "web (127.0.0.1:8080): %{http_code}\n" http://127.0.0.1:8080/ || echo "web: UNREACHABLE"
echo
echo "Fingerprint (needed for host/clients, unchanged if cert kept):"
openssl x509 -in "$INSTALL_DIR/cert.pem" -outform der | sha256sum | awk '{print $1}'
echo
echo "Done. On phone browser: http://SERVER_IP (no port) -> code + password"
