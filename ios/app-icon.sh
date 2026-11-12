#!/usr/bin/env bash
# Regenerate the phone's app icon (Assets.xcassets/AppIcon.appiconset) from
# Patchbay's mark (apps/patchbay/assets/icon.svg, the desktop app's and the
# distribution's icon). Only needed when the SVG changes: the PNG is checked
# in so a build needs no converter. architect's apps/mobile/ios/app-icon.sh.
#
# One 1024 `universal` image, from which actool makes every size the phone
# and the iPad want. iOS masks the corners itself and the App Store rejects
# an icon with an alpha channel, so the mark's tile fills the square (its
# own rounded corners and border dropped: iOS draws the corners) and the
# image is saved opaque.
#
#   apps/patchbay/ios/app-icon.sh [icon.svg]
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
svg="${1:-$here/../assets/icon.svg}"
# The tile's gradient, between its two stops: what shows in the corners
# before iOS's mask cuts them.
bg="${ICON_BG:-#171b22}"
out="$here/Assets.xcassets/AppIcon.appiconset"
NIX="${NIX:-$(command -v nix || echo /nix/var/nix/profiles/default/bin/nix)}"

mkdir -p "$out"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/patchbay-icon.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
# The tile is the 120-unit rect at (4,4), rounded and stroked: square
# and unstroked, the view cropped to it.
sed -e 's/viewBox="0 0 128 128"/viewBox="4 4 120 120"/' \
    -e 's/ rx="26"//' -e 's/ stroke="#2b323d" stroke-width="3"//' "$svg" > "$tmp/icon.svg"
"$NIX" shell nixpkgs#librsvg -c rsvg-convert -w 1024 -h 1024 \
    --background-color "$bg" "$tmp/icon.svg" -o "$tmp/icon.png"
# Through JPEG and back: the only way sips drops the alpha channel.
sips -s format jpeg -s formatOptions 100 "$tmp/icon.png" --out "$tmp/icon.jpg" >/dev/null
sips -s format png "$tmp/icon.jpg" --out "$out/icon-1024.png" >/dev/null
cat > "$out/Contents.json" <<'JSON'
{
  "images" : [
    { "filename" : "icon-1024.png", "idiom" : "universal", "platform" : "ios", "size" : "1024x1024" }
  ],
  "info" : { "author" : "xcode", "version" : 1 }
}
JSON
echo '{ "info" : { "author" : "xcode", "version" : 1 } }' > "$here/Assets.xcassets/Contents.json"
sips -g pixelWidth -g hasAlpha "$out/icon-1024.png" | tail -2
echo "wrote $out"
