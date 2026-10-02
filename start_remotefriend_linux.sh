#!/usr/bin/env bash
# RemoteFriend one-command setup + start (Linux x86_64).
# Usage: ./start_remotefriend_linux.sh
# Downloads missing binaries from the latest release, asks server settings once,
# saves them to ~/.config/remotefriend/local.env (stays on your machine), runs host.
set -euo pipefail

BASE="https://github.com/siyahkarga/remotefriend/releases/latest/download"
BIN_DIR="$(cd "$(dirname "$0")" && pwd)"
ENV_FILE="$HOME/.config/remotefriend/local.env"

for bin in remote-friend-host remote-friend-client; do
  if [ ! -x "$BIN_DIR/$bin" ]; then
    echo "Downloading $bin..."
    curl -sL "$BASE/$bin" -o "$BIN_DIR/$bin"
    chmod +x "$BIN_DIR/$bin"
  fi
done

mkdir -p "$(dirname "$ENV_FILE")"
[ -f "$ENV_FILE" ] && . "$ENV_FILE"

if [ -z "${RF_RV_SERVER:-}" ]; then
  echo "Leave server empty for LAN-only mode."
  read -r -p "Server [example 1.2.3.4:33202, empty=LAN]: " RF_RV_SERVER || true
fi
if [ -n "${RF_RV_SERVER:-}" ] && [ -z "${RF_RV_FP:-}" ]; then
  read -r -p "Certificate fingerprint: " RF_RV_FP || true
fi
if [ -z "${REMOTE_FRIEND_PASS:-}" ]; then
  echo "No password set: the program generates a new one on every start."
fi

{
  echo "# Auto-generated, delete to ask again."
  [ -n "${RF_RV_SERVER:-}" ] && echo "RF_RV_SERVER=$RF_RV_SERVER"
  [ -n "${RF_RV_FP:-}" ] && echo "RF_RV_FP=$RF_RV_FP"
  [ -n "${REMOTE_FRIEND_PASS:-}" ] && echo "REMOTE_FRIEND_PASS=$REMOTE_FRIEND_PASS"
} > "$ENV_FILE"
chmod 600 "$ENV_FILE" 2>/dev/null || true

export RF_RV_SERVER RF_RV_FP REMOTE_FRIEND_PASS
echo
echo "Starting host..."
exec "$BIN_DIR/remote-friend-host"
