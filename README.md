# patchbay

A PipeWire routing app for studios — see every node and port on the
machine, wire them together, and keep the result.

Built for a live rig, where "which output is the drummer's headphone mix
on today" needs answering in seconds, not by reading `pw-link -l` output.

## What is here

| crate | what it is |
|---|---|
| `patchbay-proto` | the wire contract |
| `patchbay` (crate) | the engine — PipeWire graph state, links, persistence |
| `patchbay-ui` | the Dioxus interface |
| `fts-patchbay` | the desktop app + RPC server (`ws://:4046`) |
| `patchbay` (binary) | the agent/script CLI for the same RPC surface |
| `patchbay-web` | the browser remote |

The engine is headless and the UI is a client, so the desktop app and the
browser remote are the same program seen through different windows.

## Running it

```bash
# Desktop app and local RPC server
cargo run -p fts-patchbay --bin fts-patchbay

# Agent CLI against a running app
cargo run -p fts-patchbay --bin patchbay -- --help
cargo run -p fts-patchbay --bin patchbay -- health --json
```

The app serves the browser remote at `http://127.0.0.1:4046/` when a web
bundle is available. The CLI connects to that app over
`ws://127.0.0.1:4046/vox`; use `--url ws://host:4046/vox` for another rig.
Use `--local` only for an intentional private/headless engine:

```bash
cargo run -p fts-patchbay --bin patchbay -- --local graph --json
```

The RPC endpoint is currently unauthenticated. Keep it on loopback
(`PATCHBAY_ADDR=127.0.0.1:4046`) or an isolated trusted studio network until
authentication/TLS is added; anyone who can reach it can change routing.

The installed names are `patchbay` for the CLI and `patchbay-app` for the
desktop app. The CLI is designed for agents: use `--json`, stable node/port
names or aliases, and explicit mutations. Typical workflows are:

```bash
patchbay health --json
patchbay health --json --strict   # non-zero exit when an error is found
patchbay graph --json
patchbay nodes --json
patchbay ports "Inferno source" --json
patchbay route bank inferno-to-reaper "Inferno source" REAPER
patchbay dante health --json
patchbay dante list --json
patchbay dante subscribe "Galaxy32" 1 "Inferno" "TX 1"
patchbay dante save
patchbay dante repair --apply-config
```

`health`/`dante health` are read-only scans. `dante repair` only performs
actions explicitly requested (`--start-stack`, `--restart-failed`, and/or
`--apply-config`; `--all` enables all three), so an agent cannot silently
rewrite Dante hardware routing. `dante save` snapshots the live routing and
`dante apply` restores that saved snapshot non-destructively.

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
