#!/usr/bin/env bash
# Build, sign, install and RESTART Patchbay on this Mac, then wait until
# the new one is answering. Used by `just install`.
#
# One command because the browser remote is compiled into the app: a UI
# change reaches a phone on the LAN only after all four steps, and doing
# them by hand means remembering the order and relaunching yourself.
#
# install.sh quits the running engine, so audio passing through Patchbay
# (mixes, taps) drops for the couple of seconds until the new one is up.
# Saved mixes restart on their own.
#
#   PATCHBAY_APP_DIR   install location (default ~/Applications)
#   PATCHBAY_PORT      port to wait on   (default 4046)
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
[[ "$(uname -s)" == Darwin ]] || { echo "deploy: macOS only" >&2; exit 1; }

packaging/macos/build-app.sh
# stdin closed: a locked signing keychain fails here instead of hanging
# on a password prompt nobody is watching.
packaging/macos/sign.sh < /dev/null
packaging/macos/install.sh

app="${PATCHBAY_APP_DIR:-$HOME/Applications}/Patchbay.app"
port="${PATCHBAY_PORT:-4046}"
echo "==> launching $app"
open "$app"

echo -n "==> waiting for the engine on :$port "
for _ in $(seq 1 60); do
    if curl -fsS -o /dev/null "http://127.0.0.1:$port/health" 2>/dev/null; then
        echo " up"
        echo
        echo "Patchbay is running. Browser remote:"
        echo "  http://127.0.0.1:$port/"
        # Only useful if Settings → Network has it open to the LAN.
        for ip in $(ifconfig 2>/dev/null | awk '/inet /{print $2}' | grep -v '^127\.'); do
            echo "  http://$ip:$port/"
        done
        exit 0
    fi
    echo -n "."
    sleep 0.5
done
echo " no answer after 30s" >&2
echo "deploy: Patchbay did not come up — see ~/Library/Logs/Patchbay/patchbay.log" >&2
exit 1
