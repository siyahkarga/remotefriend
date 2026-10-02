#!/usr/bin/env bash
# RemoteFriend v0.5.6 VPS kurulum/güncelleme (tek seferde çalışır).
# Kullanım: curl -sL https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/vps-kur-v056.sh | sudo bash
# Mevcut sertifika VARSA korunur (istemcilerdeki fingerprint bozulmaz).
set -euo pipefail

VERSION="v0.5.6"
REPO="https://github.com/siyahkarga/remotefriend"
INSTALL_DIR="/opt/remotefriend"
DATA_DIR="/var/lib/remotefriend"
SERVICE_FILE="/etc/systemd/system/remotefriend.service"
NGINX_SITE="/etc/nginx/sites-enabled/remotefriend"

if [ "$(id -u)" -ne 0 ]; then
  echo "root gerekli: sudo ile çalıştır." >&2
  exit 1
fi

echo "=== 1/6 paketler ==="
apt-get update -qq
apt-get install -y -qq nginx curl openssl ufw ca-certificates > /dev/null

echo "=== 2/6 kullanıcı ve dizinler ==="
useradd --system --home "$DATA_DIR" --shell /usr/sbin/nologin remotefriend 2>/dev/null || true
install -d -o root -g remotefriend -m 0750 "$INSTALL_DIR"
install -d -o remotefriend -g remotefriend -m 0700 "$DATA_DIR"

echo "=== 3/6 binary ($VERSION) ==="
curl -sL "$REPO/releases/download/$VERSION/remote-friend-rendezvous" -o "$INSTALL_DIR/remote-friend-rendezvous"
chmod +x "$INSTALL_DIR/remote-friend-rendezvous"
chown root:remotefriend "$INSTALL_DIR/remote-friend-rendezvous"

echo "=== 4/6 TLS sertifikası (varsa korunur) ==="
if [ -f "$INSTALL_DIR/cert.pem" ] && [ -f "$INSTALL_DIR/key.pem" ]; then
  echo "Mevcut sertifika korunuyor."
else
  openssl req -x509 -newkey rsa:3072 \
    -keyout "$INSTALL_DIR/key.pem" -out "$INSTALL_DIR/cert.pem" \
    -days 825 -nodes -subj "/CN=remotefriend-relay"
  echo "Yeni sertifika üretildi."
fi
chown root:remotefriend "$INSTALL_DIR/key.pem" "$INSTALL_DIR/cert.pem"
chmod 0640 "$INSTALL_DIR/key.pem"
chmod 0644 "$INSTALL_DIR/cert.pem"

echo "=== 5/6 servis ==="
curl -sL "https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/remotefriend.service" -o "$SERVICE_FILE"
systemctl daemon-reload
systemctl enable --now remotefriend
sleep 2
systemctl restart remotefriend
sleep 2

echo "=== 6/6 nginx + firewall ==="
if [ ! -f "$NGINX_SITE" ]; then
  # SADECE bu IP'ye gelen istekler buraya düşer; diğer sitelere dokunmaz.
  cat > "$NGINX_SITE" <<'EOF'
server {
  listen 80;
  server_name 169.58.37.61;
  location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Proto http;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 3600s;
    proxy_send_timeout 3600s;
  }
}
EOF
  echo "nginx sitesi yazıldı."
else
  echo "nginx sitesi zaten var, dokunulmadı."
fi
nginx -t
systemctl reload nginx
ufw allow 80/tcp > /dev/null 2>&1 || true
ufw allow 33202/tcp > /dev/null 2>&1 || true

echo
echo "=== DURUM ==="
systemctl is-active --quiet remotefriend && echo "servis: CALISIYOR" || { echo "servis: HATA"; journalctl -u remotefriend -n 20 --no-pager; exit 1; }
curl -s -o /dev/null -w "web (127.0.0.1:8080): %{http_code}\n" http://127.0.0.1:8080/ || echo "web: ERISILEMIYOR"
echo
echo "Fingerprint (host/clientlara lazım, değişmediyse aynıdır):"
openssl x509 -in "$INSTALL_DIR/cert.pem" -outform der | sha256sum | awk '{print $1}'
echo
echo "Bitti. Telefonda: http://SUNUCU_IP  (portsuz) -> kod + sifre"
