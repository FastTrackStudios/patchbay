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
# available to build it, or when apps/patchbay/web-dist/ is already staged;
# otherwise the app serves only /health + /vox and says so.
set -euo pipefail
root="$(cd "$(dirname "$0")/../../../.." && pwd)"
cd "$root"
[[ "$(uname -s)" == Darwin ]] || { echo "macos-app: macOS only" >&2; exit 1; }

# A shell that never sourced ~/.cargo/env (a GUI-launched terminal, a
# bare zsh) has neither cargo nor dx. Without dx the web build below is
# skipped and a stale apps/patchbay/web-dist is embedded without a word, so look
# where rustup puts them before deciding they are missing.
if ! command -v cargo >/dev/null 2>&1 && [[ -x "$HOME/.cargo/bin/cargo" ]]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi
command -v cargo >/dev/null 2>&1 || {
    echo "macos-app: cargo not found (PATH, ~/.cargo/bin) — install rustup, or run from the dev shell" >&2
    exit 1
}

features=()
if command -v dx >/dev/null 2>&1; then
    echo "==> building the web remote (dx)"
    # dx never cleans its output: every past build's hashed wasm + js
    # stays in public/assets, and all of it would be embedded (~5 MB a
    # build). Start from nothing so the bundle is exactly this build.
    rm -rf target/dx/fts-patchbay/release/web/public
    (cd apps/patchbay && dx build --platform web --release)
    rm -rf apps/patchbay/web-dist
    cp -r target/dx/fts-patchbay/release/web/public apps/patchbay/web-dist
fi
if [[ -f apps/patchbay/web-dist/index.html ]]; then
    command -v dx >/dev/null 2>&1 \
        || echo "==> WARNING: dx not found — embedding the apps/patchbay/web-dist staged earlier, NOT a fresh web build" >&2
    features=(--features embed-web)
    echo "==> embedding apps/patchbay/web-dist (browser remote at http://127.0.0.1:4046/)"
else
    echo "==> NOTE: no web bundle (dx not installed, apps/patchbay/web-dist/ not staged):"
    echo "         building without embed-web; the browser remote is unavailable,"
    echo "         the RPC (/vox) and the desktop window work normally."
fi

echo "==> cargo build --release"
cargo build --release -p fts-patchbay --bin fts-patchbay --bin patchbay ${features[@]+"${features[@]}"}

# The repo's one version ([workspace.package] in the root Cargo.toml).
version="$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version *= *"\([^"]*\)".*/\1/p;}' Cargo.toml | head -1)"
build="$(date -u +%Y%m%d%H%M)"
app="target/macos/Patchbay.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Helpers" "$app/Contents/Resources"
install -m 755 target/release/fts-patchbay "$app/Contents/MacOS/Patchbay"
install -m 755 target/release/patchbay "$app/Contents/Helpers/patchbay"
install -m 644 apps/patchbay/packaging/macos/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
sed -e "s/@VERSION@/$version/g" -e "s/@BUILD@/$build/g" \
    apps/patchbay/packaging/macos/Info.plist > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
plutil -lint "$app/Contents/Info.plist" >/dev/null
echo "==> assembled $app (version $version, build $build, unsigned)"
