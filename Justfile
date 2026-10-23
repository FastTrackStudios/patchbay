# patchbay — recipes
# Run commands: just <recipe-name>
#
# These assume the dev shell (`nix develop`, or direnv via .envrc). Outside
# it the Linux build needs pipewire/gtk/webkit headers, and the
# pactl-backed features quietly do nothing.

# List recipes by default
default:
    @just --list

# ── Build and test ───────────────────────────────────────────────────────

# cargo check of everything (all targets, the server and UI features).
check:
    cargo check --workspace --all-targets --locked

# The browser remote (fts-patchbay with `web`) still builds for wasm.
check-web:
    cargo check -p fts-patchbay --target wasm32-unknown-unknown --no-default-features --features web --locked

# Unit tests plus, on Linux, the sandboxed integration tests, which spin up
# a private PipeWire daemon — never the host's session.
test:
    cargo test --workspace --locked

# Format the tree.
fmt:
    cargo fmt --all

# Format check only.
fmt-check:
    cargo fmt --all -- --check

# Clippy under the workspace lints (not a CI gate yet).
clippy:
    cargo clippy --workspace --all-targets

# Every crate this branch changes has a change file (.changes/).
change-check BASE="origin/main":
    cargo rail change check --since {{BASE}}

# What CI runs (.github/workflows/checks.yml), in the same order; stops at
# the first failure.
ci: fmt-check change-check check check-web test

# ── The app (apps/patchbay) ──────────────────────────────────────────────
# One crate, two platforms: the desktop app (default) and the browser
# remote (`--platform web` turns on `web` and the defaults off).

# Stage the browser remote for embedding: dx web build of fts-patchbay →
# apps/patchbay/web-dist/, which `--features embed-web` compiles into the
# binary via include_dir!. web-dist/ is gitignored. dx never cleans its
# output (every past build's hashed wasm stays in public/assets and would
# be embedded too), so it is removed first.
web-stage:
    rm -rf target/dx/fts-patchbay/release/web/public
    cd apps/patchbay && dx build --platform web --release
    rm -rf apps/patchbay/web-dist
    cp -r target/dx/fts-patchbay/release/web/public apps/patchbay/web-dist

# Install Patchbay on this machine and (re)start it — the one command
# after any change. macOS: build + sign + install Patchbay.app, relaunch
# it, wait until it answers (apps/patchbay/packaging/macos/deploy.sh; the
# running engine drops for a couple of seconds). Linux: `install-linux`.
install:
    #!/usr/bin/env bash
    set -euo pipefail
    case "$(uname -s)" in
        Darwin) apps/patchbay/packaging/macos/deploy.sh ;;
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
    install -m 644 apps/patchbay/assets/icon.svg \
        ~/.local/share/icons/hicolor/scalable/apps/patchbay.svg
    install -d ~/.local/share/applications
    sed "s|@BIN@|$HOME/.local/lib/fts/patchbay-app|" \
        apps/patchbay/assets/patchbay.desktop \
        > ~/.local/share/applications/patchbay.desktop
    update-desktop-database ~/.local/share/applications 2>/dev/null || true
    gtk-update-icon-cache ~/.local/share/icons/hicolor 2>/dev/null || true
    # KDE keeps its own per-environment menu cache; rebuild it in the
    # session's env (a dev-shell kbuildsycoca updates the wrong cache).
    systemd-run --user --collect kbuildsycoca6 2>/dev/null || kbuildsycoca6 2>/dev/null || true
    echo "installed: Patchbay CLI ('patchbay health') and desktop app (launch from the app menu or run 'patchbay-app')"

# Run the Patchbay app from source.
run:
    cargo run -p fts-patchbay

# Run Patchbay's agent CLI from source, e.g. `just cli health --json`.
cli *args:
    cargo run -p fts-patchbay --bin patchbay -- {{args}}

# macOS: Patchbay.app. The engine runs inside Patchbay.app, which owns the
# privacy grants (System Audio Recording for Core Audio taps, Microphone);
# the `patchbay` CLI stays a plain RPC client.

# Build target/macos/Patchbay.app (release, unsigned).
macos-app:
    apps/patchbay/packaging/macos/build-app.sh

# Build + sign (hardened runtime, Developer ID; PATCHBAY_SIGN_ID overrides) + verify.
macos-sign: macos-app
    apps/patchbay/packaging/macos/sign.sh

# Build + sign + install to ~/Applications (PATCHBAY_APP_DIR overrides); CLI → ~/.local/bin/patchbay.
# Quits a running Patchbay but does not relaunch it — `just install` does.
macos-install: macos-sign
    apps/patchbay/packaging/macos/install.sh

# Notarize + staple the signed app for other Macs (profile: PATCHBAY_NOTARY_PROFILE, default patchbay-notary).
macos-notarize:
    apps/patchbay/packaging/macos/notarize.sh

# Regenerate AppIcon.icns from apps/patchbay/assets/icon.svg (needs rsvg-convert).
macos-icon:
    apps/patchbay/packaging/macos/make-icon.sh

# ── Releases (fleet release flow v4; scripts/release.sh, AGENTS.md) ──────
# Every PR into main carries a cargo-rail change file (`cargo rail change
# add <crates> --bump <none|patch|minor|major> -m "<note>"`). A release is
# a TAG: `release-prepare` cuts `release/pending` (ONE commit: the CalVer
# bump YY.M.MICRO and the CHANGELOG.md section built from .changes/),
# `release-pr` opens it, `release-merge` squash-merges it after
# re-checking it against the current main, and `release-tag` pushes
# `vYY.M.MICRO` once that commit's `Build + test` is green, which
# publishes the GitHub Release. Production = the latest `v*` tag.

# The repo's release version ([workspace.package] in Cargo.toml).
version:
    @bash scripts/release.sh version

# What is live (latest v* tag), what main (or REF) holds since, the next
# CalVer version, the draft changelog from .changes/ and the git deps that
# moved. Reads only.
release-plan *REF:
    bash scripts/release.sh plan {{REF}}

# Cut `release/pending` from origin/main: bump to the next CalVer version,
# write the changelog section from .changes/ (deleting the files), commit
# `chore(release): v<next>`. An X.Y.Z argument only pins the version you
# expect (it must be the computed next one). `--hotfix` cuts from
# origin/release/v<latest tag> instead (after `release-hotfix` merged).
release-prepare *ARGS:
    bash scripts/release.sh prepare {{ARGS}}

# On release/pending (or a hotfix branch): push it and open its PR.
release-pr:
    bash scripts/release.sh pr

# Re-run the release guard against the CURRENT base, then squash-merge
# the release PR (pinned to its head commit).
release-merge:
    bash scripts/release.sh merge

# After the release PR merged: tag v<version> on its merge commit once
# `Build + test` is green there. THE TAG SHIPS (the GitHub Release). After
# a hotfix it opens the forward-port PR into main. Push as the fts-forge
# bot or a person, not a workflow token (that starts no workflow).
release-tag *ARGS:
    bash scripts/release.sh tag {{ARGS}}

# Hotfix while main can't ship: release/v<latest tag> from the tag (if
# missing), the given main commits cherry-picked with -x on a branch, and
# its PR into that line. Then release-prepare --hotfix, release-pr,
# release-merge, release-tag.
release-hotfix +COMMITS:
    bash scripts/release.sh hotfix {{COMMITS}}

# One version's section of CHANGELOG.md (default: the current version).
release-notes *ARGS:
    @bash scripts/release.sh notes {{ARGS}}

# The release tooling's own end-to-end test (synthetic repo, local origin).
release-sandbox:
    bash scripts/release-sandbox.sh
