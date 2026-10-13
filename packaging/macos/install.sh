#!/usr/bin/env bash
# Install the signed target/macos/Patchbay.app. Used by `just macos-install`.
#
#   ~/Applications/Patchbay.app   ($PATCHBAY_APP_DIR overrides the folder,
#                                  e.g. PATCHBAY_APP_DIR=/Applications)
#   ~/.local/bin/patchbay  ->  Patchbay.app/Contents/Helpers/patchbay
#
# Replaces an existing copy atomically (staged next to it, then renamed).
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
src="$root/target/macos/Patchbay.app"
dest_dir="${PATCHBAY_APP_DIR:-$HOME/Applications}"
dest="$dest_dir/Patchbay.app"
[[ -d "$src" ]] || { echo "no $src — run 'just macos-sign' first" >&2; exit 1; }
codesign --verify --deep --strict "$src" || { echo "$src is not validly signed" >&2; exit 1; }

if pgrep -xq Patchbay; then
    echo "==> Patchbay is running; quitting it"
    osascript -e 'tell application id "app.fasttrackstudio.patchbay" to quit' >/dev/null 2>&1 || true
    for _ in $(seq 1 20); do pgrep -xq Patchbay || break; sleep 0.25; done
fi

mkdir -p "$dest_dir"
stage="$dest_dir/.Patchbay.app.new.$$"
old="$dest_dir/.Patchbay.app.old.$$"
rm -rf "$stage"
ditto "$src" "$stage"               # preserves the signature + xattrs
[[ -e "$dest" ]] && mv "$dest" "$old"
mv "$stage" "$dest"
rm -rf "$old"
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$dest" 2>/dev/null || true

mkdir -p "$HOME/.local/bin"
ln -sfn "$dest/Contents/Helpers/patchbay" "$HOME/.local/bin/patchbay"

echo "installed: $dest"
echo "CLI:       $HOME/.local/bin/patchbay -> $dest/Contents/Helpers/patchbay"
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) echo "           (add ~/.local/bin to PATH)";; esac
echo
echo "Launch:    open \"$dest\"   (or Spotlight/Launchpad: Patchbay)"
echo "Check:     patchbay health --json"
echo "Logs:      ~/Library/Logs/Patchbay/patchbay.log"
echo "First run: approve 'System Audio Recording' (and Microphone) for Patchbay,"
echo "           or System Settings → Privacy & Security → Screen & System Audio"
echo "           Recording → System Audio Recording Only → Patchbay."
