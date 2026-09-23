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

The window is built around one question — *which device?* — and two
things to do to it. The rail shows the current device (with its link
state) and switches it; under it:

| | **Route** — where signals go | **Mix** — levels |
|---|---|---|
| **Core Audio** (this Mac) | every app with audio, its level, the device it plays to, one click to capture it into a mix | Loopback/OBS-style mixes: sources summed into virtual devices |
| **PipeWire** (a Linux host) | the node canvas | — |
| **Dante** | the subscription grid: zoomable, with a safe-edit mode for touch | — (Dante only routes, and says so) |
| **Galaxy 32** | its router | its four mixers, line-in trims, AFX inserts, clock |
| **Yamaha TF** | — (the adapter exposes no patching) | channel strips with the desk's own names, colours, icons, faders and ON keys |

Devices are named for what they are, not "System": the rail is meant to
hold several hosts side by side. Below those, two places that aren't
about any one device:

| view | what it is for |
|---|---|
| **Scenes** | everything that can be saved and put back, for every device: presets, device snapshots, the Dante network, mixes |
| **Settings** | appearance, privacy grants, the virtual-device driver, who can reach this Patchbay, the graph clock |

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

### Several machines, one window

Each Patchbay engine serves one machine. Put them in **Settings → Hosts**
(`thebattleship.local`, an IP, or a pasted URL — engines that are open to
the network also find each other over Bonjour, `_patchbay._tcp`, and show
up there to be saved) and the rail's device switcher lists every device
on every one of them, grouped by machine. Picking a device on another
machine makes *that* engine the live one: Route, Mix, Scenes and Settings
are then about it, and the rail says which machine you are on.

The window connects to each engine itself — nothing is proxied and the
engines never talk to each other, so whatever you are holding has to be
able to reach each host on its RPC port, and each host has to be open to
the network. A machine on several networks is dialled on all its
addresses at once and the first to answer wins. The host list is kept by
the engine the window came from, so the desktop app and a phone pointed
at the same machine offer the same hosts. If a remote engine drops, the
window comes back home and keeps retrying it.

What you were doing — view, device, machine, Dante grid zoom — is
remembered per browser, so a phone that reloads lands where it was.

### After a change

```bash
just install
```

builds, installs and **restarts** Patchbay on this machine and waits until
it answers — on macOS that is build + sign + install + relaunch
(`packaging/macos/deploy.sh`, runnable directly where `just` isn't
installed). The browser remote is compiled into the app, so this is also
how a UI change reaches a phone. The engine drops for a couple of seconds;
saved mixes come back on their own.

### On a phone or tablet

The browser remote is the same interface, laid out for the screen it is
on: below 760px the rail becomes a tab bar inside the safe area, Mixes
turns into a list that opens an editor, strips and grids scroll sideways
under sticky headers, and anything you touch is sized for a finger (this
follows the pointer, not the width, so a touch laptop gets it too). The
Graph canvas pans with one finger and zooms with two.

To reach it from another device: Settings → Network → *open to the
network*, restart Patchbay, then open one of the listed URLs
(`http://<this-machine's-ip>:4046/`). "Add to Home Screen" runs it
full-screen. If the phone sleeps or the engine restarts, the remote
reconnects by itself.

Settings → Appearance picks a theme (Midnight by default; Auto, Studio,
Console, Daylight, Contrast), an accent and a density. It is stored per device in
the browser, never in the engine config — the phone on the music stand
can be true black while the desktop stays as it was. A theme is ~30
lines of base colours in `crates/ui/src/theme/tokens.css`; every tint,
glow and hairline is mixed from those.

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

### External devices

Hardware with its own router or mixer (Antelope Galaxy32, Yamaha TF, the
Dante network, …) and the machine's own audio system are controlled through
the same RPC surface — the device switcher in the app's rail, and
`patchbay device` for agents.

**Mix** shows each device as the thing it is: a **Yamaha TF** as channel
strips carrying the desk's own names, colours, icons, faders and ON keys; a
**Galaxy 32** as its router, its four mixers, the line-in trims and the AFX
insert grid; the host's own audio layer as a device list. Everything the
device reports is still there, under **Inspector** — the parameter tree is
the right tool for reverse-engineering and for anything a console page
doesn't cover, and the wrong first thing to show someone who wants a fader.

