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

command -v curl >/dev/null 2>&1 || die "curl was not found on your PATH (macOS ships it as /usr/bin/curl)"
command -v shasum >/dev/null 2>&1 || die "shasum was not found on your PATH (macOS ships it as /usr/bin/shasum)"

tmp="$(mktemp -d 2>/dev/null || mktemp -d -t zenteru)" || die "couldn't create a temporary folder (is the disk full?)"
trap 'rm -rf "$tmp"' EXIT INT TERM
fetch() { curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"; }

mkdir -p "$APP_DIR" || die "couldn't create $APP_DIR; pick another place with ZENTERU_APP_DIR=/path"
[ -w "$APP_DIR" ] || die "$APP_DIR is not writable; fix its permissions or pick another place with ZENTERU_APP_DIR=/path"

say "downloading $ASSET ($VERSION)"
fetch "$base/$ASSET" "$tmp/$ASSET" ||
    die "couldn't download $base/$ASSET (curl's reason is above). Check your connection; if you asked for a version, check that the release exists: https://github.com/$REPO/releases"
fetch "$base/$ASSET.sha256" "$tmp/$ASSET.sha256" || die "couldn't download the checksum $base/$ASSET.sha256 (curl's reason is above)"
expected="$(cut -d ' ' -f 1 <"$tmp/$ASSET.sha256")"
actual="$(shasum -a 256 "$tmp/$ASSET" | cut -d ' ' -f 1)"
[ "$expected" = "$actual" ] || die "the download is damaged or was altered on the way (SHA-256 is $actual, the release says $expected). Nothing was installed; try again"

ditto -x -k "$tmp/$ASSET" "$tmp/x" || die "couldn't unpack $ASSET (is the disk full?)"
[ -d "$tmp/x/zenteru.app" ] || die "archive does not contain zenteru.app"
codesign --verify --strict "$tmp/x/zenteru.app" 2>/dev/null || die "zenteru.app signature does not verify. Nothing was installed"

if [ -d "$APP_DIR/zenteru.app" ]; then
    pgrep -f "$APP_DIR/zenteru.app/Contents/MacOS/zenteru" >/dev/null 2>&1 &&
        say "zenteru is running; the new version takes effect after you quit and reopen it"
    rm -rf "$APP_DIR/zenteru.app.old"
    mv "$APP_DIR/zenteru.app" "$APP_DIR/zenteru.app.old" || die "couldn't move the old $APP_DIR/zenteru.app aside. Nothing was changed"
fi
if ! ditto "$tmp/x/zenteru.app" "$APP_DIR/zenteru.app"; then
    # Put the old version back rather than leave half a copy.
    rm -rf "$APP_DIR/zenteru.app"
    [ -d "$APP_DIR/zenteru.app.old" ] && mv "$APP_DIR/zenteru.app.old" "$APP_DIR/zenteru.app"
    die "couldn't copy zenteru.app into $APP_DIR (is the disk full?). The previous version was kept"
fi
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
        *":$BIN_DIR:"*) say "run \`zenteru\` to open it, \`zenteru update\` to update it, \`zenteru sudo-auth\` for Touch ID / YubiKey sudo" ;;
        *)
            say "linked $BIN_DIR/zenteru, but $BIN_DIR is not on your PATH. Add this to your shell profile:"
            printf '\n    export PATH="%s:$PATH"\n\n' "$BIN_DIR"
            ;;
    esac
fi
