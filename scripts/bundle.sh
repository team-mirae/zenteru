#!/bin/sh
# Build dist/zenteru.app and sign it.
#
#   scripts/bundle.sh            release build, signed with the first
#                                "Apple Development" identity in the keychain
#   ZENTERU_SIGN_IDENTITY="Developer ID Application: …" scripts/bundle.sh
#   ZENTERU_SIGN_IDENTITY=- scripts/bundle.sh   (ad hoc)
set -eu
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release -p zenteru-app

APP=dist/zenteru.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/zenteru "$APP/Contents/MacOS/zenteru"
sed "s/__VERSION__/$VERSION/g" resources/Info.plist > "$APP/Contents/Info.plist"
[ -f resources/zenteru.icns ] && cp resources/zenteru.icns "$APP/Contents/Resources/" && \
    /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string zenteru" "$APP/Contents/Info.plist"

# Runtime resources: plugins, the Python API, shell integration.
for d in plugins python shell; do
    rsync -a --exclude '__pycache__' --exclude 'tests' --exclude '*.pyc' "$d/" "$APP/Contents/Resources/$d/"
done
cp LICENSE "$APP/Contents/Resources/"

IDENTITY="${ZENTERU_SIGN_IDENTITY:-}"
if [ -z "$IDENTITY" ]; then
    IDENTITY=$(security find-identity -v -p codesigning | sed -n 's/.*"\(Apple Development:[^"]*\)".*/\1/p' | head -1)
fi
if [ -z "$IDENTITY" ]; then
    echo "No signing identity found; signing ad hoc." >&2
    IDENTITY=-
fi
codesign --force --deep --options runtime --timestamp=none --sign "$IDENTITY" "$APP"
codesign --verify --strict --verbose=2 "$APP"
echo "Built $APP ($VERSION), signed by: $IDENTITY"
