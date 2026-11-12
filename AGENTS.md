# Patchbay — agent instructions

Patchbay is the PipeWire studio-routing domain: live graph
(nodes/ports/links), routing presets (connection memory), display
aliases, graph clock (quantum/rate), and the Dante/Inferno stack
switches. The desktop shell is `apps/patchbay` (`fts-patchbay`).

## Architecture

Crate facade pattern:
- `patchbay` — facade: the PipeWire engine + `PatchbayService` impl.
- `patchbay-proto` — wire contract (Facet types + `#[architect::rpc]`).
- `patchbay-ui` — Dioxus components (webview desktop shell today; keep
  it shell-agnostic — no props on `PatchbayApp`, context only).

Apps depend on `patchbay` / `patchbay-ui`, never on internals.

## As an Architect extension

Patchbay is becoming Architect extensions: this crate's core plus one
extension per device family, packaged as its own distribution
(FastTrackStudios/studio#410 has the design and the stack).

- `ext::Patchbay` (feature `extension`, manifest `extension.toml`, id
  `patchbay`) is the core as a linked extension: the `patchbay` view and
  the `PATCHBAY_*` actions (`STATUS`, `HEALTH`, `NOW`, `DEVICE_LIST`,
  `DEVICE_SHOW`), each answering the JSON its `patchbay … --json` verb
  prints. Linked only: no `[entry]`/`[source]` until a linked extension
  can be stopped (FastTrackStudios/architect#577).
- **One engine per machine** (`server::Engine`, held by `home`).
  `build` starts nothing; the first action or view calls
  `home::engine()`, which joins a Patchbay engine already answering on
  the port (`fts-patchbay`, `patchbay serve`, another Architect app;
  probed on the configured host, loopback for `0.0.0.0`/`[::]`) or binds
  the port **before** creating the backend and runs it here; a loser of
  the race retries the join. When a joined engine goes away (its app
  quit), the next call or the view starts or joins again, so a survivor
  takes the port. `fts-patchbay` goes through the same `home` (it joins
  too; a port held by something that is not Patchbay still runs it
  unserved). Every engine and start runs on one runtime per process
  (`server::runtime`). No version check on join yet (#425).
- `PATCHBAY_PIPEWIRE=off` keeps an engine off the machine's PipeWire (no
  connection, no `pw-dump` poller): the tests use it. The engine's
  PipeWire client is always named `patchbay`
  (`pw-cli ls Client | rg -c 'application.name = "patchbay"'`).
- Reports built from several calls (`report::status`,
  `report::collect_health`) live in the crate, so the CLI and the actions
  answer with the same JSON.

## Key rules

- **Headless core (STRICT)**: the engine never touches dioxus. GUIs are
  vox remotes over `PatchbayService`; the desktop app connects through
  an in-process `architect::LocalServer` — the same client remotes use.
- **One PipeWire thread**: all libpipewire objects live on the
  `patchbay-pw` thread (`Rc`/`RefCell`, helvum's engine design).
  Commands go in via `pipewire::channel`; state comes out via the
  shared `GraphStore` mirror + `GraphEvent` mpsc → PubSub hub.
- **Identity is names, not ids**: presets/aliases key on
  `node.name` / `port.name`. PipeWire global ids are never persisted.
- Links are created with `object.linger` — the patchbay edits the
  system graph, it doesn't own it. Never make the engine "clean up"
  links at exit; this machine's graph runs Sunday-worship production.
- Shell-outs (`pw-metadata`, `systemctl --user`) are best-effort and
  live in `clock.rs` / `dante.rs` only.

## Build & smoke

```bash
cargo check -p patchbay -p patchbay-ui -p fts-patchbay
cargo run -p patchbay --example snapshot   # read-only live-graph smoke
cargo run -p fts-patchbay                  # the app (ws on :4046 too)
# everything, the extension and engine start/join/takeover included
# (the engines in the tests never touch PipeWire)
cargo nextest run -p patchbay
```
