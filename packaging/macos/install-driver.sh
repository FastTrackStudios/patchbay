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
hal=/Library/Audio/Plug-Ins/HAL
bundles=(Patchbay.driver)
# Earlier BlackHole-based drivers, removed on install/uninstall (the
# runtime driver publishes the same "Patchbay" and "Broadcast" devices,
# with the same UIDs).
legacy=(PatchbayBroadcast.driver PatchbayBus.driver Broadcast.driver)

for b in "${legacy[@]}"; do sudo rm -rf "${hal:?}/$b"; done
if [[ "${1:-}" == "--remove" ]]; then
    for b in "${bundles[@]}"; do sudo rm -rf "$hal/$b"; done
else
    for b in "${bundles[@]}"; do
        src="$root/target/macos/drivers/$b"
        [[ -d "$src" ]] || { echo "missing $src — run build-driver.sh first" >&2; exit 1; }
        sudo rm -rf "$hal/$b.new"
        sudo cp -R "$src" "$hal/$b.new"
        sudo chown -R root:wheel "$hal/$b.new"
        sudo rm -rf "$hal/$b"
        sudo mv "$hal/$b.new" "$hal/$b"
    done
fi
echo "==> restarting coreaudiod (audio drops briefly)"
sudo killall coreaudiod
sleep 2
system_profiler SPAudioDataType 2>/dev/null | grep -E '^\s+(Patchbay|Broadcast):' || echo "(no Patchbay devices listed)"
