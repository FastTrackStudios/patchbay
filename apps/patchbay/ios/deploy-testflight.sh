#!/usr/bin/env bash
# Patchbay for iPhone and iPad (apps/patchbay `patchbay-ios`) to TestFlight:
# a release build for the phone, signed for the App Store and uploaded
# through the App Store Connect API. architect's
# apps/mobile/ios/deploy-testflight.sh with Patchbay's names, and Studio's
# apps/studio/desktop/ios/deploy-testflight.sh by its environment, so one
# Mac and one runner serve all three: the API key in ~/.appstoreconnect,
# the Apple Distribution identity in a dedicated keychain
# (apps/studio/desktop/ios/setup-keychain.sh makes it), the App Store
# profile minted for the bundle id.
#
#   MARKETING_VER=26.11.2 apps/patchbay/ios/deploy-testflight.sh             build, sign, upload
#   MARKETING_VER=26.11.2 apps/patchbay/ios/deploy-testflight.sh --dry-run   build, sign, validate; no upload
#   Headless (airlock, the release job):
#   KEYCHAIN=fts-build.keychain KEYCHAIN_PW=fts-build MARKETING_VER=… apps/patchbay/ios/deploy-testflight.sh
#
# The upload needs an App Store Connect app record for the bundle id; until
# one exists, altool refuses it (and the validation) with "No suitable
# application records were found".
#
# Environment (Studio's names):
#   MARKETING_VER  CFBundleShortVersionString, the release tag's (v26.11.2 -> 26.11.2). Required.
#   BUILD_NO       CFBundleVersion; must climb on every upload. Default: unix time.
#   KEYCHAIN       the keychain holding "Apple Distribution" (default login.keychain-db);
#   KEYCHAIN_PW    unlocks it (headless).
#   ASC_KEY_ID, ASC_ISSUER_ID, ASC_KEY_PATH   the App Store Connect API key, from
#                  ~/.appstoreconnect/config.env (ASC_CONFIG) unless already set.
#   DX_BUNDLE_ID   the bundle id the profile is for (default app.fasttrackstudio.patchbay,
#                  Info.plist's).
#   PROFILE_NAME   the App Store provisioning profile (default "FTS Patchbay App Store"),
#                  found among the installed ones; MINT_PROFILE=1 mints it instead with
#                  apps/studio/desktop/ios/mint-dev-profile.rb (which deletes and
#                  re-creates a profile of that name, and registers the bundle id when
#                  it is missing).
#   TEAM_ID        default 28C2G63DA7.
#   XCODE_DIR      the Xcode to build with (…/Xcode.app/Contents/Developer); default xcode-select's.
#   ACTOOL_DEVELOPER_DIR  an Xcode whose actool runs on this macOS, for the icon only.
#   SKIP_BUILD=1   reuse the last release bundle (iterating on signing).
set -euo pipefail

DRY_RUN=0
for a in "$@"; do
    case "$a" in
        --dry-run) DRY_RUN=1 ;;
        *) echo "usage: $0 [--dry-run]" >&2; exit 2 ;;
    esac
done

# Checked first: a missing version must not cost a whole build.
: "${MARKETING_VER:?MARKETING_VER is required: the version of the release tag being shipped (e.g. 0.12.0)}"
[[ "$MARKETING_VER" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]] \
    || { echo "ERROR: MARKETING_VER '$MARKETING_VER' is not 1-3 dot-separated integers (App Store rule)" >&2; exit 1; }
BUILD_NO="${BUILD_NO:-$(date +%s)}"

# Paths from the script's own place, not git.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
MINT_PROFILE_RB="${MINT_PROFILE_RB:-$ROOT/apps/studio/desktop/ios/mint-dev-profile.rb}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

