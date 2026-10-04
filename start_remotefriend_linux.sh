#!/usr/bin/env bash
# RemoteFriend terminal host (alternative to the desktop app; Linux x86_64, X11 and Wayland).
# Most people should install the app instead: https://blobidea.com/products/remotefriend
# Usage: ./start_remotefriend_linux.sh
# Updates the programs when a new release is out, asks for the server once
# (saved in ~/.config/remotefriend/local.env) and starts the host.
set -euo pipefail

REPO_API="https://api.github.com/repos/siyahkarga/remotefriend/releases/latest"
BASE="https://github.com/siyahkarga/remotefriend/releases/latest/download"
BIN_DIR="$(cd "$(dirname "$0")" && pwd)"
ENV_FILE="$HOME/.config/remotefriend/local.env"
VER_FILE="$BIN_DIR/.rf-version"

latest="$(curl -s --max-time 8 "$REPO_API" | grep '"tag_name"' | cut -d'"' -f4 || true)"
current="$(cat "$VER_FILE" 2>/dev/null || true)"
updated_ok=1
for bin in remote-friend-host; do
  if [ ! -x "$BIN_DIR/$bin" ] || { [ -n "$latest" ] && [ "$latest" != "$current" ]; }; then
    echo "Downloading: $bin ${latest:-}"
    if curl -fsSL "$BASE/$bin" -o "$BIN_DIR/$bin.new"; then
      chmod +x "$BIN_DIR/$bin.new"
      mv -f "$BIN_DIR/$bin.new" "$BIN_DIR/$bin"
    else
      rm -f "$BIN_DIR/$bin.new"
      [ -x "$BIN_DIR/$bin" ] || { echo "DOWNLOAD ERROR: check your internet connection." >&2; exit 1; }
      echo "Warning: update failed, continuing with the current version."
      updated_ok=0
    fi
  fi
done
if [ -n "$latest" ] && [ "$updated_ok" = 1 ]; then echo "$latest" > "$VER_FILE"; fi

mkdir -p "$(dirname "$ENV_FILE")"
# shellcheck disable=SC1090
[ -f "$ENV_FILE" ] && . "$ENV_FILE"

if [ -z "${RF_REGISTER_KEY+x}" ]; then
  echo "To be reachable over the internet (e.g. from your phone), enter your access key; leave empty for local network only."
  read -r -p "Access key [RF-XXXXX-...]: " RF_REGISTER_KEY || true
fi

# Values are shell-quoted safely (the password may contain spaces, $, ; etc.).
{
  echo "# Generated automatically; delete this file to be asked again."
  printf 'RF_REGISTER_KEY=%q\n' "${RF_REGISTER_KEY:-}"
  [ -n "${RF_RV_SERVER:-}" ] && printf 'RF_RV_SERVER=%q\n' "$RF_RV_SERVER"
  [ -n "${RF_WEB_URL:-}" ] && printf 'RF_WEB_URL=%q\n' "$RF_WEB_URL"
  [ -n "${RF_RV_FP:-}" ] && printf 'RF_RV_FP=%q\n' "$RF_RV_FP"
  [ -n "${REMOTE_FRIEND_PASS:-}" ] && printf 'REMOTE_FRIEND_PASS=%q\n' "$REMOTE_FRIEND_PASS"
  true
} > "$ENV_FILE"
chmod 600 "$ENV_FILE" 2>/dev/null || true

export RF_REGISTER_KEY RF_RV_SERVER RF_WEB_URL RF_RV_FP REMOTE_FRIEND_PASS
if [ "${XDG_SESSION_TYPE:-}" = "wayland" ] || [ -n "${WAYLAND_DISPLAY:-}" ]; then
  echo "Wayland: when the 'remote desktop' permission prompt appears, choose Share/Allow (asked once)."
fi
echo
exec "$BIN_DIR/remote-friend-host"
