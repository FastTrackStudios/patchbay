# patchbay

A PipeWire routing app for studios — see every node and port on the
machine, wire them together, and keep the result.

Built for a live rig, where "which output is the drummer's headphone mix
on today" needs answering in seconds, not by reading `pw-link -l` output.

## What is here

| crate | what it is |
|---|---|
| `patchbay-proto` | the wire contract |
| `patchbay` | the engine — PipeWire graph state, links, persistence |
| `patchbay-ui` | the Dioxus interface |
| `fts-patchbay` | the binary (serves the router on `ws://:4046`) |
| `patchbay-web` | the browser remote |

The engine is headless and the UI is a client, so the desktop app and the
browser remote are the same program seen through different windows.

## Running it

```bash
cargo run -p fts-patchbay
```

Then open the desktop window, or point a browser at the served remote.

## Where it came from

Extracted from the [FastTrackStudio
monorepo](https://github.com/FastTrackStudios/FastTrackStudio) in August
2026. It was always a leaf — the monorepo's CI already gave it its own
gate, separate from the workspace one — and it depends on nothing from
the audio-production stack around it.

Dependencies that remain, all external:

- [architect](https://github.com/FastTrackStudios/architect) — RPC, entity
  framework, transports
- [music-convention](https://github.com/FastTrackStudios/music-convention)
  — `music-catalog`, used to colour ports by instrument name
- [inferno-control](https://codeberg.org/FastTrackStudios/inferno-control)
  — `inferno-net`, for Dante/AoIP devices
- [vendor](https://github.com/FastTrackStudios/vendor) — patched `phon` /
  `phon-jit` / `styx-format`

## Licence

MIT OR Apache-2.0.
