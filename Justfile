# patchbay — recipes
# Run commands: just <recipe-name>

# List recipes by default
default:
    @just --list

# Stage the browser remote for embedding: dx web build of patchbay-web →
# app/web-dist/, which `--features embed-web` compiles into the binary
# via include_dir!. web-dist/ is gitignored. dx never cleans its output
# (every past build's hashed wasm stays in public/assets and would be
# embedded too), so it is removed first.
web-stage:
    rm -rf target/dx/patchbay-web/release/web/public
    cd app/web && dx build --platform web --release
    rm -rf app/web-dist
    cp -r target/dx/patchbay-web/release/web/public app/web-dist

# Install on this machine and (re)start it — the one command after any
# change. macOS: build + sign + install Patchbay.app, relaunch it, wait
# until it answers (packaging/macos/deploy.sh; the running engine drops
# for a couple of seconds). Linux: `install-linux`.
install:
    #!/usr/bin/env bash
    set -euo pipefail
    case "$(uname -s)" in
        Darwin) packaging/macos/deploy.sh ;;
        *) {{just_executable()}} install-linux ;;
    esac

# Linux: release binary (web bundle EMBEDDED) in ~/.local/lib/fts,
# `patchbay` on PATH, launcher entry + icon. Does not restart a running
# app.
install-linux: web-stage
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release -p fts-patchbay --features embed-web
    install -d ~/.local/lib/fts
    install -m 755 target/release/fts-patchbay ~/.local/lib/fts/patchbay-app.new
    mv -T ~/.local/lib/fts/patchbay-app.new ~/.local/lib/fts/patchbay-app
    install -m 755 target/release/patchbay ~/.local/lib/fts/patchbay.new
    mv -T ~/.local/lib/fts/patchbay.new ~/.local/lib/fts/patchbay
    install -d ~/.local/bin
    ln -sf ~/.local/lib/fts/patchbay ~/.local/bin/patchbay
    install -d ~/.local/share/icons/hicolor/scalable/apps
    install -m 644 app/assets/icon.svg \
        ~/.local/share/icons/hicolor/scalable/apps/patchbay.svg
    install -d ~/.local/share/applications
    sed "s|@BIN@|$HOME/.local/lib/fts/patchbay-app|" \
        app/assets/patchbay.desktop \
        > ~/.local/share/applications/patchbay.desktop
    update-desktop-database ~/.local/share/applications 2>/dev/null || true
    gtk-update-icon-cache ~/.local/share/icons/hicolor 2>/dev/null || true
    # KDE keeps its own per-environment menu cache; rebuild it in the
    # session's env (a dev-shell kbuildsycoca updates the wrong cache).
    systemd-run --user --collect kbuildsycoca6 2>/dev/null || kbuildsycoca6 2>/dev/null || true
    echo "installed: Patchbay CLI ('patchbay health') and desktop app (launch from the app menu or run 'patchbay-app')"

# Run the app from source
run:
    cargo run -p fts-patchbay

# Run the agent CLI from source, e.g. `just cli health --json`.
cli *args:
    cargo run -p fts-patchbay --bin patchbay -- {{args}}

# These assume the dev shell (`nix develop`, or direnv via .envrc).
# Outside it the build needs pipewire/gtk/webkit headers, and the
# pactl-backed features quietly do nothing.

check:
    cargo check --workspace --all-targets

# Unit tests plus the sandboxed integration tests, which spin up a
# private PipeWire daemon — never the host's session.
test:
    cargo test --workspace

lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

# Everything CI runs, in one go.
ci: lint check test

# ── macOS: Patchbay.app ───────────────────────────────────────────────
# The engine runs inside Patchbay.app, which owns the privacy grants
# (System Audio Recording for Core Audio taps, Microphone); the
# `patchbay` CLI stays a plain RPC client. Scripts: packaging/macos/.

# Build target/macos/Patchbay.app (release, unsigned).
macos-app:
    packaging/macos/build-app.sh

# Build + sign (hardened runtime, Developer ID; PATCHBAY_SIGN_ID overrides) + verify.
macos-sign: macos-app
    packaging/macos/sign.sh

# Build + sign + install to ~/Applications (PATCHBAY_APP_DIR overrides); CLI → ~/.local/bin/patchbay.
# Quits a running Patchbay but does not relaunch it — `just install` does.
macos-install: macos-sign
    packaging/macos/install.sh

# Notarize + staple the signed app for other Macs (profile: PATCHBAY_NOTARY_PROFILE, default patchbay-notary).
macos-notarize:
    packaging/macos/notarize.sh

# Regenerate packaging/macos/AppIcon.icns from app/assets/icon.svg (needs rsvg-convert).
macos-icon:
    packaging/macos/make-icon.sh