Two honesty notes the pages carry themselves: the Galaxy never announces
routing changes made elsewhere, so its router can be up to five seconds
behind the hardware panel; and its trim page says out loud that ALL mode
moves all 32 line inputs at once.

With **no config at all**, four entries come up, each auto-discovered and
each failing soft (a missing device never affects the others):

| name | kind | found by | state when absent |
|---|---|---|---|
| `system-audio` | `system-audio` | Core Audio (macOS) / the PipeWire graph (Linux); read-only summary | — |
| `galaxy32` | `antelope-galaxy32` | Antelope Manager Server multicast announce | `not found` |
| `tf1` | `yamaha-tf` | TCP 49280 probe of the local /24 subnets + `devinfo productname` (read-only); last address cached in `<config>.state.json` | `not found` |
| `dante` | `dante` | mDNS (`_netaudio-arc`) + ARC reads; the whole network is one device | `not found` |

`device list` shows each one's state: `searching`, `online`, `not found`
(discovery found nothing; it keeps looking), `offline` (was found, went
away; it reconnects), `connecting` (pinned `addr`), `disabled`. A `devices`
section in the config replaces the defaults; any entry takes
`enabled false`, `addr` (pin an endpoint) and `serial`:

```styx
devices ({name system-audio, kind system-audio}
         {name galaxy32, kind antelope-galaxy32}
         {name tf1, kind yamaha-tf, addr "192.168.1.214"}
         {name dante, kind dante, enabled false})
```

`PATCHBAY_DEVICES=off` disables the device layer entirely. See
`docs/devices.md`.
On a host without PipeWire (macOS), `patchbay serve` runs the engine
headless so the device layer is reachable without the desktop window.

```bash
patchbay serve                                          # headless engine on ws://127.0.0.1:4046/vox
patchbay device list --json
patchbay device show galaxy                             # id, config name, or a model/serial substring
patchbay device params galaxy mixer/1/strip/16 --json  # prefix = whole path segments
patchbay device get galaxy monitor/dim
patchbay device set galaxy mixer/1/strip/16/level -12  # dB, -inf, on/off, L/C/R, enum labels
patchbay device set galaxy clock/sample_rate 48000 --allow-disruptive
patchbay device route galaxy DIGI_OUT0:17 COM_PLAY1:1  # 1-based; group id or name; `none` clears
patchbay device watch galaxy --json                     # one JSON event per line
patchbay device snapshot save galaxy sunday --include mixer route/DIGI_OUT0
patchbay device snapshot list
patchbay device snapshot diff sunday                    # what a restore would change (read-only)
patchbay device snapshot restore sunday --dry-run
patchbay device snapshot restore sunday --only mixer/1/strip/16
```

Every write answers with the value **read back from the device**, not the
value requested. Params flagged disruptive (clock source, sample rate, TF
scene recall) are refused without `--allow-disruptive`. A snapshot restore
diffs against the live device first and writes only what differs; skipped
and failed items are listed per path, and a failed item makes the command
exit non-zero.

## macOS: Patchbay.app

On macOS the engine runs inside a signed **Patchbay.app**. The app owns
the privacy grants: System Audio Recording (Core Audio process taps) and
Microphone. The `patchbay` CLI is only an RPC client of the running app,
so agents and scripts never need a privacy permission of their own.

```bash
just macos-install          # build + sign + install (runs macos-app, macos-sign)
open ~/Applications/Patchbay.app
patchbay health --json      # ~/.local/bin/patchbay -> the CLI inside the app
```

| recipe | what it does |
|---|---|
| `just macos-app` | release build, assembled as `target/macos/Patchbay.app` (unsigned) |
| `just macos-sign` | `macos-app`, then hardened-runtime Developer ID signing, then `codesign --verify --deep --strict` and `spctl` |
| `just macos-install` | `macos-sign`, then an atomic replace of `~/Applications/Patchbay.app` (`PATCHBAY_APP_DIR=/Applications` to change the folder) and the `~/.local/bin/patchbay` symlink |
| `just macos-notarize` | notarize and staple the signed app, for other Macs |
| `just macos-icon` | regenerate `AppIcon.icns` from `app/assets/icon.svg` (the `.icns` is checked in) |

