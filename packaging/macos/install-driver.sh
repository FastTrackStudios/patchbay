#!/usr/bin/env bash
# Install Patchbay.driver (runtime virtual devices) into
# /Library/Audio/Plug-Ins/HAL and restart coreaudiod so it loads. Only
# installing/updating the driver needs this — devices are then created,
# renamed and removed at runtime without restarting anything. Needs admin (sudo). ALL audio on
# the Mac drops for a second or two; apps like REAPER may need to reset
# their audio device afterwards.
#
#   packaging/macos/install-driver.sh            install / update
#   packaging/macos/install-driver.sh --remove   uninstall
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
# After `allow-driver-reload.sh` the bundle is ours and coreaudiod can be
# restarted without a password, so no step needs sudo.
as_root() { if [[ -w "$hal" || -w "$hal/Patchbay.driver" ]]; then "$@"; else sudo "$@"; fi; }
hal=/Library/Audio/Plug-Ins/HAL
bundles=(Patchbay.driver)
# Earlier BlackHole-based drivers, removed on install/uninstall (the
# runtime driver publishes the same "Patchbay" and "Broadcast" devices,
# with the same UIDs).
legacy=(PatchbayBroadcast.driver PatchbayBus.driver Broadcast.driver)

for b in "${legacy[@]}"; do as_root rm -rf "${hal:?}/$b"; done
if [[ "${1:-}" == "--remove" ]]; then
    for b in "${bundles[@]}"; do as_root rm -rf "$hal/$b"; done
else
    for b in "${bundles[@]}"; do
        src="$root/target/macos/drivers/$b"
        [[ -d "$src" ]] || { echo "missing $src — run build-driver.sh first" >&2; exit 1; }
        if [[ -w "$hal/$b" ]]; then
            # The bundle is ours (see allow-driver-reload.sh) but the HAL
            # folder isn't: replace its contents in place, no sudo.
            echo "==> updating $hal/$b in place"
            rm -rf "${hal:?}/$b/Contents"
            ditto "$src/Contents" "$hal/$b/Contents"
        else
            owner=root:wheel
            [[ -d "$hal/$b" ]] && owner="$(stat -f '%Su:%Sg' "$hal/$b")"
            as_root rm -rf "$hal/$b.new"
            as_root cp -R "$src" "$hal/$b.new"
            as_root chown -R "$owner" "$hal/$b.new"
            as_root rm -rf "$hal/$b"
            as_root mv "$hal/$b.new" "$hal/$b"
        fi
    done
fi
echo "==> restarting coreaudiod (audio drops briefly)"
# -n first: with the sudoers rule from allow-driver-reload.sh this needs
# no password; otherwise fall back to a normal (prompting) sudo.
sudo -n killall coreaudiod 2>/dev/null || sudo killall coreaudiod
sleep 2
system_profiler SPAudioDataType 2>/dev/null | grep -E '^\s+(Patchbay|Broadcast):' || echo "(no Patchbay devices listed)"
