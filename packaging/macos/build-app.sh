#!/usr/bin/env bash
# Assemble target/macos/Patchbay.app from a release build (unsigned; run
# sign.sh next). Used by `just macos-app`.
#
#   Contents/MacOS/Patchbay        the desktop app + engine (fts-patchbay)
#   Contents/Helpers/patchbay      the agent CLI (RPC client of the app)
#   Contents/Resources/AppIcon.icns
#   Contents/Info.plist            from Info.plist (@VERSION@/@BUILD@)
#
# The CLI lives in Helpers/, not MacOS/: on the default case-insensitive
# APFS "patchbay" and "Patchbay" would be the same file.
#
# The browser remote is embedded (--features embed-web) when `dx` is
# available to build it, or when app/web-dist/ is already staged;
# otherwise the app serves only /health + /vox and says so.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
[[ "$(uname -s)" == Darwin ]] || { echo "macos-app: macOS only" >&2; exit 1; }

features=()
if command -v dx >/dev/null 2>&1; then
    echo "==> building the web remote (dx)"
    (cd app/web && dx build --platform web --release)
    rm -rf app/web-dist
    cp -r target/dx/patchbay-web/release/web/public app/web-dist
fi
if [[ -f app/web-dist/index.html ]]; then
    features=(--features embed-web)
    echo "==> embedding app/web-dist (browser remote at http://127.0.0.1:4046/)"
else
    echo "==> NOTE: no web bundle (dx not installed, app/web-dist/ not staged):"
    echo "         building without embed-web; the browser remote is unavailable,"
    echo "         the RPC (/vox) and the desktop window work normally."
fi

echo "==> cargo build --release"
cargo build --release -p fts-patchbay --bin fts-patchbay --bin patchbay ${features[@]+"${features[@]}"}

version="$(sed -n 's/^version = "\(.*\)"/\1/p' app/Cargo.toml | head -1)"
build="$(date -u +%Y%m%d%H%M)"
app="target/macos/Patchbay.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Helpers" "$app/Contents/Resources"
install -m 755 target/release/fts-patchbay "$app/Contents/MacOS/Patchbay"
install -m 755 target/release/patchbay "$app/Contents/Helpers/patchbay"
install -m 644 packaging/macos/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
sed -e "s/@VERSION@/$version/g" -e "s/@BUILD@/$build/g" \
    packaging/macos/Info.plist > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
plutil -lint "$app/Contents/Info.plist" >/dev/null
echo "==> assembled $app (version $version, build $build, unsigned)"
