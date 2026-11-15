#!/usr/bin/env bash
# Builds dist/g4v-<version>-macos.dmg: a drag-to-Applications disk image holding g4v.app.
#
# Run on a Mac with Xcode Command Line Tools and rustup:
#   installer/macos/build.sh                 universal binary (Apple Silicon + Intel)
#   installer/macos/build.sh --native        this Mac's architecture only (faster)
#
# Optional signing and notarization (otherwise the app is ad-hoc signed):
#   G4V_SIGN_IDENTITY="Developer ID Application: Name (TEAMID)"
#   G4V_NOTARY_PROFILE=<keychain profile created with `xcrun notarytool store-credentials`>
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "error: the macOS installer must be built on macOS (hdiutil, lipo and codesign are required)." >&2
    exit 1
fi

NATIVE=0
for arg in "$@"; do
    case "$arg" in
        --native) NATIVE=1 ;;
        -h|--help) sed -n '2,11p' "$0"; exit 0 ;;
        *) echo "error: unknown option $arg" >&2; exit 1 ;;
    esac
done

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
MIN_MACOS="11.0"
export MACOSX_DEPLOYMENT_TARGET="$MIN_MACOS"

# --- Build -------------------------------------------------------------------------------------------------------
BIN_DIR="target/macos"
mkdir -p "$BIN_DIR"
if [[ "$NATIVE" == 1 ]]; then
    cargo build --release --bin g4v
    cp target/release/g4v "$BIN_DIR/g4v"
else
    TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
    rustup target add "${TARGETS[@]}"
    for t in "${TARGETS[@]}"; do
        cargo build --release --bin g4v --target "$t"
    done
    lipo -create -output "$BIN_DIR/g4v" \
        target/aarch64-apple-darwin/release/g4v \
        target/x86_64-apple-darwin/release/g4v
fi
echo "Binary: $(lipo -archs "$BIN_DIR/g4v") (macOS $MIN_MACOS+)"

# --- App bundle ----------------------------------------------------------------------------------------------------
STAGE="target/macos/dmg"
APP="$STAGE/g4v.app"
rm -rf "$STAGE"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN_DIR/g4v" "$APP/Contents/MacOS/g4v"
chmod 755 "$APP/Contents/MacOS/g4v"
cp assets/g4v.icns "$APP/Contents/Resources/g4v.icns"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@MIN_MACOS@/$MIN_MACOS/g" installer/macos/Info.plist > "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# --- Sign ----------------------------------------------------------------------------------------------------------
if [[ -n "${G4V_SIGN_IDENTITY:-}" ]]; then
    codesign --force --options runtime --timestamp --sign "$G4V_SIGN_IDENTITY" "$APP"
else
    # Ad-hoc signature: required for Apple Silicon to run the binary at all. Gatekeeper will still ask the user to
    # confirm the first launch of a downloaded copy (right-click › Open).
    codesign --force --sign - "$APP"
fi
codesign --verify --strict --verbose=2 "$APP"

# --- Disk image ----------------------------------------------------------------------------------------------------
ln -s /Applications "$STAGE/Applications"
mkdir -p dist
DMG="dist/g4v-$VERSION-macos.dmg"
rm -f "$DMG"
hdiutil create -volname "g4v $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG" >/dev/null

if [[ -n "${G4V_SIGN_IDENTITY:-}" ]]; then
    codesign --force --timestamp --sign "$G4V_SIGN_IDENTITY" "$DMG"
    if [[ -n "${G4V_NOTARY_PROFILE:-}" ]]; then
        xcrun notarytool submit "$DMG" --keychain-profile "$G4V_NOTARY_PROFILE" --wait
        xcrun stapler staple "$DMG"
    fi
fi

echo "Installer: $ROOT/$DMG"
