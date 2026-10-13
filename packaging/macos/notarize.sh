#!/usr/bin/env bash
# Notarize + staple the signed target/macos/Patchbay.app so it opens on
# other Macs without Gatekeeper warnings. Used by `just macos-notarize`.
#
# One-time setup (stores an app-specific password in the login keychain):
#   xcrun notarytool store-credentials patchbay-notary \
#       --apple-id <your Apple ID> --team-id 28C2G63DA7 \
#       --password <app-specific password from appleid.apple.com>
# ($PATCHBAY_NOTARY_PROFILE overrides the profile name.)
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
app="$root/target/macos/Patchbay.app"
profile="${PATCHBAY_NOTARY_PROFILE:-patchbay-notary}"
zip="$root/target/macos/Patchbay.zip"
[[ -d "$app" ]] || { echo "no $app — run 'just macos-sign' first" >&2; exit 1; }
codesign --verify --deep --strict "$app"
# notarytool reads the stored profile from the keychain; unlock the build
# keychain (see sign.sh) when present so it never prompts. Profiles
# stored with `store-credentials --keychain <path>` need
# PATCHBAY_NOTARY_KEYCHAIN set to that path.
keychain="${PATCHBAY_KEYCHAIN:-fts-build.keychain}"
if [[ -f "$HOME/Library/Keychains/${keychain}-db" ]]; then
    if [[ -n "${PATCHBAY_KEYCHAIN_PW:-}" ]]; then
        security unlock-keychain -p "$PATCHBAY_KEYCHAIN_PW" "$keychain"
    else
        # No password in the environment: macOS asks once (and the
        # keychain stays unlocked for the session).
        security unlock-keychain "$keychain"
    fi
fi
nt_kc=()
[[ -n "${PATCHBAY_NOTARY_KEYCHAIN:-}" ]] && nt_kc=(--keychain "$PATCHBAY_NOTARY_KEYCHAIN")

rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
echo "==> submitting to Apple (profile: $profile) — usually a few minutes"
xcrun notarytool submit "$zip" --keychain-profile "$profile" ${nt_kc[@]+"${nt_kc[@]}"} --wait
echo "==> stapling the ticket"
xcrun stapler staple "$app"
xcrun stapler validate "$app"
spctl -a -vv -t exec "$app"
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
echo "notarized: $app  (distributable zip: $zip)"
