#!/bin/bash
set -euo pipefail

[[ $(uname -sm) == 'Linux x86_64' ]] || { echo 'Build on x86-64 Linux.' >&2; exit 1; }
cd "$(dirname "$0")/.."

binary=prophet-6-patch-librarian
name=Prophet-6-Patch-Librarian
cargo build --locked --release --target-dir target
stage="target/linux/$name"
mkdir -p "$stage"
install -m 755 "target/release/$binary" "$stage/$binary"
strip --strip-unneeded "$stage/$binary"
install -m 644 LICENSE.md "$stage/LICENSE.md"
archive="$name-Linux-x86_64.tar.gz"
tar -C target/linux -czf "target/linux/$archive" "$name"
sha256sum "target/linux/$archive"
