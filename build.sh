#!/bin/sh
# Builds Batly for Windows (x86-64) from Linux, macOS or WSL and produces
#   dist/Batly-Setup-<version>.exe      installer
#   dist/Batly-<version>-portable.zip   portable archive
#   dist/SHA256SUMS.txt                   checksums (used by the in-app updater)
#   dist/packages/                        winget / Scoop / Chocolatey manifests
#
# Requirements: rustup (target x86_64-pc-windows-gnu), zig, cargo-zigbuild.
# On Windows, plain `cargo build --release` works as well (MSVC toolchain).
set -eu
cd "$(dirname "$0")"

TARGET=x86_64-pc-windows-gnu
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
OUT=target/$TARGET/release

cargo test -p batly
cargo zigbuild --release --target $TARGET -p batly
BATLY_PAYLOAD="$PWD/$OUT/batly.exe" cargo zigbuild --release --target $TARGET -p batly-setup

rm -rf dist
mkdir -p dist/portable
cp "$OUT/batly-setup.exe" "dist/Batly-Setup-$VERSION.exe"
cp "$OUT/batly.exe" packaging/appsettings.toml README.md LICENSE dist/portable/
python3 - "$VERSION" <<'PY'
import sys, zipfile, pathlib
version = sys.argv[1]
with zipfile.ZipFile(f"dist/Batly-{version}-portable.zip", "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    for f in sorted(pathlib.Path("dist/portable").iterdir()):
        z.write(f, f"Batly/{f.name}")
PY
rm -rf dist/portable
(cd dist && sha256sum * > SHA256SUMS.txt)
sh packaging/update-manifests.sh "$VERSION" dist dist/packages >/dev/null
ls -l dist
