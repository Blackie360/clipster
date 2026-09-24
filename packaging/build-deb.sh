#!/usr/bin/env bash
# Build a clipster .deb from already-compiled binaries.
#
# Deliberately hand-rolled with dpkg-deb rather than cargo-deb: the package
# spans two crates plus contrib files, and a staging tree is easier to reason
# about (and to inspect in CI) than metadata spread across Cargo.toml files.
#
#   packaging/build-deb.sh --version 0.1.0 --bin-dir target/release
#
set -euo pipefail

VERSION=""
ARCH="$(dpkg --print-architecture)"
BIN_DIR="target/release"
OUT_DIR="dist"
STATIC=0

usage() { sed -n '2,10p' "$0"; exit "${1:-0}"; }

while [ $# -gt 0 ]; do
    case "$1" in
        --version)  VERSION="$2"; shift 2 ;;
        --arch)     ARCH="$2";    shift 2 ;;
        --bin-dir)  BIN_DIR="$2"; shift 2 ;;
        --out-dir)  OUT_DIR="$2"; shift 2 ;;
        # Static builds carry no shared-library dependencies, so the control
        # file must not claim any.
        --static)   STATIC=1;     shift ;;
        -h|--help)  usage 0 ;;
        *) echo "unknown argument: $1" >&2; usage 1 ;;
    esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Default the version to whatever the workspace declares, so the package and
# the crate cannot drift apart.
if [ -z "$VERSION" ]; then
    VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
fi
[ -n "$VERSION" ] || { echo "could not determine version" >&2; exit 1; }

for bin in clipster clipsterd; do
    [ -x "$BIN_DIR/$bin" ] || { echo "missing binary: $BIN_DIR/$bin (cargo build --release first)" >&2; exit 1; }
done

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
# mktemp gives 0700; the package root must be world-readable like any other
# directory dpkg lays down, or the install leaves / with odd permissions.
chmod 755 "$STAGE"

install -Dm755 "$BIN_DIR/clipster"        "$STAGE/usr/bin/clipster"
install -Dm755 "$BIN_DIR/clipsterd"       "$STAGE/usr/bin/clipsterd"
install -Dm755 contrib/clipster-rofi.sh   "$STAGE/usr/bin/clipster-rofi"
install -Dm644 contrib/config.example.toml "$STAGE/usr/share/doc/clipster/config.example.toml"
install -Dm644 README.md                  "$STAGE/usr/share/doc/clipster/README.md"

# The shipped unit points at ~/.local/bin for source installs; a packaged one
# must point at the packaged binary. Rewriting here keeps contrib/ as the
# single source of truth for everything else in the unit.
install -Dm644 contrib/clipsterd.service "$STAGE/usr/lib/systemd/user/clipsterd.service"
sed -i 's|^ExecStart=.*|ExecStart=/usr/bin/clipsterd|' "$STAGE/usr/lib/systemd/user/clipsterd.service"

# ProtectHome=read-only would stop the daemon writing its own database under
# a packaged install, since ReadWritePaths uses %h.
grep -q 'ReadWritePaths' "$STAGE/usr/lib/systemd/user/clipsterd.service" || {
    echo "unit lost its ReadWritePaths line" >&2; exit 1; }

# Enables the unit for users who run `systemctl --user preset-all`, and for
# newly created accounts. Existing sessions still need an explicit enable,
# which postinst spells out.
install -Dm644 /dev/stdin "$STAGE/usr/lib/systemd/user-preset/90-clipster.preset" <<'PRESET'
enable clipsterd.service
PRESET

install -Dm644 /dev/stdin "$STAGE/usr/share/doc/clipster/copyright" <<'COPYRIGHT'
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: clipster
Source: https://github.com/Blackie360/clipster

Files: *
Copyright: 2026 Felix Jumason
License: MIT
 Permission is hereby granted, free of charge, to any person obtaining a
 copy of this software and associated documentation files (the "Software"),
 to deal in the Software without restriction, including without limitation
 the rights to use, copy, modify, merge, publish, distribute, sublicense,
 and/or sell copies of the Software, and to permit persons to whom the
 Software is furnished to do so, subject to the following conditions:
 .
 The above copyright notice and this permission notice shall be included in
 all copies or substantial portions of the Software.
 .
 THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
 DEALINGS IN THE SOFTWARE.
COPYRIGHT

if [ "$STATIC" -eq 1 ]; then
    DEPENDS=""
else
    DEPENDS="libc6"
fi

mkdir -p "$STAGE/DEBIAN"
{
    echo "Package: clipster"
    echo "Version: $VERSION"
    echo "Section: utils"
    echo "Priority: optional"
    echo "Architecture: $ARCH"
    [ -n "$DEPENDS" ] && echo "Depends: $DEPENDS"
    # clipster copy shells out to one of these; the picker needs a menu.
    echo "Recommends: xclip | xsel | wl-clipboard"
    echo "Suggests: rofi | fuzzel | suckless-tools"
    echo "Maintainer: Felix Jumason <felixkent360@gmail.com>"
    echo "Homepage: https://github.com/Blackie360/clipster"
    cat <<'DESC'
Description: fast keyboard-driven clipboard history
 clipster captures clipboard history in a background daemon and gives it
 back through a scriptable CLI. It stores text entries in SQLite, dedupes
 them, supports pinning and named snippets, and can skip clipboard content
 from denylisted applications.
 .
 This release captures via X11 (XFixes) only; under Wayland it sees
 XWayland clients but not native Wayland ones.
DESC
} > "$STAGE/DEBIAN/control"

install -Dm755 /dev/stdin "$STAGE/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -e
if [ "$1" = "configure" ]; then
    cat <<'EOF'

clipster is installed. It runs as a systemd *user* service, so enable it as
your own user (not root):

    systemctl --user daemon-reload
    systemctl --user enable --now clipsterd
    clipster status

An example config is at /usr/share/doc/clipster/config.example.toml; copy it
to ~/.config/clipster/config.toml to change defaults. Review the privacy
denylist there before relying on it.

EOF
fi
exit 0
POSTINST

# md5sums covers everything except the control files themselves.
( cd "$STAGE" && find . -path ./DEBIAN -prune -o -type f -print0 \
    | xargs -0 md5sum | sed 's|\./||' > DEBIAN/md5sums )

mkdir -p "$OUT_DIR"
PKG="$OUT_DIR/clipster_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group --build "$STAGE" "$PKG" >/dev/null
echo "$PKG"
