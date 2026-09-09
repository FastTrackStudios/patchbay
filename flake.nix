{
  description = "FTS Patchbay — PipeWire studio routing";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # Honours rust-toolchain.toml, so the shell's rustc matches the pin
    # (1.94.0 + wasm32 + clippy/rustfmt/rust-src) instead of drifting
    # with nixpkgs.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self, nixpkgs, rust-overlay }:
    let
      # PipeWire is Linux-only, and so is everything this repo does.
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              overlays = [ (import rust-overlay) ];
            }
          )
        );
    in
    {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          name = "patchbay";

          nativeBuildInputs = [
            (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
            pkgs.pkg-config
            # libspa-sys runs bindgen, which needs a libclang at build time.
            pkgs.clang
          ];

          buildInputs = [
            # ── The graph ────────────────────────────────────────────
            pkgs.pipewire # libpipewire + pw-cli / pw-dump / pw-link / pw-metadata

            # ── Desktop shell (dioxus + webkit) ──────────────────────
            pkgs.gtk3
            pkgs.webkitgtk_4_1
            pkgs.libsoup_3
            pkgs.xdotool # libxdo
            pkgs.dbus
            pkgs.glib

            # ── Everything else the tree links against ───────────────
            pkgs.alsa-lib
            pkgs.udev
            pkgs.openssl
          ];

          # Tools the code SHELLS OUT to. These are not link-time deps,
          # so they'd be easy to forget — but without them the features
          # that use them silently no-op, which is worse than failing:
          #
          #   pactl / parec  metering, OBS capture sources, app routing
          #   wireplumber    the integration sandbox (nothing applies a
          #                  port config without a session manager, so
          #                  buses come up with zero ports)
          #   systemd        `systemctl --user` for the service panel
          packages = [
            # Client tools ONLY — nothing here runs a PulseAudio
            # server. `pipewire` speaks the pulse protocol itself via
            # libpipewire-module-protocol-pulse.
            pkgs.pulseaudio # pactl, parec
            pkgs.wireplumber
          ];

          env = {
            LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
            # webkit/gtk are dlopen'd at runtime, so pkg-config alone
            # isn't enough to launch the desktop app from the shell.
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
              pkgs.gtk3
              pkgs.webkitgtk_4_1
              pkgs.libsoup_3
              pkgs.glib
              pkgs.openssl
            ];
            # Make the integration tests fail loudly rather than skip if
            # this shell somehow lacks pipewire.
            PATCHBAY_REQUIRE_SANDBOX = "1";
          };

          shellHook = ''
            echo "patchbay dev shell — pipewire $(${pkgs.pipewire}/bin/pw-cli --version 2>/dev/null | head -1 || echo '?')"
            echo "  cargo test --workspace     unit + sandboxed integration tests"
            echo "  cargo clippy --workspace --all-targets"
            echo
            echo "note: 'dx' (dioxus-cli) is NOT in this shell — it is a long"
            echo "      build and only 'just web-stage' needs it."
          '';
        };
      });
    };
}
