#!/bin/bash
set -euo pipefail

cd "$(dirname "$0")/.."
: "${CODESIGN_IDENTITY:?Set CODESIGN_IDENTITY to your Developer ID Application identity}"
: "${NOTARY_PROFILE:?Set NOTARY_PROFILE to your notarytool Keychain profile}"
[[ $(uname -sm) == 'Darwin arm64' ]] || { echo 'Use an Apple Silicon Mac.' >&2; exit 1; }

CARGO_PROFILE=release bash scripts/package-macos.sh
app='target/macos/Prophet-6 Patch Librarian.app'
submission='target/macos/notary-submission.zip'
archive='target/macos/Prophet-6-Patch-Librarian-macOS-arm64.zip'
rm -f "$submission" "$archive"
ditto -c -k --keepParent "$app" "$submission"
xcrun notarytool submit "$submission" --keychain-profile "$NOTARY_PROFILE" --wait
xcrun stapler staple "$app"
xcrun stapler validate "$app"
codesign --verify --deep --strict "$app"
spctl --assess --type execute --verbose=2 "$app"
ditto -c -k --keepParent "$app" "$archive"
unzip -tq "$archive"
shasum -a 256 "$archive"
