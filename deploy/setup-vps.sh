#!/usr/bin/env bash
# RemoteFriend VPS kurulum/güncelleme (tek komut, tekrar çalıştırmak güvenli).
#   curl -sL https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/setup-vps.sh | sudo bash
#
# Yapılanlar: servis kullanıcısı, binary, TLS sertifikası (33202, korunur), systemd servisi,
# nginx + otomatik HTTPS (Let's Encrypt), güvenlik duvarı, izin onarımı.
#
# Seçenekler (ortam değişkeni):
#   SERVER_IP=1.2.3.4      genel IP otomatik bulunamazsa
#   DOMAIN=uzak.ornek.com  kendi alan adın (yoksa <ip>.sslip.io kullanılır, ayar gerekmez)
#   EMAIL=sen@ornek.com    Let's Encrypt bildirimleri için (isteğe bağlı)
#   NO_TLS=1               HTTPS kurma (önerilmez: telefonda görüntü yavaş, şifre açık gider)
#   VERSION=v0.6.2         belirli sürüm
set -euo pipefail

REPO_RAW="https://raw.githubusercontent.com/siyahkarga/remotefriend/main"
REPO="https://github.com/siyahkarga/remotefriend"
INSTALL_DIR="/opt/remotefriend"
DATA_DIR="/var/lib/remotefriend"
SERVICE_FILE="/etc/systemd/system/remotefriend.service"
NGINX_SITE="/etc/nginx/sites-enabled/remotefriend"
ACME_ROOT="/var/www/remotefriend-acme"

if [ "$(id -u)" -ne 0 ]; then
  echo "root gerekli: sudo ile çalıştır." >&2
  exit 1
fi

VERSION="${VERSION:-$(curl -s --max-time 10 https://api.github.com/repos/siyahkarga/remotefriend/releases/latest | grep '"tag_name"' | cut -d'"' -f4 || true)}"
if [ -z "$VERSION" ]; then
  echo "Son sürüm bulunamadı (GitHub'a erişim?). VERSION=vX.Y.Z ile çalıştır." >&2
  exit 1
fi
SERVER_IP="${SERVER_IP:-$(curl -s -4 --max-time 10 https://ifconfig.me 2>/dev/null || true)}"
if [ -z "$SERVER_IP" ]; then
  echo "Genel IP bulunamadı. Şöyle çalıştır: curl -sL ... | sudo SERVER_IP=x.x.x.x bash" >&2
  exit 1
fi
DOMAIN="${DOMAIN:-${SERVER_IP//./-}.sslip.io}"

echo "=== 1/7 paketler ==="
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq nginx curl openssl ufw ca-certificates > /dev/null
if [ "${NO_TLS:-0}" != "1" ]; then
  apt-get install -y -qq certbot > /dev/null
fi

echo "=== 2/7 kullanıcı ve dizinler ==="
id remotefriend >/dev/null 2>&1 || useradd --system --home "$DATA_DIR" --shell /usr/sbin/nologin remotefriend
install -d -o root -g remotefriend -m 0750 "$INSTALL_DIR"
install -d -o remotefriend -g remotefriend -m 0700 "$DATA_DIR"
# Elle yapılan değişikliklerden kalan bozuk izinleri onar.
chown -R remotefriend:remotefriend "$DATA_DIR"
chmod 0700 "$DATA_DIR"

# Bir adım hata verirse (ya da betik erken biterse) röle kapalı kalmasın.
trap 'systemctl is-active --quiet remotefriend 2>/dev/null || systemctl start remotefriend 2>/dev/null || true' EXIT

echo "=== 3/7 binary ($VERSION) ==="
TMP_BIN="$(mktemp "$INSTALL_DIR/.rv.XXXXXX")"
if ! curl -fsSL --retry 3 --max-time 600 "$REPO/releases/download/$VERSION/remote-friend-rendezvous" -o "$TMP_BIN"; then
  rm -f "$TMP_BIN"
  echo "İNDİRME HATASI: sürüm adı ($VERSION) ya da internet erişimi." >&2
  exit 1
fi
if [ "$(stat -c%s "$TMP_BIN")" -lt 1000000 ]; then
  rm -f "$TMP_BIN"
  echo "İNDİRME HATASI: dosya çok küçük (bozuk indirme)." >&2
  exit 1
fi
chmod 0755 "$TMP_BIN"
chown root:remotefriend "$TMP_BIN"
systemctl stop remotefriend 2>/dev/null || true
mv -f "$TMP_BIN" "$INSTALL_DIR/remote-friend-rendezvous"

