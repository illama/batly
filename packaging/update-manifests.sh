#!/bin/sh
# Fills the package manifest templates (winget, Scoop, Chocolatey) for a
# release, from the files in dist/ (see build.sh or the release workflow).
#
#   packaging/update-manifests.sh <version> [dist-dir] [out-dir]
#
# Output (default: dist/packages):
#   winget/manifests/i/illama/LGSTray/<version>/*.yaml   -> microsoft/winget-pkgs
#   scoop/lgstray.json                                   -> a Scoop bucket
#   chocolatey/lgstray.nuspec, chocolatey/tools/*.ps1    -> choco pack / push
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION=${1:?usage: $0 <version> [dist-dir] [out-dir]}
VERSION=${VERSION#v}
DIST=${2:-$ROOT/dist}
OUT=${3:-$DIST/packages}
SETUP="LGSTray-Setup-$VERSION.exe"
ZIP="LGSTray-$VERSION-portable.zip"

case $VERSION in
  *[!0-9.]* | "" | .* | *. ) echo "invalid version: $VERSION" >&2; exit 1 ;;
esac

sha256() {
  # Prefer the published SHA256SUMS.txt (what the users verify against).
  if [ -f "$DIST/SHA256SUMS.txt" ]; then
    h=$(awk -v f="$1" '{ n = $2; sub(/^\*/, "", n); if (n == f) print tolower($1) }' "$DIST/SHA256SUMS.txt")
    if [ -n "$h" ]; then echo "$h"; return; fi
  fi
  [ -f "$DIST/$1" ] || { echo "missing $DIST/$1" >&2; exit 1; }
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$DIST/$1" | cut -d' ' -f1
  else
    shasum -a 256 "$DIST/$1" | cut -d' ' -f1
  fi
}

SETUP_SHA256=$(sha256 "$SETUP")
ZIP_SHA256=$(sha256 "$ZIP")
# Check the listed sums against the files when both are there.
for f in "$SETUP:$SETUP_SHA256" "$ZIP:$ZIP_SHA256"; do
  name=${f%%:*}; want=${f#*:}
  if [ -f "$DIST/$name" ]; then
    got=$( (command -v sha256sum >/dev/null 2>&1 && sha256sum "$DIST/$name" || shasum -a 256 "$DIST/$name") | cut -d' ' -f1)
    [ "$got" = "$want" ] || { echo "checksum mismatch for $name: $got != $want" >&2; exit 1; }
  fi
done
RELEASE_DATE=${RELEASE_DATE:-$(date -u +%Y-%m-%d)}

fill() {
  mkdir -p "$(dirname "$2")"
  sed -e "s/{{VERSION}}/$VERSION/g" \
      -e "s/{{SETUP_SHA256}}/$(echo "$SETUP_SHA256" | tr a-f A-F)/g" \
      -e "s/{{ZIP_SHA256}}/$ZIP_SHA256/g" \
      -e "s/{{RELEASE_DATE}}/$RELEASE_DATE/g" \
      -e '/^# Template: /d' -e '/^<!-- Template: /d' \
      "$1" > "$2"
  if grep -q '{{' "$2"; then echo "unfilled placeholder in $2" >&2; exit 1; fi
}

rm -rf "$OUT"
WINGET="$OUT/winget/manifests/i/illama/LGSTray/$VERSION"
for f in "$ROOT"/packaging/winget/*.yaml; do
  fill "$f" "$WINGET/$(basename "$f")"
done
fill "$ROOT/packaging/scoop/lgstray.json" "$OUT/scoop/lgstray.json"
fill "$ROOT/packaging/chocolatey/lgstray.nuspec" "$OUT/chocolatey/lgstray.nuspec"
for f in "$ROOT"/packaging/chocolatey/tools/*.ps1; do
  fill "$f" "$OUT/chocolatey/tools/$(basename "$f")"
done
# Chocolatey's checksum is lowercase (both work, keep it readable).
sed -i.bak "s/$(echo "$SETUP_SHA256" | tr a-f A-F)/$SETUP_SHA256/" "$OUT/chocolatey/tools/chocolateyinstall.ps1"
rm -f "$OUT/chocolatey/tools/chocolateyinstall.ps1.bak"

echo "Manifests for LGSTray $VERSION in $OUT"
echo "  setup  $SETUP_SHA256"
echo "  zip    $ZIP_SHA256"
find "$OUT" -type f | sort
