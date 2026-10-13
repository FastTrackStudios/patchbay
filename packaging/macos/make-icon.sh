#!/usr/bin/env bash
# Regenerate AppIcon.icns from app/assets/icon.svg. Only needed when the
# SVG changes — the .icns is checked in so builds need no converter.
# Needs rsvg-convert (brew install librsvg); sips/iconutil ship with macOS.
# The artwork is inset to Apple's 824/1024 icon grid so macOS doesn't
# shrink it into a grey "legacy icon" plate.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
svg="$here/../../app/assets/icon.svg"
set_dir="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$set_dir"
mk() {
    local out=$1 px=$2 inner
    inner=$(( (px * 824 + 512) / 1024 ))
    rsvg-convert -w "$inner" -h "$inner" "$svg" -o "$set_dir/tmp.png"
    sips -p "$px" "$px" "$set_dir/tmp.png" --out "$set_dir/$out" >/dev/null
}
for sz in 16 32 128 256 512; do
    mk "icon_${sz}x${sz}.png" "$sz"
    mk "icon_${sz}x${sz}@2x.png" $((sz * 2))
done
rm "$set_dir/tmp.png"
iconutil -c icns "$set_dir" -o "$here/AppIcon.icns"
echo "wrote $here/AppIcon.icns"
