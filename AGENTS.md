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
  the port (`fts-patchbay`, `patchbay serve`, `patchbay-app`, another
  Architect app; probed on the configured host, loopback for
  `0.0.0.0`/`[::]`) or binds the port **before** creating the backend and
  runs it here. When a joined engine goes away (its app quit), the next
  call or the view starts or joins again, so a survivor takes the port.
  Every starter goes through `home`, the macOS bundle included (it joins
  rather than exiting). Every engine and start runs on one runtime per
  process (`server::runtime`). No version check on join yet (#425).
- **Never a second engine while a Patchbay holds the port.** A join that
  is refused takes the port; one that connects and gets no answer keeps
  trying until `join_deadline` (20 s) and then fails (`NoAnswer`); only a
  holder that answers and is not Patchbay lets `IfTaken::RunUnserved`
  (`fts-patchbay`) run the engine unserved.
- **The engine's config decides LAN serving, not the starter**
  (`server::listen_addr`): `PATCHBAY_ADDR` (a per-process override for
  scratch engines), else the config's `network.bind` (`patchbay listen
  lan|local`), else `DEFAULT_ADDR` (loopback on macOS, every interface on
  Linux), for every starter and every takeover.
- `PATCHBAY_PIPEWIRE=off` keeps an engine off the machine's PipeWire: it
  gates the connection and the `pw-dump` poller only (see
  `server::engine::pipewire_enabled` for what still runs). It is logged as
  a warning at start, and `engine_info` (`status`'s and `health`'s
  `pipewire`: `on`/`off`/`unsupported`) says so. The tests use it. The
  engine's PipeWire client is always named `patchbay`
  (`pw-cli ls Client | rg -c 'application.name = "patchbay"'`).
- **The Patchbay distribution** (`apps/patchbay/src/distribution.rs`,
  `patchbay-app`, feature `app`, `just patchbay-app`): Architect's window
  with the `patchbay` bundle linked in, app and folder `patchbay`, bundle
  id `app.fasttrackstudio.patchbay`. Beside `fts-patchbay` until that
  becomes the distribution (#428).
- Reports built from several calls (`report::status`,
  `report::collect_health`) live in the crate, so the CLI and the actions
  answer with the same JSON.

## One UI for every renderer

The same components draw in Architect's window (Blitz), the desktop
WebView and the browser remote (dioxus-web). Load the `blitz-design`
skill before UI work.

- **No page APIs in `src/ui`.** What only the page can do (saved
  preferences, the system colour scheme, the page's chrome, a coarse
  pointer) comes from the shell as `ui::Page` (`Shell::page`). Browser
  shells give `apps/patchbay/src/page.rs` (JavaScript, `localStorage`);
  Architect's window gives `home::native_page` (prefs in `ui.json` beside
  the engine config). No `document::eval` in `src/ui`.
- **Controls Blitz can draw.** `ui::ui::Choice` instead of `<select>`,
  `ui::ui::Slider` instead of `<input type=range>` (`on_input` live,
  `on_commit` on release), `ui::ui::TextField` (commit on Enter/blur)
  instead of `onchange`. Blitz has no `<select>`, no range input and no
  `change` event.
- **Blitz's CSS gaps** (`overflow: auto`, `position: fixed/sticky`,
  viewport units) are covered by `ui/theme/native.css`, loaded only when
  the page is native, so browsers keep the main sheets as they are.
- **Look at it in Blitz**: `cargo run -p fts-patchbay --features native
  --example blitz_shot -- DIR W H` draws each view into `DIR/<view>.png`
  (`--live ws://…/vox` reads a running engine, never starts one;
  `--window` opens a fixed-size Blitz window to click through).

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
