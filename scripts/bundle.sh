#!/bin/bash
# Build Petal.app (and optionally Petal.dmg) in dist/.
#
#   scripts/bundle.sh            # ad-hoc signed: runs on this Mac
#   scripts/bundle.sh --dmg      # ...plus a drag-to-Applications disk image
#
# For other people's Macs it must be signed with a Developer ID and notarized:
#   PETAL_SIGN_ID="Developer ID Application: Your Name (TEAMID)" \
#   PETAL_NOTARY_PROFILE=petal-notary scripts/bundle.sh --dmg
# where the profile was saved once with
#   xcrun notarytool store-credentials petal-notary --apple-id you@example.com --team-id TEAMID
set -euo pipefail
cd "$(dirname "$0")/.."

DMG=false
[[ "${1:-}" == "--dmg" ]] && DMG=true
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 1)
APP=dist/Petal.app

# Rust embeds source paths (for panic messages); keep this machine's paths out of the
# shipped binary. Its own target directory, so the different flags don't force normal
# builds to recompile.
# (The encoded form, separated by 0x1f, copes with spaces in the paths.)
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=/build"$'\x1f'"--remap-path-prefix=$PWD=/petal"
cargo build --release --target-dir target/bundle
rm -rf "$APP" && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/bundle/release/petal "$APP/Contents/MacOS/petal"
[[ -f packaging/AppIcon.icns ]] || python3 scripts/make-icon.py packaging
cp packaging/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
sed -e "s/__VERSION__/$VERSION/" -e "s/__BUILD__/$BUILD/" packaging/Info.plist > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" >/dev/null

if [[ -n "${PETAL_SIGN_ID:-}" ]]; then
    codesign --force --options runtime --timestamp --entitlements packaging/Petal.entitlements --sign "$PETAL_SIGN_ID" "$APP"
else
    # Ad-hoc: fine on this Mac. Note that macOS ties Full Disk Access to the signature,
    # so an ad-hoc build may need access granted again after each rebuild.
    codesign --force --sign - "$APP"
fi
codesign --verify --strict "$APP"
echo "built $APP ($VERSION, build $BUILD)"

if $DMG; then
    STAGE=$(mktemp -d)
    cp -R "$APP" "$STAGE/"
    ln -s /Applications "$STAGE/Applications"
    rm -f dist/Petal.dmg
    hdiutil create -quiet -volname Petal -srcfolder "$STAGE" -ov -format UDZO dist/Petal.dmg
    rm -rf "$STAGE"
    [[ -n "${PETAL_SIGN_ID:-}" ]] && codesign --force --sign "$PETAL_SIGN_ID" dist/Petal.dmg
    echo "built dist/Petal.dmg"
    if [[ -n "${PETAL_NOTARY_PROFILE:-}" ]]; then
        xcrun notarytool submit dist/Petal.dmg --keychain-profile "$PETAL_NOTARY_PROFILE" --wait
        xcrun stapler staple dist/Petal.dmg
        echo "notarized and stapled dist/Petal.dmg"
    fi
fi
