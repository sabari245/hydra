#!/bin/sh
# Hydra STT installer.
#
#   curl -fsSL https://github.com/sabari245/hydra/releases/latest/download/install.sh | sh
#
# Options (also settable through environment variables):
#   --version vX.Y.Z   Install a specific release   (HYDRA_STT_VERSION, default: latest)
#   --prefix DIR       Install to DIR/bin           (HYDRA_STT_PREFIX, default: ~/.local)
#   --uninstall        Remove the installed binary, menu entry and login service
# HYDRA_STT_BASE_URL overrides where release files are downloaded from.

set -eu

REPO="sabari245/hydra"
VERSION="${HYDRA_STT_VERSION:-latest}"
PREFIX="${HYDRA_STT_PREFIX:-$HOME/.local}"
UNINSTALL=0

say() { printf 'hydra-stt: %s\n' "$*"; }
fail() { printf 'hydra-stt: error: %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || fail "--version needs a value"; VERSION="$2"; shift 2 ;;
        --prefix) [ $# -ge 2 ] || fail "--prefix needs a value"; PREFIX="$2"; shift 2 ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h | --help) sed -n '2,11p' "$0" 2>/dev/null || true; exit 0 ;;
        *) fail "unknown option: $1" ;;
    esac
done

BIN_DIR="$PREFIX/bin"
APPS_DIR="$PREFIX/share/applications"
UNIT="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/hydra-stt.service"

if [ "$UNINSTALL" -eq 1 ]; then
    [ -x "$BIN_DIR/hydra-stt" ] && "$BIN_DIR/hydra-stt" --stop 2>/dev/null || true
    if [ -f "$UNIT" ]; then
        systemctl --user disable hydra-stt.service 2>/dev/null || true
        rm -f "$UNIT"
    fi
    rm -f "$BIN_DIR/hydra-stt" "$APPS_DIR/hydra-stt.desktop"
    say "removed $BIN_DIR/hydra-stt"
    say "configuration (~/.config/hydra-stt) and logs (~/.local/state/hydra-stt) were kept"
    exit 0
fi

[ "$(uname -s)" = "Linux" ] || fail "Hydra STT only supports Linux"
case "$(uname -m)" in
    x86_64 | amd64) TARGET="x86_64-unknown-linux-gnu" ;;
    *) fail "Hydra STT only supports x86_64, not $(uname -m)" ;;
esac

if [ -n "${HYDRA_STT_BASE_URL:-}" ]; then
    BASE_URL="$HYDRA_STT_BASE_URL"
elif [ "$VERSION" = "latest" ]; then
    BASE_URL="https://github.com/$REPO/releases/latest/download"
else
    BASE_URL="https://github.com/$REPO/releases/download/$VERSION"
fi

if has curl; then
    download() { curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"; }
elif has wget; then
    download() { wget -q --https-only -O "$2" "$1"; }
else
    fail "curl or wget is required"
fi
has tar || fail "tar is required"

ARCHIVE="hydra-stt-$TARGET.tar.gz"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
trap 'exit 1' INT TERM

say "downloading $ARCHIVE ($VERSION)"
download "$BASE_URL/$ARCHIVE" "$TMP/$ARCHIVE" || fail "could not download $BASE_URL/$ARCHIVE"

if download "$BASE_URL/$ARCHIVE.sha256" "$TMP/$ARCHIVE.sha256" 2>/dev/null; then
    expected="$(cut -d ' ' -f 1 "$TMP/$ARCHIVE.sha256")"
    if has sha256sum; then
        actual="$(sha256sum "$TMP/$ARCHIVE" | cut -d ' ' -f 1)"
    elif has shasum; then
        actual="$(shasum -a 256 "$TMP/$ARCHIVE" | cut -d ' ' -f 1)"
    else
        actual=""
        say "warning: no sha256 tool found; skipping checksum verification"
    fi
    if [ -n "$actual" ] && [ "$actual" != "$expected" ]; then
        fail "checksum mismatch for $ARCHIVE"
    fi
else
    say "warning: no checksum published; skipping verification"
fi

tar -xzf "$TMP/$ARCHIVE" -C "$TMP"
mkdir -p "$BIN_DIR"
install -m 755 "$TMP/hydra-stt-$TARGET/hydra-stt" "$BIN_DIR/hydra-stt"
say "installed $("$BIN_DIR/hydra-stt" --version) to $BIN_DIR/hydra-stt"

# Older releases do not ship the menu entry.
DESKTOP="$TMP/hydra-stt-$TARGET/hydra-stt.desktop"
if [ -f "$DESKTOP" ]; then
    mkdir -p "$APPS_DIR"
    sed "s|^Exec=hydra-stt\$|Exec=$BIN_DIR/hydra-stt|" "$DESKTOP" > "$APPS_DIR/hydra-stt.desktop"
    chmod 644 "$APPS_DIR/hydra-stt.desktop"
    say "added Hydra STT to your applications menu"
fi

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) say "note: $BIN_DIR is not on your PATH; add it to your shell profile" ;;
esac

missing=""
for tool in arecord paplay wtype playerctl; do
    has "$tool" || missing="$missing $tool"
done
[ -n "${WAYLAND_DISPLAY:-}" ] || say "warning: Hydra STT needs a Wayland session"
if [ -n "$missing" ]; then
    say "missing runtime tools:$missing"
    say "  Debian/Ubuntu: sudo apt install alsa-utils pulseaudio-utils wtype playerctl"
    say "  Fedora:        sudo dnf install alsa-utils pulseaudio-utils wtype playerctl"
    say "  Arch:          sudo pacman -S alsa-utils libpulse wtype playerctl"
fi

cat <<EOF

Next steps:
  1. Open Hydra STT from your applications menu (or run 'hydra-stt').
  2. Add your Groq (and optionally IsoQuant) API keys, then press Start.
  3. Bind '$BIN_DIR/hydra-stt --toggle' to a key in your Wayland compositor.
EOF
