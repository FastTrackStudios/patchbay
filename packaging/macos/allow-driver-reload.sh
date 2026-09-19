#!/usr/bin/env bash
# One-time setup so Patchbay.driver can be updated and reloaded WITHOUT a
# password (development convenience on your own Mac).
#
# It does two things, both reversible with --undo:
#
#   1. Gives the installed driver bundle to you, so updating it is an
#      ordinary file copy instead of a root one.
#   2. Adds a sudoers rule allowing exactly one command without a
#      password: restarting coreaudiod.
#
# TRADE-OFF, read before running: a driver bundle your account can write
# is loaded into coreaudiod (a system audio daemon). Anything running as
# you could then change code that daemon loads. That is a real weakening
# of the boundary between your account and the system — fine for a
# single-user studio machine you control, not for a shared or untrusted
# one. `--undo` puts both back.
#
#   sudo packaging/macos/allow-driver-reload.sh          set up
#   sudo packaging/macos/allow-driver-reload.sh --undo   revert
set -euo pipefail
hal=/Library/Audio/Plug-Ins/HAL
bundle="$hal/Patchbay.driver"
sudoers=/etc/sudoers.d/patchbay-driver
user="${SUDO_USER:-$USER}"

[[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 1; }

if [[ "${1:-}" == "--undo" ]]; then
    rm -f "$sudoers"
    [[ -d "$bundle" ]] && chown -R root:wheel "$bundle"
    echo "reverted: driver owned by root again, sudoers rule removed"
    exit 0
fi

[[ -d "$bundle" ]] || { echo "no $bundle — install it first" >&2; exit 1; }
chown -R "$user":staff "$bundle"

# Only these two commands, only for this user, only without a password.
tmp="$(mktemp)"
cat >"$tmp" <<EOF
# Added by patchbay packaging/macos/allow-driver-reload.sh
# Lets $user reload Patchbay.driver without a password. Remove with:
#   sudo packaging/macos/allow-driver-reload.sh --undo
Cmnd_Alias PATCHBAY_AUDIO_RELOAD = /usr/bin/killall coreaudiod, \\
    /bin/launchctl kickstart -kp system/com.apple.audio.coreaudiod
$user ALL=(root) NOPASSWD: PATCHBAY_AUDIO_RELOAD
EOF
visudo -cqf "$tmp" || { echo "generated sudoers rule is invalid — nothing changed" >&2; rm -f "$tmp"; exit 1; }
install -m 440 -o root -g wheel "$tmp" "$sudoers"
rm -f "$tmp"

echo "done:"
echo "  $bundle is owned by $user (updates need no sudo)"
echo "  $sudoers allows restarting coreaudiod without a password"
echo "  revert with: sudo packaging/macos/allow-driver-reload.sh --undo"
