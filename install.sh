#!/bin/sh
# clipster installer — one command, no Rust toolchain required.
#
#   curl -fsSL https://raw.githubusercontent.com/Blackie360/clipster/main/install.sh | sh
#
# Downloads the prebuilt static (musl) binaries for this machine's
# architecture, drops them under ~/.local (no sudo), installs the systemd
# user unit and starts the daemon.
#
# Options (pass with `... | sh -s -- --flag`):
#   --version X.Y.Z   install a specific release instead of the latest
#   --prefix DIR      install root (default ~/.local, or /usr/local as root)
#   --no-service      install files only; do not enable or start clipsterd
#   --uninstall       remove what this script installed (config/data are kept)
#
# Environment:
#   GITHUB_TOKEN      token for a private repo (the `gh` CLI is used if set up)
#   CLIPSTER_TARBALL  install from a local release tarball instead of downloading
set -eu

REPO="${CLIPSTER_REPO:-Blackie360/clipster}"
VERSION="${CLIPSTER_VERSION:-latest}"
TARBALL="${CLIPSTER_TARBALL:-}"
TOKEN="${GITHUB_TOKEN:-${GH_TOKEN:-}}"
PREFIX=""
WANT_SERVICE=1
UNINSTALL=0

info() { printf '  %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
usage() { sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }

while [ $# -gt 0 ]; do
    case "$1" in
        --version)   VERSION="${2:-}"; shift 2 ;;
        --prefix)    PREFIX="${2:-}";  shift 2 ;;
        --no-service) WANT_SERVICE=0;  shift ;;
        --uninstall) UNINSTALL=1;      shift ;;
        -h|--help)   usage 0 ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; usage 1 ;;
    esac
done

[ "$(uname -s)" = "Linux" ] || die "clipster is Linux-only (this is $(uname -s))."

IS_ROOT=0
[ "$(id -u)" -eq 0 ] && IS_ROOT=1

if [ -z "$PREFIX" ]; then
    if [ "$IS_ROOT" -eq 1 ]; then PREFIX="/usr/local"; else PREFIX="$HOME/.local"; fi
fi
BIN_DIR="$PREFIX/bin"
DOC_DIR="$PREFIX/share/doc/clipster"
# A user install keeps its unit in the user's own config; a root install ships
# a system-wide *user* unit, which every account then enables for itself.
if [ "$IS_ROOT" -eq 1 ]; then
    UNIT_DIR="$PREFIX/lib/systemd/user"
else
    UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
fi
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/clipster"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/clipster"

# systemctl --user needs a session bus; there is none in a container, a plain
# ssh session without lingering, or a root shell.
user_systemd() {
    [ "$IS_ROOT" -eq 0 ] && have systemctl && systemctl --user show >/dev/null 2>&1
}

# ---------------------------------------------------------------- uninstall
if [ "$UNINSTALL" -eq 1 ]; then
    if user_systemd; then
        systemctl --user disable --now clipsterd.service >/dev/null 2>&1 || true
    fi
    for f in "$BIN_DIR/clipster" "$BIN_DIR/clipsterd" "$BIN_DIR/clipster-rofi" \
             "$UNIT_DIR/clipsterd.service"; do
        [ -e "$f" ] && rm -f "$f" && info "removed $f"
    done
    rm -rf "$DOC_DIR"
    user_systemd && systemctl --user daemon-reload || true
    printf '\nclipster removed. History and config were kept:\n'
    info "$DATA_DIR"
    info "$CONFIG_DIR"
    exit 0
fi

# ------------------------------------------------------------------ fetch
case "$(uname -m)" in
    x86_64|amd64)  TARGET="x86_64-unknown-linux-musl";  DEB_ARCH="amd64" ;;
    aarch64|arm64) TARGET="aarch64-unknown-linux-musl"; DEB_ARCH="arm64" ;;
    *) die "no prebuilt binary for $(uname -m); build from source (see README)." ;;
esac

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT INT TERM

get() { # get <url> <dest>; quiet, fails on HTTP errors
    if have curl; then
        if [ -n "$TOKEN" ]; then
            curl -fsSL -H "Authorization: Bearer $TOKEN" -o "$2" "$1"
        else
            curl -fsSL -o "$2" "$1"
        fi
    elif have wget; then
        if [ -n "$TOKEN" ]; then
            wget -qO "$2" --header="Authorization: Bearer $TOKEN" "$1"
        else
            wget -qO "$2" "$1"
        fi
    else
        die "need curl or wget to download clipster."
    fi
}

resolve_version() {
    if get "https://api.github.com/repos/$REPO/releases/latest" "$STAGE/rel.json" 2>/dev/null; then
        sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$STAGE/rel.json" | head -1
    elif have gh; then
        gh release view --repo "$REPO" --json tagName -q .tagName 2>/dev/null || true
    fi
}

if [ -n "$TARBALL" ]; then
    [ -f "$TARBALL" ] || die "CLIPSTER_TARBALL is not a file: $TARBALL"
    cp "$TARBALL" "$STAGE/clipster.tar.gz"
    printf 'Installing clipster from %s\n' "$TARBALL"
