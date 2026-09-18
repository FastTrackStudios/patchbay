#!/usr/bin/env bash
# Sign target/macos/Patchbay.app with the hardened runtime (required for
# notarization) and verify it. Used by `just macos-sign`.
#
# Identity: $PATCHBAY_SIGN_ID, default the Developer ID Application cert.
# Keychain: $PATCHBAY_KEYCHAIN (default fts-build.keychain), unlocked with
# $PATCHBAY_KEYCHAIN_PW when set (e.g. exported in your shell profile);
# otherwise `security` prompts once. The password is never stored here.
# A stable Developer ID signature is what keeps the macOS privacy grants
# (System Audio Recording, Microphone) across rebuilds: TCC keys them on
# the bundle id + the signing team, not on the binary's hash.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
app="$root/target/macos/Patchbay.app"
ent="$root/packaging/macos/Patchbay.entitlements"
id="${PATCHBAY_SIGN_ID:-Developer ID Application: CODY JAMES WRIGHT (28C2G63DA7)}"
[[ -d "$app" ]] || { echo "no $app — run 'just macos-app' first" >&2; exit 1; }

# Non-interactive signing: the identities live in a dedicated build
# keychain (created by signal/apps/desktop/ios/setup-keychain.sh with a
# key partition list for apple-tool:/apple:), unlocked here so codesign
# never pops a password dialog. Missing keychain → the default search
# list (login keychain) is used.
keychain="${PATCHBAY_KEYCHAIN:-fts-build.keychain}"
keychain_path="$HOME/Library/Keychains/${keychain}-db"
kc_args=()
if [[ -f "$keychain_path" ]]; then
    if [[ -n "${PATCHBAY_KEYCHAIN_PW:-}" ]]; then
        security unlock-keychain -p "$PATCHBAY_KEYCHAIN_PW" "$keychain"
    else
        # No password in the environment: macOS asks once (and the
        # keychain stays unlocked for the session).
        security unlock-keychain "$keychain"
    fi
    kc_args=(--keychain "$keychain_path")
    echo "==> using keychain $keychain"
fi

echo "==> signing with: $id"
# Inside-out: nested code first, then the bundle (which seals them).
codesign --force --options runtime --timestamp ${kc_args[@]+"${kc_args[@]}"} \
    --identifier app.fasttrackstudio.patchbay.cli \
    --entitlements "$root/packaging/macos/patchbay-cli.entitlements" \
    --sign "$id" "$app/Contents/Helpers/patchbay"
codesign --force --options runtime --timestamp ${kc_args[@]+"${kc_args[@]}"} \
    --entitlements "$ent" \
    --sign "$id" "$app"

echo "==> codesign --verify --deep --strict"
codesign --verify --deep --strict --verbose=2 "$app"
echo "==> signature"
codesign -dv "$app" 2>&1 | grep -E '^(Identifier|Format|Authority|TeamIdentifier|Runtime Version|Timestamp|flags)|flags=' || true
echo "==> Gatekeeper assessment (\"Unnotarized Developer ID\" is expected until 'just macos-notarize')"
spctl -a -vv -t exec "$app" 2>&1 || true
