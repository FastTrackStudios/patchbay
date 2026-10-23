{
  description = "FTS Patchbay — studio routing (PipeWire on Linux, Core Audio on macOS)";

  inputs = {
    # The same nixpkgs and rust-overlay revisions as the studio monorepo,
    # so the toolchain, the native libraries and cargo-rail are the store
    # paths studio's CI already pushed to fasttrackstudio.cachix.org.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # Honours rust-toolchain.toml (1.94.0 + wasm32 + clippy/rustfmt/rust-src).
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # A second rust-overlay, used ONLY to build cargo-rail: rail 0.32 needs
    # rustc >= 1.98.1, the workspace pin is 1.94. Nothing else reads it.
    rust-overlay-rail = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  nixConfig = {
    extra-trusted-public-keys = [
      "fasttrackstudio.cachix.org-1:r7v7WXBeSZ7m5meL6w0wttnvsOltRvTpXeVNItcy9f4="
    ];
    extra-substituters = [ "https://fasttrackstudio.cachix.org" ];
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      rust-overlay-rail,
    }:
    let
      # Linux runs the PipeWire engine; macOS the Core Audio one (and the
      # Patchbay.app packaging under apps/patchbay/packaging/macos).
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];
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

      # cargo-rail: change files (`cargo rail change add/check`, the
      # .changes/ the release changelog is built from) and CI change
      # selection. nixpkgs carries 0.7.0, which predates both, so it is
      # built from the release tag with its own compiler (the same
      # derivation as studio's nix/modules/cargo-rail.nix).
      cargoRailFor =
        pkgs:
        let
          railRust = (pkgs.extend rust-overlay-rail.overlays.default).rust-bin.stable."1.98.1".default;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = railRust;
            rustc = railRust;
          };
        in
        rustPlatform.buildRustPackage rec {
          pname = "cargo-rail";
          version = "0.32.0";
          src = pkgs.fetchFromGitHub {
            owner = "loadingalias";
            repo = "cargo-rail";
            rev = "v${version}";
            hash = "sha256-XdwwQVv3OIauYefmQNq6CKK/fcaEnmq6Rkilmg9bhk8=";
          };
          cargoHash = "sha256-kpFJ3/s1qgXqY7Nhci7/4YtMcw7E+UmB3SX8F5MHoxs=";
          doCheck = false;
          meta = {
            description = "Cargo-native monorepo change selection (plan), unify, release";
            homepage = "https://github.com/loadingalias/cargo-rail";
            license = pkgs.lib.licenses.mit;
            mainProgram = "cargo-rail";
          };
        };
    in
    {
      packages = forAllSystems (pkgs: {
        cargo-rail = cargoRailFor pkgs;
      });

      devShells = forAllSystems (
        pkgs:
        let
          inherit (pkgs) lib stdenv;
          linuxRuntimeLibs = [
            pkgs.gtk3
            pkgs.webkitgtk_4_1
            pkgs.libsoup_3
            pkgs.glib
            pkgs.openssl
          ];
        in
        {
          default = pkgs.mkShell {
            name = "patchbay";

            nativeBuildInputs = [
              (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
              pkgs.pkg-config
              # cargo shells out to git for every git dependency. On macOS
              # /usr/bin/git is an xcrun shim that resolves through the nix
              # SDK's DEVELOPER_DIR and fails; a real git shadows it.
              pkgs.git
              pkgs.just
              (cargoRailFor pkgs)
              # The release changelog's fallback when there are no change
              # files (scripts/release.sh).
              pkgs.git-cliff
            ]
            ++ lib.optionals stdenv.isLinux [
              # libspa-sys runs bindgen, which needs a libclang at build time.
              pkgs.clang
            ];

            buildInputs = [
              pkgs.openssl
            ]
            ++ lib.optionals stdenv.isLinux [
              # ── The graph ──────────────────────────────────────────
              pkgs.pipewire # libpipewire + pw-cli / pw-dump / pw-link / pw-metadata

              # ── Desktop shell (dioxus + webkit) ────────────────────
              pkgs.gtk3
              pkgs.webkitgtk_4_1
              pkgs.libsoup_3
              pkgs.xdotool # libxdo
              pkgs.dbus
              pkgs.glib

              # ── Everything else the tree links against ─────────────
              pkgs.alsa-lib
              pkgs.udev
            ]
            ++ lib.optionals stdenv.isDarwin [
              # No explicit apple-sdk: the darwin stdenv carries one, and a
              # second SDK in scope makes the cc wrapper abort with
              # "Multiple conflicting values defined for DEVELOPER_DIR".
              # Core Audio, AppKit and AVFoundation come from it.
              pkgs.libiconv
            ];

            # Tools the code SHELLS OUT to on Linux. Not link-time deps, so
            # they'd be easy to forget — but without them the features that
            # use them silently no-op:
            #
            #   pactl / parec  metering, OBS capture sources, app routing
            #   wireplumber    the integration sandbox (nothing applies a
            #                  port config without a session manager, so
            #                  buses come up with zero ports)
            packages = lib.optionals stdenv.isLinux [
              # Client tools ONLY — `pipewire` speaks the pulse protocol
              # itself via libpipewire-module-protocol-pulse.
              pkgs.pulseaudio # pactl, parec
              pkgs.wireplumber
            ];

            env = lib.optionalAttrs stdenv.isLinux {
              LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
              # webkit/gtk are dlopen'd at runtime, so pkg-config alone
              # isn't enough to launch the desktop app from the shell.
              LD_LIBRARY_PATH = lib.makeLibraryPath linuxRuntimeLibs;
              # The integration tests fail loudly rather than skip if this
              # shell somehow lacks pipewire.
              PATCHBAY_REQUIRE_SANDBOX = "1";
            };

            # APPEND ~/.cargo/bin (dx lives there), never prepend: a hosted
            # runner's rustup would shadow the pinned toolchain.
            shellHook = ''
              export PATH="$PATH:$HOME/.cargo/bin"
              if [ -t 1 ] && [ -z "''${CI:-}" ]; then
                echo "patchbay dev shell ($(rustc --version | cut -d' ' -f2), ${stdenv.hostPlatform.system})"
                echo "  just check / just test / just fmt     just ci runs what CI runs"
                echo "  just run                              the app (ws://:4046)"
                echo "  'dx' (dioxus-cli) is not in this shell; only 'just web-stage' needs it"
              fi
            '';
          };
        }
      );

    };
}
