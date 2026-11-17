#!/usr/bin/env bash
# Patchbay for iPhone and iPad as an app bundle, without dx: cargo builds
# `patchbay-ios` (fts-patchbay, `--no-default-features --features ios`), and
# this folder's Info.plist, its asset catalog (the app icon and the launch
# screen's colour, compiled by actool) and the binary go into
# `<target>/<triple>/<profile>/Patchbay.app`. It prints the bundle's path.
# architect's apps/mobile/ios/build-app.sh, for Patchbay.
#
#   apps/patchbay/ios/build-app.sh [--device] [cargo build flags…]   (in `devenv shell`)
#   apps/patchbay/ios/build-app.sh --device --release
#
# By default the bundle is for the Simulator (aarch64-apple-ios-sim),
# signed ad hoc. `--device` builds for a phone (aarch64-apple-ios) and
# leaves the bundle unsigned: deploy-testflight.sh signs it for the App
# Store.
#
# The dev shell's C toolchain is Nix's, for macOS: its xcrun knows no iOS
# SDK and its SDKROOT and flags are the Mac's. A crate's C code for the
# phone is built with Xcode's clang and xcrun instead; the host's build
# scripts keep the shell's. The shell's pkg-config answers with the Mac's
# libraries, so for the phone it answers nothing.
#
# XCODE_DIR: an Xcode other than xcode-select's. ACTOOL_DEVELOPER_DIR: an
# Xcode whose actool runs on this macOS, when the build's own cannot.
set -euo pipefail

die() { echo "build-app: $*" >&2; exit 1; }

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
# Info.plist's MinimumOSVersion.
min_ios="$(/usr/libexec/PlistBuddy -c 'Print :MinimumOSVersion' "$here/Info.plist")"
bin=patchbay-ios
app_name=Patchbay

triple=aarch64-apple-ios-sim
sdk_name=iphonesimulator
cargo_args=()
for a in "$@"; do
    if [[ "$a" == "--device" ]]; then
        triple=aarch64-apple-ios
        sdk_name=iphoneos
    else
        cargo_args+=("$a")
    fi
done
profile=debug
if [[ " ${cargo_args[*]-} " == *" --release "* ]]; then profile=release; fi
t="${triple//-/_}"
T="$(echo "$t" | tr '[:lower:]' '[:upper:]')"

xcode_bin="$(mktemp -d "${TMPDIR:-/tmp}/patchbay-ios.XXXXXX")"
trap 'rm -rf "$xcode_bin"' EXIT
ln -s /usr/bin/xcrun "$xcode_bin/xcrun"

# The shell's DEVELOPER_DIR and SDKROOT are its Nix SDK's (the Mac's);
# Xcode's tools follow them, so they go first.
unset SDKROOT
if [[ -n "${XCODE_DIR:-}" ]]; then export DEVELOPER_DIR="$XCODE_DIR"; else unset DEVELOPER_DIR; fi
dev="${XCODE_DIR:-$(/usr/bin/xcode-select -p)}"
toolchain="$dev/Toolchains/XcodeDefault.xctoolchain"
sdk="$(/usr/bin/xcrun --sdk "$sdk_name" --show-sdk-path)"

env -u NIX_CFLAGS_COMPILE -u NIX_LDFLAGS -u MACOSX_DEPLOYMENT_TARGET \
    PATH="$xcode_bin:$PATH" \
    IPHONEOS_DEPLOYMENT_TARGET="$min_ios" \
    "PKG_CONFIG_$t=/usr/bin/false" \
    LIBCLANG_PATH="$toolchain/usr/lib" \
    "BINDGEN_EXTRA_CLANG_ARGS_$t=-isysroot $sdk" \
    "CC_$t=/usr/bin/clang" \
    "CXX_$t=/usr/bin/clang++" \
    "AR_$t=/usr/bin/ar" \
    "CARGO_TARGET_${T}_LINKER=/usr/bin/clang" \
    cargo build -p fts-patchbay --bin "$bin" --no-default-features --features ios \
        --target "$triple" ${cargo_args[@]+"${cargo_args[@]}"} >&2

app="$target_dir/$triple/$profile/$app_name.app"
rm -rf "$app"
mkdir -p "$app"
cp "$here/Info.plist" "$app/Info.plist"
cp "$target_dir/$triple/$profile/$bin" "$app/$bin"

# The app icon and the launch screen's colour (Info.plist's UILaunchScreen
# names it) into Assets.car; actool's partial plist adds CFBundleIcons.
icon_dev="${ACTOOL_DEVELOPER_DIR:-$dev}"
partial="$xcode_bin/icon.plist"
DEVELOPER_DIR="$icon_dev" /usr/bin/xcrun actool "$here/Assets.xcassets" --compile "$app" \
    --platform "$sdk_name" --minimum-deployment-target "$min_ios" \
    --target-device iphone --target-device ipad --app-icon AppIcon \
    --output-partial-info-plist "$partial" >"$xcode_bin/actool.log" 2>&1 \
    || { tail -20 "$xcode_bin/actool.log" >&2; die "actool failed"; }
[[ -f "$app/Assets.car" ]] || { tail -20 "$xcode_bin/actool.log" >&2; die "actool wrote no Assets.car (set ACTOOL_DEVELOPER_DIR)"; }
/usr/libexec/PlistBuddy -c "Merge $partial" "$app/Info.plist" >/dev/null 2>&1 || true

if [[ "$triple" == aarch64-apple-ios-sim ]]; then
    /usr/bin/codesign --force --sign - --timestamp=none "$app" >&2
fi
echo "$app"
