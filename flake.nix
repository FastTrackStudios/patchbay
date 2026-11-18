{
  description = "FTS Patchbay — studio routing (PipeWire on Linux, Core Audio on macOS)";

  inputs = {
    # The native libraries' nixpkgs (flake.lock: 5e11f7ac, 2026-04), as
    # before. Not the fleet's newer one: on it, this shell's buildInputs
    # (GTK/WebKit's closure) put more -isystem/-L/-rpath flags on every gcc
    # call than Linux passes to collect2 (COLLECT_GCC_OPTIONS is one string,
    # 128 KiB at most), and every link fails with "Argument list too long".
    # The devenv shell has no such limit (its libraries are one profile).
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # The toolchain and cargo-rail: devenv-fleet at the revision devenv.yaml
    # imports, evaluated on ITS pinned nixpkgs and rust-overlay (below), so
    # rustc is the devenv shell's store path. Move it with devenv.yaml.
    devenv-fleet.url = "github:FastTrackStudios/devenv-fleet/c4ddec850ad97b5da74c746bff39a988f02b13a4";
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
      devenv-fleet,
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
            }
          )
        );

      # The fleet toolchain (rust 1.99.1 with wasm32, rust-src, clippy,
      # rustfmt, rust-analyzer): the expression devenv-fleet's `rust` module
      # uses, so `nix develop` and `devenv shell` have one rustc store path.
      # devenv-fleet's own nixpkgs and rust-overlay (pins.json), for this
      # system: the package set the devenv shell's toolchain comes from.
      fleetPkgsFor =
        pkgs:
        import devenv-fleet.inputs.nixpkgs {
          inherit (pkgs.stdenv.hostPlatform) system;
          overlays = [ devenv-fleet.overlays.default ];
        };
      toolchainFor = pkgs: devenv-fleet.lib.toolchainFor (fleetPkgsFor pkgs);

      # cargo-rail: change files (`cargo rail change add/check`, the
      # .changes/ the release changelog is built from) and CI change
      # selection. devenv-fleet's derivation (built from the release tag with
      # the fleet toolchain), so the devenv shell's cargo-rail too.
      cargoRailFor =
        pkgs:
        (fleetPkgsFor pkgs).callPackage "${devenv-fleet}/pkgs/cargo-rail.nix" {
          toolchain = toolchainFor pkgs;
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
              (toolchainFor pkgs)
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
              # The linker .cargo/config.toml selects on x86_64 Linux (the
              # fleet's rustflags); the devenv shell has it from devenv-fleet.
              pkgs.mold
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
