#!/usr/bin/env bash
# RemoteFriend tek tıkla kurulum + başlatma (macOS, Apple Silicon ve Intel).
# Finder'da çift tıkla. Yeni sürüm varsa günceller, sunucu ayarını bir kez sorar.
#
# İlk çalıştırmada macOS iki izin ister (Sistem Ayarları > Gizlilik ve Güvenlik):
#   - Ekran Kaydı      (görüntü için)
#   - Erişilebilirlik  (fare/klavye kontrolü için)
# İzni Terminal uygulamasına ver, sonra bu dosyayı tekrar çalıştır.
set -euo pipefail

REPO_API="https://api.github.com/repos/siyahkarga/remotefriend/releases/latest"
BASE="https://github.com/siyahkarga/remotefriend/releases/latest/download"
BIN_DIR="$(cd "$(dirname "$0")" && pwd)"
ENV_FILE="$HOME/.config/remotefriend/local.env"
VER_FILE="$BIN_DIR/.rf-version"
cd "$BIN_DIR"

latest="$(curl -s --max-time 8 "$REPO_API" | grep '"tag_name"' | cut -d'"' -f4 || true)"
current="$(cat "$VER_FILE" 2>/dev/null || true)"
updated_ok=1
for bin in remote-friend-host remote-friend-client; do
  if [ ! -x "$BIN_DIR/$bin" ] || { [ -n "$latest" ] && [ "$latest" != "$current" ]; }; then
    echo "İndiriliyor: $bin ${latest:-}"
    if curl -fsSL "$BASE/$bin-macos" -o "$BIN_DIR/$bin.new"; then
      chmod +x "$BIN_DIR/$bin.new"
      xattr -d com.apple.quarantine "$BIN_DIR/$bin.new" 2>/dev/null || true
      mv -f "$BIN_DIR/$bin.new" "$BIN_DIR/$bin"
    else
      rm -f "$BIN_DIR/$bin.new"
      [ -x "$BIN_DIR/$bin" ] || { echo "İNDİRME HATASI: internet bağlantısını kontrol et." >&2; read -r -p "Enter..."; exit 1; }
      updated_ok=0
    fi
  fi
done
if [ -n "$latest" ] && [ "$updated_ok" = 1 ]; then echo "$latest" > "$VER_FILE"; fi

mkdir -p "$(dirname "$ENV_FILE")"
# shellcheck disable=SC1090
[ -f "$ENV_FILE" ] && . "$ENV_FILE"
if [ -z "${RF_RV_SERVER+x}" ]; then
  echo "İnternetten (telefondan) bağlanmak için VPS adresini yaz; sadece yerel ağ için boş bırak."
  read -r -p "Sunucu [örnek 1.2.3.4:33202, boş = yerel ağ]: " RF_RV_SERVER || true
fi
# Değerler kabuk-güvenli tırnaklanır (şifrede boşluk, $, ; vb. olabilir).
{
  echo "# Otomatik oluşturuldu; tekrar sorması için bu dosyayı sil."
  printf 'RF_RV_SERVER=%q\n' "${RF_RV_SERVER:-}"
  [ -n "${RF_WEB_URL:-}" ] && printf 'RF_WEB_URL=%q\n' "$RF_WEB_URL"
  [ -n "${RF_RV_FP:-}" ] && printf 'RF_RV_FP=%q\n' "$RF_RV_FP"
  [ -n "${REMOTE_FRIEND_PASS:-}" ] && printf 'REMOTE_FRIEND_PASS=%q\n' "$REMOTE_FRIEND_PASS"
  true
} > "$ENV_FILE"
chmod 600 "$ENV_FILE" 2>/dev/null || true
export RF_RV_SERVER RF_WEB_URL RF_RV_FP REMOTE_FRIEND_PASS

echo "Not: görüntü gelmezse Ekran Kaydı, kontrol çalışmazsa Erişilebilirlik iznini Terminal'e ver."
echo
"$BIN_DIR/remote-friend-host"
