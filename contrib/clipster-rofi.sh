#!/usr/bin/env bash
# Picker for clipster built on rofi or dmenu.
#
# This stands in for the native picker until v0.2. Bind it to a hotkey:
#   i3/sway:   bindsym $mod+v exec --no-startup-id ~/.local/bin/clipster-rofi
#   Hyprland:  bind = SUPER, V, exec, ~/.local/bin/clipster-rofi
set -euo pipefail

CLIPSTER=${CLIPSTER:-clipster}
PROMPT=${CLIPSTER_PROMPT:-clipboard}

if command -v rofi >/dev/null 2>&1; then
    menu() { rofi -dmenu -i -p "$PROMPT" -format s; }
elif command -v fuzzel >/dev/null 2>&1; then
    menu() { fuzzel --dmenu --prompt "$PROMPT: "; }
elif command -v dmenu >/dev/null 2>&1; then
    menu() { dmenu -i -l 15 -p "$PROMPT"; }
else
    echo "clipster-rofi: install rofi, fuzzel or dmenu" >&2
    exit 1
fi

# --format plain emits "id<TAB>preview", one entry per line, pinned first.
selection=$("$CLIPSTER" list --all --format plain | menu) || exit 0
[ -n "$selection" ] || exit 0

# Take the id from before the first tab. Previews are single-line by
# construction, so the id is unambiguous.
id=${selection%%$'\t'*}
case "$id" in
    ''|*[!0-9]*) echo "clipster-rofi: could not parse an id from selection" >&2; exit 1 ;;
esac

exec "$CLIPSTER" copy "$id"
