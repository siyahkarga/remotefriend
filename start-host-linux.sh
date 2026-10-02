#!/usr/bin/env bash
# RemoteFriend tek-komut kurulum + başlat (Linux x86_64).
# Kullanım: ./baslat.sh
# Exe yoksa Releases'tan indirir, sunucu ayarlarını bir kez sorar,
# ~/.config/remotefriend/ayarlarim.env dosyasına kaydeder.
set -euo pipefail

VER="v0.5.6"
BIN_DIR="$(cd "$(dirname "$0")" && pwd)"
EXE="$BIN_DIR/remote-friend-host"
ENV_FILE="$HOME/.config/remotefriend/ayarlarim.env"

[ -x "$EXE" ] || {
  echo "Host indiriliyor ($VER)..."
  curl -sL "https://github.com/siyahkarga/remotefriend/releases/download/$VER/remote-friend-host" -o "$EXE"
  chmod +x "$EXE"
}

mkdir -p "$(dirname "$ENV_FILE")"
[ -f "$ENV_FILE" ] && . "$ENV_FILE"

if [ -z "${RF_RV_SERVER:-}" ]; then
  echo "Sunucu boş bırakılırsa sadece LAN çalışır."
  read -r -p "Sunucu [örnek 1.2.3.4:33202, boş=LAN]: " RF_RV_SERVER || true
fi
if [ -n "${RF_RV_SERVER:-}" ] && [ -z "${RF_RV_FP:-}" ]; then
  read -r -p "Sertifika parmak izi: " RF_RV_FP || true
fi
if [ -z "${REMOTE_FRIEND_PASS:-}" ]; then
  echo "Şifre boş: program her açılışta yeni şifre üretir."
fi

{
  echo "# Otomatik oluştu, silersen tekrar sorar."
  [ -n "${RF_RV_SERVER:-}" ] && echo "RF_RV_SERVER=$RF_RV_SERVER"
  [ -n "${RF_RV_FP:-}" ] && echo "RF_RV_FP=$RF_RV_FP"
  [ -n "${REMOTE_FRIEND_PASS:-}" ] && echo "REMOTE_FRIEND_PASS=$REMOTE_FRIEND_PASS"
} > "$ENV_FILE"
chmod 600 "$ENV_FILE" 2>/dev/null || true

export RF_RV_SERVER RF_RV_FP REMOTE_FRIEND_PASS
echo
echo "Başlatılıyor..."
exec "$EXE"
