# patchbay — agent instructions

Patchbay is FastTrackStudio's studio routing: the live audio graph
(PipeWire on Linux, Core Audio on macOS — nodes, ports, links), routing
presets (connection memory), display aliases, the graph clock
(quantum/rate), mixes, the Dante/Inferno stack, and hardware devices
(Antelope Galaxy 32, Yamaha TF, the Dante network). The desktop app and
its RPC server (`ws://:4046`) are `apps/patchbay` (`fts-patchbay`).

It lived in the studio monorepo in October 2026 and keeps that layout.
It depends on no studio crate and builds standalone; a future, off-by-
default `studio` feature may take studio's contract crates as git deps
pinned to a studio release tag (see the commented slot in the root
`Cargo.toml`).

## Layout

```
crates/patchbay         patchbay         ONE crate, cargo features:
                                         server (the engine + adapters),
                                         vox (wire types), ui (Dioxus),
                                         tcc-spi (macOS permission prompts)
  src/proto             the wire contract (Facet types + #[architect::rpc])
  src/server            the engine: PipeWire graph, presets, mixes, clock,
                        Dante stack, the device hub
  src/host, coreaudio   the host-audio model and the macOS Core Audio backend
  src/device            the adapter-agnostic hardware model + DeviceAdapter
  src/antelope, yamaha, dante   the hardware adapters
  src/ui                the Dioxus interface
crates/patchbay/driver  patchbay-driver  the macOS AudioServerPlugIn behind
                                         the virtual devices (MARS fork,
                                         GPL-3.0-or-later AND MIT)
apps/patchbay           fts-patchbay     the desktop app + RPC server, the
                                         `patchbay` agent CLI, the browser
                                         remote (--platform web);
                                         packaging/macos (Patchbay.app)
docs/                   devices.md, host-backends.md, yamaha-tf-rcp.md
```

## Key rules

- **Headless core (strict)**: the engine never touches dioxus. GUIs are
  vox remotes over `PatchbayService`; the desktop app connects through an
  in-process `architect::LocalServer`, the same client remotes use. Root
  UI components take no props (context only).
- **One PipeWire thread**: all libpipewire objects live on the
  `patchbay-pw` thread (`Rc`/`RefCell`). Commands go in via
  `pipewire::channel`; state comes out via the shared `GraphStore` mirror
  + `GraphEvent` mpsc → PubSub hub.
- **Identity is names, not ids**: presets/aliases key on `node.name` /
  `port.name`. PipeWire global ids are never persisted.
- Links are created with `object.linger` — the patchbay edits the system
  graph, it doesn't own it. Never make the engine "clean up" links at
  exit; the studio's graph runs Sunday-worship production.
- Shell-outs (`pw-metadata`, `systemctl --user`) are best-effort and live
  in `clock.rs` / `dante.rs` only.
- Architect idiom: services are `#[architect::rpc]` traits (at most 4
  params per method); live updates are `#[subscribe]` streams from
  `architect::PubSub` hubs.
- Logging: `tracing` with structured fields; never `println!`/`eprintln!`
  in library or server code (the CLI's user-facing output is the
  exception).
- Licence GPL-3.0-or-later; crates take `license.workspace = true` (the
  driver keeps `GPL-3.0-or-later AND MIT`).

## Build and test

Dev shell: `nix develop` (or direnv `use flake`) on Linux and macOS — the
pinned Rust 1.94 (rust-toolchain.toml) with wasm, cargo-rail, just; on
Linux also PipeWire, wireplumber, pactl and the webkit/gtk desktop libs.

```bash
just check        # cargo check --workspace --all-targets
just check-web    # the browser remote for wasm32
just test         # cargo test --workspace (+ the PipeWire sandbox tests on Linux)
just fmt          # cargo fmt --all
just ci           # what CI runs: fmt-check, change-check, check, check-web, test
just run          # the app from source (ws://:4046)
just cli health   # the agent CLI against a running engine
just install      # build, install and restart Patchbay on this machine
cargo run -p patchbay --features server --example snapshot   # read-only live-graph smoke
```

CI (`.github/workflows/checks.yml`) runs on `ubuntu-latest` in the nix
dev shell; `Build + test` is the required check.

## Branches, PRs and releases: fleet release flow v4

Load the fleet `release-workflow` skill
(github.com/FastTrackStudios/fleet-skills) before branching, opening or
merging a PR, or releasing. In short:

- Branch from `origin/main` (`feat/sta-123-slug`, `chore/no-issue-slug`),
  open the PR into `main` with a conventional title and
  `.github/pull_request_template.md` filled in, squash-merge. There is no
  `dev` branch.
- Every PR that changes a crate adds a change file:
  `cargo rail change add <crates> --bump <none|patch|minor|major> -m
  "<note> (patchbay)"` (`.changes/README`). CI runs `cargo rail change
  check`. Never edit `CHANGELOG.md` or the workspace version in a normal
  PR.
- Releases are CalVer tags `vYY.M.MICRO` (one version for the repo,
  `[workspace.package] version`): `just release-plan`, then
  `just release-prepare` → `release-pr` → `release-merge` →
  `release-tag`. The tag publishes the GitHub Release
  (`.github/workflows/release.yml`). Hotfixes: `just release-hotfix
  <commit on main>`. The tooling is `scripts/release.sh`, tested end to
  end by `just release-sandbox`.
