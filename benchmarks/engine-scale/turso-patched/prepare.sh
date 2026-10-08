#!/bin/sh
# Copies pristine turso_core 0.8.1 from the cargo registry into vendor/ and applies the patch.
set -eu
cd "$(dirname "$0")"
version=0.8.1
registry="${CARGO_HOME:-$HOME/.cargo}/registry/src"
find_source() { find "$registry" -maxdepth 2 -type d -name "turso_core-$version" 2>/dev/null | head -n 1; }
source=$(find_source)
if [ -z "$source" ]; then
    cargo fetch --locked --manifest-path ../Cargo.toml
    source=$(find_source)
fi
[ -n "$source" ] || { echo "turso_core-$version is not in $registry" >&2; exit 1; }
rm -rf vendor/turso_core
mkdir -p vendor
cp -R "$source" vendor/turso_core
rm -f vendor/turso_core/.cargo-ok
patch -s -p1 -d vendor/turso_core < root-page-lookup.patch
# Its own workspace root, so `cargo test` works inside it.
printf '\n[workspace]\n' >> vendor/turso_core/Cargo.toml
echo "vendor/turso_core: $version with root-page-lookup.patch"