echo "=== 4/7 relay TLS sertifikası (varsa korunur) ==="
if [ -f "$INSTALL_DIR/cert.pem" ] && [ -f "$INSTALL_DIR/key.pem" ]; then
  echo "Mevcut sertifika korundu (host'lardaki parmak izi geçerli kalır)."
else
  openssl req -x509 -newkey rsa:3072 -sha256 \
    -keyout "$INSTALL_DIR/key.pem" -out "$INSTALL_DIR/cert.pem" \
    -days 3650 -nodes -subj "/CN=remotefriend-relay" 2>/dev/null
  echo "Yeni sertifika üretildi."
fi
chown root:remotefriend "$INSTALL_DIR/key.pem" "$INSTALL_DIR/cert.pem"
chmod 0640 "$INSTALL_DIR/key.pem"
chmod 0644 "$INSTALL_DIR/cert.pem"

echo "=== 5/7 servis ==="
# İç web portu (yalnızca 127.0.0.1, nginx arkası). Başka bir uygulama kullanıyorsa
# (ör. aynı VPS'teki başka bir site 8080'de) sıradaki boş port seçilir.
port_busy() { ss -ltnH "sport = :$1" 2>/dev/null | grep -q .; }
WEB_PORT="${WEB_PORT:-33203}"
while port_busy "$WEB_PORT"; do
  echo "port $WEB_PORT başka bir uygulamada; sonraki deneniyor"
  WEB_PORT=$((WEB_PORT + 1))
  [ "$WEB_PORT" -gt 33299 ] && { echo "boş yerel port bulunamadı" >&2; exit 1; }
done
echo "iç web portu: 127.0.0.1:$WEB_PORT"
TMP_UNIT="$(mktemp)"
curl -fsSL "$REPO_RAW/deploy/remotefriend.service" -o "$TMP_UNIT"
grep -q "ExecStart=/opt/remotefriend/remote-friend-rendezvous" "$TMP_UNIT" || { echo "servis dosyası bozuk indi" >&2; exit 1; }
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
  }
EOF
}
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
# Daha önce HTTPS kurulduysa (HSTS gönderildi) asla düz HTTP'ye dönme.
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
    echo "HTTPS hazır: $WEB_URL"
  elif [ -f "$LIVE_CERT" ]; then
    WEB_URL="https://$DOMAIN"
    echo "UYARI: sertifika yenilenemedi (/tmp/rf-certbot.log); mevcut sertifikayla HTTPS sürüyor."
  else
    echo "UYARI: HTTPS sertifikası alınamadı (80 portu dışarıdan açık mı?). Ayrıntı: /tmp/rf-certbot.log"
    echo "       Şimdilik düz HTTP ile devam ediliyor; telefonda görüntü yavaş olur."
  fi
fi
nginx -t
systemctl reload nginx

echo "=== 7/7 durum ==="
if systemctl is-active --quiet remotefriend; then
  echo "servis: ÇALIŞIYOR"
else
  echo "servis: HATA"
  journalctl -u remotefriend -n 40 --no-pager
  exit 1
fi
# Sayfanın gerçekten RemoteFriend olduğunu doğrula (port çakışması / yanlış site kontrolü).
if curl -s --max-time 5 "http://127.0.0.1:$WEB_PORT/info" | grep -q '"role":"relay"'; then
  echo "web (127.0.0.1:$WEB_PORT): RemoteFriend OK"
else
  echo "HATA: 127.0.0.1:$WEB_PORT RemoteFriend yanıtı vermiyor."
  journalctl -u remotefriend -n 30 --no-pager
  exit 1
fi
if [ "${WEB_URL#https://}" != "$WEB_URL" ]; then
  if curl -s --max-time 10 "$WEB_URL/info" | grep -q '"role":"relay"'; then
    echo "dışarıdan ($WEB_URL): RemoteFriend OK"
  else
    echo "UYARI: $WEB_URL dışarıdan RemoteFriend göstermiyor (443 başka bir sunucuda olabilir)."
  fi
fi
FP="$(openssl x509 -in "$INSTALL_DIR/cert.pem" -outform der | sha256sum | awk '{print $1}')"
cat <<EOF

=============================================================
 Kurulum tamam.

 Telefondan / tarayıcıdan:   $WEB_URL
   (bilgisayar kodu + şifre ile bağlan)

 Paylaşılacak bilgisayarda (host) ayar:
   RF_RV_SERVER=$SERVER_IP:33202
   RF_WEB_URL=$WEB_URL
 İlk bağlanışta host bu parmak izini sorar, aynı olmalı:
   $FP
=============================================================
EOF