Bundle layout: `Contents/MacOS/Patchbay` is the app and engine.
`Contents/Helpers/patchbay` is the CLI. It sits in `Helpers/` because on
case-insensitive APFS, `patchbay` and `Patchbay` in the same folder would
be the same file. The packaging sources are in `packaging/macos/`: the
`Info.plist` template, the entitlements and the scripts.

**The bundled app, compared with `cargo run`:**

- It serves RPC on `127.0.0.1:4046` by default. Set
  `PATCHBAY_ADDR=0.0.0.0:4046` to reach it from the LAN.
- It exits if something already listens on that port, such as a second
  copy or `patchbay serve`.
- It logs to `~/Library/Logs/Patchbay/patchbay.log`, and the previous
  run's log is kept as `.log.1`.
- Its working directory is `$HOME`, and Homebrew is added to `PATH`.
- Its open-file limit is raised from launchd's 256. LAN discovery probes
  hundreds of hosts at once.

Config lives in `~/Library/Application Support/fts/patchbay/`.

**Signing.** `macos-sign` unlocks the dedicated build keychain before
it calls `codesign --keychain …`. With `PATCHBAY_KEYCHAIN_PW` exported
(e.g. in your shell profile) it is non-interactive; without it `security`
asks for the password once per session. The password is never stored in
this repo. The keychain is `fts-build.keychain`, created by
`signal/apps/desktop/ios/setup-keychain.sh` with a key partition list for
`apple-tool:`/`apple:`. These variables override the defaults:

- `PATCHBAY_KEYCHAIN`: the keychain name.
- `PATCHBAY_KEYCHAIN_PW`: its password (no default).
- `PATCHBAY_SIGN_ID`: the identity. The default is `Developer ID
  Application: CODY JAMES WRIGHT (28C2G63DA7)`.

If the keychain doesn't exist, the default keychain search list is used.

**First run and permissions.** At launch Patchbay checks System Audio
Recording and Microphone:

- **Undecided:** it triggers the system prompts. For audio capture it
  briefly starts a private, non-muting tap, which is the supported way to
  get the prompt.
- **Denied:** it shows an alert whose "Open System Settings" button goes
  to the right pane. After "Not Now" the alert stays away until the next
  app version.
- **Asking again:** run `patchbay permissions request` while the app is
  running, or quit Patchbay and run
  `open -a Patchbay --args --request-permissions`.

`patchbay permissions [--json]` shows System Audio Recording, Microphone
and Local Network as the running app sees them. Local Network is needed
for Dante and Yamaha TF discovery. The last result is also written to
`~/Library/Application Support/fts/patchbay/permissions.json`.

To grant or change audio capture by hand, go to **System Settings →
Privacy & Security → Screen & System Audio Recording → "System Audio
Recording Only"** and turn on Patchbay, then relaunch Patchbay. Until you
do, taps are created but deliver silence.

**Why Developer ID signing matters.** TCC records a grant against the
app's bundle id (`app.fasttrackstudio.patchbay`) and its code-signing
requirement, which for a Developer ID app means the bundle id plus the
team (`28C2G63DA7`). The grant therefore survives every rebuild signed
with the same certificate. An ad-hoc or unsigned build has a requirement
tied to its exact binary hash, so each rebuild looks like a new app and
must be approved again. A `cargo run` binary gets no grant at all,
because the grant goes to the terminal that launched it.

**Distributing to other Macs.** Gatekeeper rejects a signed but
unnotarized app as "Unnotarized Developer ID" when it is downloaded.
First store an app-specific password in the keychain (one time):

```bash
xcrun notarytool store-credentials patchbay-notary \
    --apple-id <apple id> --team-id 28C2G63DA7 --password <app-specific password>
```

