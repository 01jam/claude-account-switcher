#!/usr/bin/env bash
#
# Give the dev build a proper icon in the dock.
#
# GNOME picks a window's icon by matching its app id against an installed
# .desktop file. `tauri dev` runs a bare binary with nothing installed, so the
# shell falls back to a generic icon. This installs a user-level entry pointing
# at the source tree; a packaged build (`npm run app:build`) ships its own and
# does not need this.
#
# Remove it with:
#   rm ~/.local/share/applications/studio.hund.claude-switch.desktop

set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
apps="$HOME/.local/share/applications"
themedir="$HOME/.local/share/icons/hicolor"
id="studio.hund.claude-switch"
entry="$apps/$id.desktop"
binary="$root/src-tauri/target/debug/claude-switch"

# Install into the icon theme under a logical name rather than pointing at the
# source tree: a themed icon can be re-cached with gtk-update-icon-cache, while
# an absolute path tends to stay stuck on whatever the shell cached first.
for size in 32x32 128x128 256x256; do
    src="$root/src-tauri/icons/$size.png"
    [ "$size" = 256x256 ] && src="$root/src-tauri/icons/128x128@2x.png"
    [ -f "$src" ] || continue
    mkdir -p "$themedir/$size/apps"
    cp -f "$src" "$themedir/$size/apps/$id.png"
done

mkdir -p "$apps"
cat > "$entry" <<EOF
[Desktop Entry]
Type=Application
Name=Claude Account Switcher
Comment=Cambia account Claude Code
Exec=$binary
Icon=$id
Terminal=false
Categories=Utility;
# Both spellings: the window reports one of these depending on the session.
StartupWMClass=claude-switch
EOF

gtk-update-icon-cache -f -t "$themedir" 2>/dev/null || true
update-desktop-database "$apps" 2>/dev/null || true
touch "$entry"

echo "installed $entry"
echo "icons:    $themedir/*/apps/$id.png"