else
    if [ "$VERSION" = "latest" ]; then
        TAG="$(resolve_version)"
        [ -n "$TAG" ] || die "could not find a release. If $REPO is private, set GITHUB_TOKEN or run \`gh auth login\`."
    else
        TAG="v${VERSION#v}"
    fi
    V="${TAG#v}"
    NAME="clipster-$V-$TARGET.tar.gz"
    SUMS="SHA256SUMS-$DEB_ARCH.txt"
    BASE="https://github.com/$REPO/releases/download/$TAG"

    printf 'Installing clipster %s (%s)\n' "$TAG" "$TARGET"
    if ! get "$BASE/$NAME" "$STAGE/clipster.tar.gz" 2>/dev/null; then
        # Private repos serve release assets only through the API, which `gh`
        # already knows how to talk to.
        if have gh && gh release download "$TAG" --repo "$REPO" --pattern "$NAME" --dir "$STAGE" >/dev/null 2>&1; then
            mv "$STAGE/$NAME" "$STAGE/clipster.tar.gz"
            gh release download "$TAG" --repo "$REPO" --pattern "$SUMS" --dir "$STAGE" >/dev/null 2>&1 || true
        else
            die "could not download $NAME from $TAG. If $REPO is private, set GITHUB_TOKEN or run \`gh auth login\`."
        fi
    else
        get "$BASE/$SUMS" "$STAGE/$SUMS" 2>/dev/null || true
    fi

    # A mismatch means a corrupt or tampered download and is fatal; a missing
    # checksum file only costs the extra check.
    if [ -f "$STAGE/$SUMS" ] && have sha256sum; then
        want="$(sed -n "s/^\([0-9a-f]\{64\}\)  *$NAME\$/\1/p" "$STAGE/$SUMS" | head -1)"
        if [ -n "$want" ]; then
            got="$(sha256sum "$STAGE/clipster.tar.gz" | cut -d' ' -f1)"
            [ "$want" = "$got" ] || die "checksum mismatch for $NAME (expected $want, got $got)."
            info "checksum ok"
        fi
    else
        warn "skipping checksum verification (no $SUMS published for this release)."
    fi
fi

# ---------------------------------------------------------------- install
mkdir -p "$STAGE/x"
tar -xzf "$STAGE/clipster.tar.gz" -C "$STAGE/x" --strip-components=1

for bin in clipster clipsterd; do
    [ -f "$STAGE/x/$bin" ] || die "release tarball is missing $bin."
done

install -Dm755 "$STAGE/x/clipster"  "$BIN_DIR/clipster"
install -Dm755 "$STAGE/x/clipsterd" "$BIN_DIR/clipsterd"
install -Dm755 "$STAGE/x/clipster-rofi.sh" "$BIN_DIR/clipster-rofi"
install -Dm644 "$STAGE/x/config.example.toml" "$DOC_DIR/config.example.toml"
install -Dm644 "$STAGE/x/README.md" "$DOC_DIR/README.md"
info "installed clipster, clipsterd, clipster-rofi to $BIN_DIR"

# The shipped unit assumes ~/.local/bin; point it at wherever we actually put
# the daemon. contrib/clipsterd.service stays the single source for the rest.
install -Dm644 "$STAGE/x/clipsterd.service" "$UNIT_DIR/clipsterd.service"
sed -i "s|^ExecStart=.*|ExecStart=$BIN_DIR/clipsterd|" "$UNIT_DIR/clipsterd.service"

# ReadWritePaths= refuses to start the unit if the directory does not exist,
# so create it before the first start rather than after the first failure.
mkdir -p "$DATA_DIR" "$CONFIG_DIR"
if [ ! -e "$CONFIG_DIR/config.toml" ]; then
    cp "$STAGE/x/config.example.toml" "$CONFIG_DIR/config.toml"
    info "wrote default config to $CONFIG_DIR/config.toml"
fi

# ---------------------------------------------------------------- service
STARTED=0
if [ "$WANT_SERVICE" -eq 1 ] && user_systemd; then
    systemctl --user daemon-reload
    if systemctl --user enable --now clipsterd.service >/dev/null 2>&1; then
        # On a reinstall the daemon is already up on the old binary and the
        # old unit, and `enable --now` leaves a running service alone. Restart
        # so an upgrade actually takes effect.
        systemctl --user try-restart clipsterd.service >/dev/null 2>&1 || true
        STARTED=1
        info "clipsterd enabled and started"
    else
        warn "could not start clipsterd; check \`systemctl --user status clipsterd\`."
    fi
fi

# The local-tarball path never resolved a tag, so ask the binary itself.
V="$("$BIN_DIR/clipster" --version 2>/dev/null || true)"; V="${V##* }"
printf '\nclipster %s is installed.\n\n' "${V:-}"
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) printf 'Add it to your PATH first:\n\n    export PATH="%s:$PATH"\n\n' "$BIN_DIR" ;;
esac
if [ "$STARTED" -eq 0 ]; then
    if [ "$IS_ROOT" -eq 1 ]; then
        printf 'Start it as your own user (it is a systemd *user* service):\n\n'
    else
        printf 'Start the daemon:\n\n'
    fi
    printf '    systemctl --user daemon-reload\n    systemctl --user enable --now clipsterd\n\n'
fi
printf 'Then:\n\n    clipster status\n    clipster list\n\n'
printf 'Bind the picker to a hotkey, e.g. i3/sway:\n\n    bindsym $mod+v exec --no-startup-id %s/clipster-rofi\n\n' "$BIN_DIR"
printf 'Config: %s/config.toml   Uninstall: sh install.sh --uninstall\n' "$CONFIG_DIR"
