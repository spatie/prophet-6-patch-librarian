#!/bin/bash
set -euo pipefail

[[ $(uname -s) == Darwin ]] || { echo 'Build the macOS app on a Mac.' >&2; exit 1; }
cd "$(dirname "$0")/.."

profile=${CARGO_PROFILE:-release}
case "$profile" in
    dev) products=debug ;;
    release) products=release ;;
    *) echo 'CARGO_PROFILE must be dev or release.' >&2; exit 1 ;;
esac

binary=prophet-6-patch-librarian
name='Prophet-6 Patch Librarian'
version=$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)
cargo build --locked --profile "$profile" --target-dir target

app="target/macos/$name.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
/usr/bin/install -m 755 "target/$products/$binary" "$app/Contents/MacOS/$binary"
/usr/bin/install -m 644 assets/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>be.spatie.prophet6patchlibrarian</string>
    <key>CFBundleExecutable</key><string>$binary</string>
    <key>CFBundleName</key><string>$name</string>
    <key>CFBundleDisplayName</key><string>$name</string>
    <key>CFBundleIconFile</key><string>AppIcon.icns</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist"

if [[ -n ${CODESIGN_IDENTITY:-} ]]; then
    codesign --force --options runtime --timestamp --sign "$CODESIGN_IDENTITY" "$app"
else
    codesign --force --sign - "$app"
fi
codesign --verify --strict --verbose=2 "$app"
printf '\n%s\n' "$app"
