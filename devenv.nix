# Patchbay's dev environment (devenv 2.4): `devenv shell`, or direnv through
# .envrc. devenv-fleet's `rust` module (devenv.yaml) gives the toolchain (rust
# 1.99.1 with wasm32, the rust-toolchain.toml pin), mold, cargo-hakari,
# cargo-rail, prek, just, pkg-config and `fleet-doctor`; its `darwin` module
# the macOS library path. This file adds what the workspace links against and
# shells out to: Studio's devenv.nix, cut to Patchbay's set. flake.nix's
# `nix develop` shell (CI and the release) takes the same toolchain from
# devenv-fleet: keep its package list in step with this one.
{ pkgs, lib, ... }:
let
  # webkit and gtk are dlopened at run time, so pkg-config alone is not enough
  # to launch the desktop app or run its tests from the shell.
  linuxRuntimeLibs = with pkgs; [ gtk3 webkitgtk_4_1 libsoup_3 glib openssl ];
in
{
  packages = with pkgs; [
    # cargo fetches every git dependency through the git CLI. On macOS this
    # real git shadows the /usr/bin/git xcrun shim.
    git
    # The release changelog's fallback when there are no change files
    # (scripts/release.sh).
    git-cliff
    openssl
  ]
  ++ lib.optionals stdenv.isLinux [
    # libspa-sys runs bindgen, which needs a libclang at build time.
    rustPlatform.bindgenHook
    # ── The graph ──────────────────────────────────────────
    pipewire # libpipewire + pw-cli / pw-dump / pw-link / pw-metadata
    pipewire.dev
    # ── Desktop shell (dioxus + webkit) ────────────────────
    gtk3
    webkitgtk_4_1
    libsoup_3
    xdotool # libxdo
    dbus
    glib
    # ── Everything else the tree links against ─────────────
    alsa-lib
    udev
    # Tools the code SHELLS OUT to on Linux; without them the features that
    # use them silently no-op: pactl / parec (metering, OBS capture sources,
    # app routing) and wireplumber (the integration sandbox: nothing applies
    # a port config without a session manager, so buses come up with zero
    # ports). Client tools only: `pipewire` speaks the pulse protocol itself.
    pulseaudio
    wireplumber
  ];

  env = lib.optionalAttrs pkgs.stdenv.isLinux {
    LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
    LD_LIBRARY_PATH = lib.makeLibraryPath linuxRuntimeLibs;
    # The integration tests fail loudly rather than skip if this shell
    # somehow lacks pipewire.
    PATCHBAY_REQUIRE_SANDBOX = "1";
  };

  # kache is not set here: the rust module exports RUSTC_WRAPPER from PATH.
  # APPEND ~/.cargo/bin (dx lives there), never prepend: a host rustup would
  # shadow the pinned toolchain.
  enterShell = ''
    export PATH="$PATH:$HOME/.cargo/bin"
  '';

  fleet.banner = ''
    patchbay dev shell
      just check / just test / just fmt     just ci runs what CI runs
      just run                              the app (ws://:4046)
      'dx' (dioxus-cli) is not in this shell; only 'just web-stage' needs it
  '';
}