Then `just macos-sign && just macos-notarize` submits the app, waits,
staples the ticket and leaves `target/macos/Patchbay.zip` ready to
share. `PATCHBAY_NOTARY_PROFILE` overrides the profile name.

`dx` builds the browser remote. When `dx` isn't installed and
`app/web-dist/` isn't staged, `macos-app` builds without `embed-web`.
The desktop window and the RPC still work, but `http://127.0.0.1:4046/`
serves no web UI.

## Mixes and virtual devices (macOS)

A **mix** sums sources — an app's audio, one app's output to one device
with every channel unmixed, an input device's channels, all system audio
— into outputs, with per-source and per-output gain, mute and meters.
Mixes run inside Patchbay.app (which holds the System Audio Recording
grant), are saved in the config, and are rebuilt automatically when an
app they tap starts or quits.

`Patchbay.driver` publishes the **virtual devices** mixes send to, and
Patchbay creates, renames and removes them **at runtime** — no coreaudiod
restart, and a rename keeps the uid so apps keep their selection. Each
device is a loopback: what an app plays into it comes back out of its
input. Out of the box:

| device | channels | for |
|---|---|---|
| **Patchbay** | 16 | apps pick it as their **output**; Patchbay takes that audio |
| **Broadcast** | 2 | apps pick it as their **input/mic** (Discord, `FaceTime`, Zoom) |

```bash
packaging/macos/install-driver.sh     # once (admin; restarts coreaudiod)
# optional: no password for later driver reloads (see the script's header)
sudo packaging/macos/allow-driver-reload.sh

patchbay mix targets                  # apps and devices a mix can use
patchbay mix create Discord --source 'app:REAPER:Galaxy32@0:0,1:1' --output Broadcast
patchbay mix meters --watch           # live levels
patchbay mix level Discord 1 -6       # source 1 to -6 dB (or `mute` / `unmute`)

patchbay virtual create "Stream Mix" --channels 2
patchbay virtual rename "Stream Mix" "OBS Feed"
patchbay virtual aggregate create "REAPER I/O" Galaxy32 Patchbay
```

A source is `app:<name|bundle>[@map]` (the app's stereo mixdown),
`app:<name>:<output device>[@map]` (what it plays to that device, every
channel — e.g. REAPER's outs 33–34 on a 64-channel interface with
`@32:0,33:1`), `input:<device>[@map]` or `system[@map]`. Maps are
0-based `src:dst` pairs; the default is `0:0,1:1`. The **Mixes** tab in
the app and browser remote has the same controls as channel strips.

A source can **copy** or take **exclusive** use of an app. Copying is
the default: the app keeps playing wherever it was and the mix gets a
duplicate. Exclusive silences the app on its own output device while
the mix runs, so its audio comes out of the mix instead of there — the
way to send one app somewhere else on a system with no per-app output
setting. Toggle it on the source strip, or:

```
patchbay mix add-source Stream app:Brave --exclusive
```

Into a DAW that opens one device, either point it at a Patchbay
**aggregate** (`Galaxy32 + Patchbay`, so app audio arrives as extra
inputs after the interface's own), or loop spare interface playback
channels back to its inputs with the device's own router.

## Opening it to the network

The app serves the same UI to a browser at `http://<host>:4046/`, so a
laptop or tablet on the same network can drive the rig. It is off by
default:

```
patchbay listen            # where it listens now, and the URLs to use
patchbay listen lan        # every interface (restart Patchbay to apply)
patchbay listen local      # back to this machine only
```

`patchbay listen lan` prints every address that will answer, including
the `.local` name (`http://airlock.local:4046/`). Apple devices resolve
that directly; Windows and Android may need the IP.

**The RPC is unauthenticated.** Anything that can reach that port can
re-route this machine's audio and write to the consoles Patchbay is
connected to — the Galaxy 32's router and the TF-1's faders included.
That is reasonable on a studio network you control and not on a shared
one. `PATCHBAY_ADDR` still overrides the saved setting, and the browser
remote needs a build with `dx` installed (`cargo install dioxus-cli`),
which `packaging/macos/build-app.sh` embeds automatically.

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
