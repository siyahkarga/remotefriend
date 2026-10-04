#!/usr/bin/env sh
# Installs RemoteFriend for the current user (no root needed): app menu entry + icon.
# Usage: ./install.sh        (run from the extracted folder)
set -e
DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$HOME/.local/bin"
APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor/256x256/apps"
mkdir -p "$BIN" "$APPS" "$ICONS"
install -m 755 "$DIR/remotefriend" "$DIR/remote-friend-host" "$BIN/"
install -m 644 "$DIR/remotefriend.png" "$ICONS/remotefriend.png"
sed "s|^Exec=.*|Exec=$BIN/remotefriend|" "$DIR/remotefriend.desktop" > "$APPS/remotefriend.desktop"
update-desktop-database "$APPS" >/dev/null 2>&1 || true
echo "RemoteFriend installed. Open it from your applications menu."