TMP_DIRS=()
cleanup_tmp() { [ ${#TMP_DIRS[@]} -eq 0 ] || rm -rf "${TMP_DIRS[@]}"; }
trap cleanup_tmp EXIT
# Scratch (the IPA's staging) in TMPDIR: BSD mktemp -d alone ignores it.
WORK="$(mktemp -d "${TMPDIR:-/tmp}/patchbay-testflight.XXXXXX")"
TMP_DIRS+=("$WORK")

DX_BUNDLE_ID="${DX_BUNDLE_ID:-app.fasttrackstudio.patchbay}"
PROFILE_NAME="${PROFILE_NAME:-FTS Patchbay App Store}"
TEAM_ID="${TEAM_ID:-28C2G63DA7}"

if [ -z "${ASC_KEY_ID:-}" ] || [ -z "${ASC_ISSUER_ID:-}" ]; then
    # shellcheck disable=SC1090,SC1091 # this machine's own secrets file
    source "${ASC_CONFIG:-$HOME/.appstoreconnect/config.env}"
fi
: "${ASC_KEY_ID:?no App Store Connect key (ASC_KEY_ID)}"
: "${ASC_ISSUER_ID:?no App Store Connect issuer (ASC_ISSUER_ID)}"
# altool looks for AuthKey_<id>.p8 in ~/.appstoreconnect/private_keys
# (among others); ASC_KEY_PATH elsewhere is linked there.
if [ -n "${ASC_KEY_PATH:-}" ] && [ ! -f "$HOME/.appstoreconnect/private_keys/AuthKey_${ASC_KEY_ID}.p8" ]; then
    mkdir -p "$HOME/.appstoreconnect/private_keys"
    ln -sf "$ASC_KEY_PATH" "$HOME/.appstoreconnect/private_keys/AuthKey_${ASC_KEY_ID}.p8"
fi

# Signing keychain: a dedicated one on a headless box (codesign over SSH
# or from a runner cannot use the login keychain).
KEYCHAIN="${KEYCHAIN:-login.keychain-db}"
KEYCHAIN_PW="${KEYCHAIN_PW:-}"
if [ -n "$KEYCHAIN_PW" ]; then
    security unlock-keychain -p "$KEYCHAIN_PW" "$KEYCHAIN"
    security list-keychains -d user -s "$KEYCHAIN" login.keychain-db >/dev/null 2>&1 || true
fi
SIGN_ID="$(security find-identity -v -p codesigning "$KEYCHAIN" \
    | awk -F'"' '/Apple Distribution/{print $2; exit}')"
[ -n "$SIGN_ID" ] || { echo "ERROR: no Apple Distribution identity in $KEYCHAIN (apps/studio/desktop/ios/setup-keychain.sh imports it)." >&2; exit 1; }
echo "=== distribution identity: $SIGN_ID (keychain: $KEYCHAIN) ==="

echo "=== App Store provisioning profile ($PROFILE_NAME) ==="
PROFILE=""
if [ "${MINT_PROFILE:-}" = 1 ]; then
    PROFILE="$(PROFILE_TYPE=IOS_APP_STORE CERT_TYPE=DISTRIBUTION \
        ruby "$MINT_PROFILE_RB" - "$DX_BUNDLE_ID" "$PROFILE_NAME" | awk -F= '/PROFILE_PATH=/{print $2}')"
else
    # The installed profile of that name, for this team and bundle id, the
    # latest to expire, still valid.
    now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    best=""
    for f in "$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles/"*.mobileprovision \
             "$HOME/Library/MobileDevice/Provisioning Profiles/"*.mobileprovision; do
        [ -f "$f" ] || continue
        security cms -D -i "$f" > "$WORK/p.plist" 2>/dev/null || continue
        [ "$(plutil -extract Name raw -o - "$WORK/p.plist" 2>/dev/null)" = "$PROFILE_NAME" ] || continue
        [ "$(plutil -extract Entitlements.application-identifier raw -o - "$WORK/p.plist" 2>/dev/null)" = "$TEAM_ID.$DX_BUNDLE_ID" ] || continue
        exp="$(plutil -extract ExpirationDate raw -o - "$WORK/p.plist")"
        [[ "$exp" > "$now" ]] || continue
        if [ -z "$best" ] || [[ "$exp" > "$best" ]]; then best="$exp"; PROFILE="$f"; fi
    done
fi
[ -n "$PROFILE" ] && [ -f "$PROFILE" ] || {
    echo "ERROR: no valid '$PROFILE_NAME' profile for $TEAM_ID.$DX_BUNDLE_ID installed (MINT_PROFILE=1 mints one)." >&2
    exit 1; }
echo "profile: $PROFILE"

echo "=== building release ==="
APP="$TARGET_DIR/aarch64-apple-ios/release/Patchbay.app"
if [ "${SKIP_BUILD:-}" = "1" ] && [ -d "$APP" ]; then
    echo "SKIP_BUILD=1: reusing $APP"
else
    # build-app.sh needs the dev shell (cargo, the toolchain); inside one
    # already (DEVENV_ROOT / IN_NIX_SHELL), it runs as it is.
    if [ -n "${DEVENV_ROOT:-}${IN_NIX_SHELL:-}" ]; then
        APP="$("$SCRIPT_DIR/build-app.sh" --device --release | tail -1)"
    else
        APP="$(cd "$ROOT" && devenv shell -- bash "$SCRIPT_DIR/build-app.sh" --device --release | tail -1)"
    fi
fi
[ -d "$APP" ] && [ -f "$APP/patchbay-ios" ] || { echo "ERROR: the release build produced no app at $APP" >&2; exit 1; }
[ -f "$APP/Assets.car" ] || { echo "ERROR: $APP has no Assets.car (the icon): altool would reject it" >&2; exit 1; }

PL="$APP/Info.plist"
BUNDLE="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$PL")"
[ "$BUNDLE" = "$DX_BUNDLE_ID" ] || { echo "ERROR: the app is $BUNDLE, the profile is for $DX_BUNDLE_ID" >&2; exit 1; }
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $MARKETING_VER" "$PL"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $BUILD_NO" "$PL"

# The SDK build metadata Xcode writes and cargo does not: without it App
# Store Connect cannot tell the SDK the binary was built against (90534).
DEV="${XCODE_DIR:-$(xcode-select -p)}"
SDK="$DEV/Platforms/iPhoneOS.platform/Developer/SDKs/iPhoneOS.sdk"
SDK_VER="$(/usr/libexec/PlistBuddy -c 'Print :Version' "$SDK/SDKSettings.plist" 2>/dev/null || true)"
SDK_BUILD="$(/usr/libexec/PlistBuddy -c 'Print :ProductBuildVersion' "$DEV/Platforms/iPhoneOS.platform/version.plist" 2>/dev/null || true)"
XCODE_ROOT="${DEV%/Contents/Developer}"
XCODE_BUILD="$(/usr/libexec/PlistBuddy -c 'Print :ProductBuildVersion' "$XCODE_ROOT/Contents/version.plist" 2>/dev/null || true)"
XCODE_VER="$(DEVELOPER_DIR="$DEV" xcodebuild -version 2>/dev/null | awk '/^Xcode/{print $2}' || true)"
DTXCODE="$(echo "$XCODE_VER" | awk -F. '{printf "%02d%d%d", $1, ($2==""?0:$2), ($3==""?0:$3)}')"
add_str() { /usr/libexec/PlistBuddy -c "Add :$1 string $2" "$PL" 2>/dev/null || \
            /usr/libexec/PlistBuddy -c "Set :$1 $2" "$PL"; }
add_str DTPlatformName iphoneos
add_str DTPlatformVersion "$SDK_VER"
add_str DTPlatformBuild "$SDK_BUILD"
add_str DTSDKName "iphoneos${SDK_VER}"
add_str DTSDKBuild "$SDK_BUILD"
add_str DTXcode "$DTXCODE"
add_str DTXcodeBuild "$XCODE_BUILD"
add_str DTCompiler "com.apple.compilers.llvm.clang.1_0"
add_str BuildMachineOSBuild "$(sw_vers -buildVersion)"
plutil -lint "$PL" >/dev/null
echo "=== app: $APP ($BUNDLE) $MARKETING_VER ($BUILD_NO), iphoneos${SDK_VER} (${SDK_BUILD}), Xcode ${XCODE_VER} (${XCODE_BUILD}) ==="

echo "=== signing (distribution) ==="
cp "$PROFILE" "$APP/embedded.mobileprovision"
security cms -D -i "$PROFILE" > "$WORK/profile.plist"
/usr/libexec/PlistBuddy -x -c "Print :Entitlements" "$WORK/profile.plist" > "$WORK/entitlements.plist"
# Inside out: anything nested first, then the app.
find "$APP" -mindepth 1 \( -name "*.dylib" -o -name "*.framework" \) -print0 \
    | while IFS= read -r -d '' f; do codesign --force --keychain "$KEYCHAIN" --sign "$SIGN_ID" --timestamp "$f"; done
codesign --force --keychain "$KEYCHAIN" --sign "$SIGN_ID" \
    --entitlements "$WORK/entitlements.plist" --timestamp "$APP"
codesign --verify --deep --strict "$APP"

echo "=== packaging IPA ==="
mkdir -p "$WORK/Payload"
cp -R "$APP" "$WORK/Payload/"
IPA="$WORK/patchbay-ios.ipa"
# ditto, not zip: it keeps the signature's symlinks, which altool wants.
( cd "$WORK" && ditto -c -k --sequesterRsrc --keepParent Payload "$IPA" )
if [ -n "${IPA_OUT:-}" ]; then cp "$IPA" "$IPA_OUT"; echo "ipa: $IPA_OUT"; fi

echo "=== validating with App Store Connect ==="
xcrun altool --validate-app -t ios -f "$IPA" --apiKey "$ASC_KEY_ID" --apiIssuer "$ASC_ISSUER_ID"

if [ "$DRY_RUN" = 1 ]; then
    echo "=== DRY RUN: $MARKETING_VER ($BUILD_NO) built, signed and validated; not uploaded ==="
    exit 0
fi

echo "=== uploading to TestFlight ==="
xcrun altool --upload-app -t ios -f "$IPA" --apiKey "$ASC_KEY_ID" --apiIssuer "$ASC_ISSUER_ID"
echo "=== DONE: $MARKETING_VER ($BUILD_NO) uploaded; TestFlight shows it once Apple has processed it (5-15 min) ==="
