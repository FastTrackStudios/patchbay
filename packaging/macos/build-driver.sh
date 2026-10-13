#!/usr/bin/env bash
# Build + sign Patchbay.driver — the AudioServerPlugIn from crates/driver
# (loopback virtual devices, created/renamed/removed at runtime) — into
# target/macos/drivers/Patchbay.driver.
#
# Universal when the x86_64-apple-darwin target is installed, arm64 only
# otherwise. Signing: $PATCHBAY_SIGN_ID, $PATCHBAY_KEYCHAIN,
# $PATCHBAY_KEYCHAIN_PW (as sign.sh).
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
out="$root/target/macos/drivers"
bundle="$out/Patchbay.driver"
icon="$root/packaging/macos/AppIcon.icns"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
id="${PATCHBAY_SIGN_ID:-Developer ID Application: CODY JAMES WRIGHT (28C2G63DA7)}"
bid="app.fasttrackstudio.patchbay.driver"
# Factory UUID → PatchbayAudioServerPlugInFactory (the one exported symbol).
factory="6F0A7C51-2D4B-4C8E-9E4F-3B1D5A6C7E10"
# kAudioServerPlugInTypeUUID.
plugin_type="443ABAB8-E7B3-491A-B985-BEB9187030DB"

cd "$root"
targets=(aarch64-apple-darwin)
if rustup target list --installed 2>/dev/null | grep -q x86_64-apple-darwin; then
    targets+=(x86_64-apple-darwin)
fi
libs=()
for t in "${targets[@]}"; do
    echo "==> cargo build --release -p patchbay-driver --target $t"
    cargo build --release -p patchbay-driver --target "$t"
    libs+=("target/$t/release/libpatchbay_driver.dylib")
done

rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
lipo -create "${libs[@]}" -output "$bundle/Contents/MacOS/Patchbay"
cp "$icon" "$bundle/Contents/Resources/Patchbay.icns"
cat >"$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key><string>English</string>
	<key>CFBundleExecutable</key><string>Patchbay</string>
	<key>CFBundleIconFile</key><string>Patchbay.icns</string>
	<key>CFBundleIdentifier</key><string>$bid</string>
	<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
	<key>CFBundleName</key><string>Patchbay</string>
	<key>CFBundlePackageType</key><string>BNDL</string>
	<key>CFBundleShortVersionString</key><string>$version</string>
	<key>CFBundleVersion</key><string>$version</string>
	<key>CFBundleSignature</key><string>????</string>
	<key>CFPlugInFactories</key>
	<dict>
		<key>$factory</key><string>PatchbayAudioServerPlugInFactory</string>
	</dict>
	<key>CFPlugInTypes</key>
	<dict>
		<key>$plugin_type</key>
		<array><string>$factory</string></array>
	</dict>
</dict>
</plist>
PLIST

keychain="${PATCHBAY_KEYCHAIN:-fts-build.keychain}"
keychain_path="$HOME/Library/Keychains/${keychain}-db"
kc_args=()
if [[ -f "$keychain_path" ]]; then
    if [[ -n "${PATCHBAY_KEYCHAIN_PW:-}" ]]; then
        security unlock-keychain -p "$PATCHBAY_KEYCHAIN_PW" "$keychain"
    else
        security unlock-keychain "$keychain"
    fi
    kc_args=(--keychain "$keychain_path")
fi
codesign --force --options runtime --timestamp ${kc_args[@]+"${kc_args[@]}"} --sign "$id" "$bundle"
codesign --verify --strict "$bundle"
echo "built: $bundle ($(lipo -archs "$bundle/Contents/MacOS/Patchbay"))"
