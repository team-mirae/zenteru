#!/bin/sh
# zenteru installer (macOS).
#
#   curl -fsSL https://raw.githubusercontent.com/team-mirae/zenteru/main/install.sh | sh
#
# Installs the latest release's zenteru.app into /Applications (or
# ~/Applications when /Applications isn't writable) and links the
# `zenteru` command into ~/.local/bin. No sudo needed. Environment:
#
#   ZENTERU_VERSION=v0.1.0      install that release instead of the latest
#   ZENTERU_APP_DIR=/path       where zenteru.app goes
#   ZENTERU_BIN_DIR=/path       where the `zenteru` symlink goes ("" to skip)

set -eu

REPO="team-mirae/zenteru"
VERSION="${ZENTERU_VERSION:-latest}"
ASSET="zenteru-macos-universal.zip"

say() { printf 'zenteru: %s\n' "$*"; }
die() { printf 'zenteru: error: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "zenteru is a macOS app; see https://github.com/$REPO"
major=$(sw_vers -productVersion | cut -d. -f1)
[ "$major" -ge 13 ] 2>/dev/null || die "macOS 13 or later is required"

if [ -n "${ZENTERU_APP_DIR:-}" ]; then
    APP_DIR="$ZENTERU_APP_DIR"
elif [ -w /Applications ]; then
    APP_DIR=/Applications
else
    APP_DIR="$HOME/Applications"
fi
BIN_DIR="${ZENTERU_BIN_DIR-$HOME/.local/bin}"

if [ "$VERSION" = "latest" ]; then
    base="https://github.com/$REPO/releases/latest/download"
else
    base="https://github.com/$REPO/releases/download/$VERSION"
fi

tmp="$(mktemp -d 2>/dev/null || mktemp -d -t zenteru)"
trap 'rm -rf "$tmp"' EXIT INT TERM
fetch() { curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"; }

say "downloading $ASSET ($VERSION)"
fetch "$base/$ASSET" "$tmp/$ASSET" || die "download failed: $base/$ASSET"
fetch "$base/$ASSET.sha256" "$tmp/$ASSET.sha256" || die "checksum download failed"
expected="$(cut -d ' ' -f 1 <"$tmp/$ASSET.sha256")"
actual="$(shasum -a 256 "$tmp/$ASSET" | cut -d ' ' -f 1)"
[ "$expected" = "$actual" ] || die "checksum mismatch for $ASSET"

ditto -x -k "$tmp/$ASSET" "$tmp/x"
[ -d "$tmp/x/zenteru.app" ] || die "archive does not contain zenteru.app"
codesign --verify --strict "$tmp/x/zenteru.app" 2>/dev/null || die "zenteru.app signature does not verify"

mkdir -p "$APP_DIR"
if [ -d "$APP_DIR/zenteru.app" ]; then
    pgrep -f "$APP_DIR/zenteru.app/Contents/MacOS/zenteru" >/dev/null 2>&1 &&
        say "zenteru is running; the new version takes effect after you quit and reopen it"
    rm -rf "$APP_DIR/zenteru.app.old"
    mv "$APP_DIR/zenteru.app" "$APP_DIR/zenteru.app.old"
fi
ditto "$tmp/x/zenteru.app" "$APP_DIR/zenteru.app"
rm -rf "$APP_DIR/zenteru.app.old"
xattr -dr com.apple.quarantine "$APP_DIR/zenteru.app" 2>/dev/null || true
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$APP_DIR/zenteru.app" 2>/dev/null || true

if [ -n "$BIN_DIR" ]; then
    mkdir -p "$BIN_DIR"
    ln -sfn "$APP_DIR/zenteru.app/Contents/MacOS/zenteru" "$BIN_DIR/zenteru"
fi

say "installed $("$APP_DIR/zenteru.app/Contents/MacOS/zenteru" --version) to $APP_DIR/zenteru.app"
if [ -n "$BIN_DIR" ]; then
    case ":$PATH:" in
        *":$BIN_DIR:"*) say "run \`zenteru\` to open it, \`zenteru sudo-auth\` for Touch ID / YubiKey sudo" ;;
        *)
            say "linked $BIN_DIR/zenteru, but $BIN_DIR is not on your PATH. Add this to your shell profile:"
            printf '\n    export PATH="%s:$PATH"\n\n' "$BIN_DIR"
            ;;
    esac
fi
