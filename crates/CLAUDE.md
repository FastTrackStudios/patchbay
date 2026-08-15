# Patchbay — Claude Code Instructions

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
```
