# Host backends: the machine's own audio system

In patchbay everything is an **adapter** with capabilities. There are two
kinds:

| kind | examples | routing model | crate |
|---|---|---|---|
| hardware adapter | Galaxy32, Yamaha TF1, Dante | **matrix**: one source per destination, plus params | `patchbay-device` + `crates/adapters/*` (see `devices.md`) |
| host backend | PipeWire (Linux), Core Audio (macOS) | **graph**: many-to-many links that mix, virtual devices, per-app streams, meters | `patchbay-host` + `patchbay-host-<backend>` |

A rig can run hardware adapters only, a host backend only, or both.

```
crates/host             patchbay-host            platform-independent graph model + HostBackend trait (no FFI)
crates/host-coreaudio   patchbay-host-coreaudio  macOS Core Audio backend (this doc, milestones 1–2)
crates/host-pipewire    patchbay-host-pipewire   (planned) today's crates/patchbay engine behind HostBackend
```

## The model (`patchbay-host`)

| Concept | Type | Notes |
|---|---|---|
| Node | `HostNode{id, name, kind, direction, ports, sample_rate, app, props}` | `kind`: `HardwareDevice`, `VirtualDevice`, `AppStream`, `Aggregate`. `id` is stable and namespaced by backend (`coreaudio:device:<DeviceUID>`). `props` holds backend-specific extras and is never part of identity. |
| Port | `PortRef{node, channel, direction}` | Channels are 0-based. Directions are from the **graph's** point of view: an `Output` port produces audio. A microphone's capture channels are graph *outputs* and a speaker's playback channels are graph *inputs*, which is PipeWire's convention. `PortCounts{inputs, outputs}` and the derived `NodeDirection` (`Source`/`Sink`/`Duplex`/`None`) use the same convention. |
| App | `AppInfo{pid, bundle_id, name}` | Persist `bundle_id`. Never persist `pid`. |
| Link | `HostLink{from, to, gain, enabled}` | **Mixing.** Any number of links can feed one input port. There is at most one link per `(from, to)` pair. `gain` is linear, from 0 to `MAX_GAIN` (4.0 ≈ +12 dB). |
| Virtual device | `VirtualDeviceSpec{name, channels, sources, monitors}` | Mirrors Rogue Amoeba Loopback. A source (`SourceSpec{kind, channel_map, volume, enabled}`) is one of `App{AppSelector}` (bundle id or pid), `InputDevice{uid}`, `SystemAudio` or `PassThru`. A monitor (`MonitorSpec{device_uid, channel_map, volume, enabled}`) listens through to an output. |
| Channel map | `ChannelMap(Vec<ChannelPair{src, dst}>)` | Fan-out and mixing are allowed. Duplicate pairs are rejected. Helpers: `identity`, `offset`, `parse("0:0,1:1")`. |
| Capabilities | `HostCapabilities{graph_links, virtual_devices, app_capture, pass_thru_device, meters}` | UIs hide what is missing. Backends refuse it with `HostError::Unsupported`. |
| Snapshot | `HostSnapshot{backend, capabilities, nodes, links}` | Read from the OS. |
| Events | `HostEvent` | `NodeAdded`, `NodeChanged`, `NodeRemoved`, `LinkAdded`, `LinkChanged`, `LinkRemoved`, `DefaultDeviceChanged`, `SnapshotReplaced`, delivered through `tokio::sync::broadcast`. A lagged receiver should re-snapshot. |
| Behaviour | `HostBackend` / `DynHostBackend` | `name`, `capabilities`, `snapshot`, `create_link`, `remove_link`, `create_virtual_device`, `remove_virtual_device`, `set_volume(VolumeTarget, gain)`, `subscribe`. Same shape as `DeviceAdapter`: native `async fn` returning `Send` futures (not object safe), plus a blanket-implemented, boxed-future `DynHostBackend` for `Box<dyn …>`. |
| Errors | `HostError` | Clonable string variants: `Unsupported`, `NotFound`, `InvalidSpec`, `PermissionDenied`, `Os{op, status}`, `Timeout`, `Closed`. |
| Planning | `validate_link`, `diff_nodes`, `ChannelMap::validate`, `VirtualDeviceSpec::validate`, `Gain::validate` | Pure functions with unit tests. Every backend runs them before it touches the OS. |

Every backend keeps this contract:

- **The OS is the source of truth.** Events report what the OS reports, such as hot-plugs and apps starting to play, not only changes patchbay made.
- Backends never change system defaults and never mute other apps on their own.
- Persistence follows the platform. PipeWire links linger and are never cleaned up at exit. Core Audio objects are owned by the process and are torn down on `Drop` until the HAL plug-in exists (see below).

## Core Audio backend (`patchbay-host-coreaudio`)

All real code is `#[cfg(target_os = "macos")]`. On any other OS the crate
builds a stub with the same public names (`CoreAudioBackend`,
`TapMonitor`, …) whose constructors return `HostError::Unsupported`, and it
pulls in no macOS dependencies (they sit under
`[target.'cfg(target_os = "macos")'.dependencies]`).

Bindings come from `objc2-core-audio` 0.3.2 (the latest), plus
`objc2-core-audio-types`, `objc2-core-foundation`, `objc2-foundation` and
`libc`. Every symbol needed was already there, including
`AudioHardwareCreateProcessTap`/`DestroyProcessTap`, `CATapDescription`,
`AudioHardwareCreateAggregateDevice`, the process-object properties and the
aggregate/tap dictionary keys, so there are **no hand-written
`extern "C"` bindings**. The only extra is the private
`TCCAccessPreflight`, which is loaded with `dlopen` (see TCC below).

### Module layout (FFI isolation)

```
lib.rs        #![deny(unsafe_code)] facade
config.rs     TapMonitorConfig, TapMute, CapturePermission (compiled on every OS)
enumerate.rs  HAL facts → HostNode (identity, graph-direction mapping)
backend.rs    CoreAudioBackend: HostBackend impl, watcher thread
monitor.rs    TapMonitor + the real-time Router (safe code)
unsupported.rs  non-macOS stub
ffi/          the ONLY place with `unsafe` (#![allow(unsafe_code)], every block has a SAFETY comment)
  property.rs   typed AudioObjectGetPropertyData (Pod values, CFString +1 ownership, AudioBufferList parsing)
  hal.rs        devices / processes / defaults as plain structs; re-exports listener selectors
  listener.rs   RAII AudioObjectAddPropertyListener (boxed Rust callback, removed on Drop)
  tap.rs        RAII ProcessTap (CATapDescription) + RAII private AggregateDevice
  ioproc.rs     RAII IOProc (Arc'd render state) + safe Buffers/BuffersMut views
  sys.rs        proc_name, TCC preflight
```

### Milestone 1: enumeration and change notifications (done)

- **Devices.** Each device reports its UID, name, manufacturer, transport (`bltn`, `usb `, `thun`, `pci `, `hdmi`, `dprt`, `virt`, `grup`), nominal rate, and input/output channel counts summed over its stream configuration. `alive` and `running_somewhere` go into `props`. Kind: aggregate transport (`grup`) → `Aggregate`, virtual transport (`virt`) → `VirtualDevice`, anything else → `HardwareDevice`.
- **Processes** (`kAudioHardwarePropertyProcessObjectList`, macOS 14.2+). Each process reports its pid, bundle id, name (`proc_name`, falling back to the bundle id) and `running`/`running_output`/`running_input`. It becomes an `AppStream` node with 2 output ports, because patchbay can take a stereo mixdown from it. Node id is `coreaudio:process:<pid>`, which is stable for the life of the process. Persist the bundle id.
- **Events.**
  - System-object listeners watch the device list, the process list, and the default output and input devices.
  - Per-object listeners are reconciled after every topology change. Devices are watched for nominal rate, stream configuration and alive state; processes for is-running and is-running-output, with wildcard scope.
  - Listener callbacks only push onto an unbounded mpsc channel, so the HAL thread never blocks.
  - A `patchbay-ca-watch` worker debounces for 40 ms, re-enumerates, diffs against the cached node list with `diff_nodes`, and broadcasts the result.
- `examples/ca_list.rs` prints the snapshot as JSON. `--watch N` also prints events as JSON lines.

### Milestone 2: Loopback-style monitoring without a HAL driver (first cut)

```
 app process(es) ──CATapDescription(stereo mixdown, private, mute behaviour)──▶ process tap
 private aggregate  = main subdevice: <output device>  +  tap list: [tap, drift compensation on]
 IOProc on aggregate:  input = [subdevice capture streams…, tap stream]
                       output = output device streams
                       out[dst] += tap[src] × gain   (per ChannelMap pair), meters = pre-gain peaks
```

`TapMonitor::start(&TapMonitorConfig)` / `start_with_timeout` works as follows:

1. It resolves the `AppSelector`. A pid goes through `TranslatePIDToProcessObject`. A bundle id matches every process object with that id. patchbay never taps itself.
2. It creates the tap. The tap is **private** and a stereo mixdown. `TapMute::Unmuted` is the default; patchbay never silences a user's app unless asked.
3. It reads `kAudioTapPropertyFormat` and requires Float32.
4. It validates the channel map against the tap's channels and the output device's channels.
5. It creates a **private**, unstacked aggregate with tap auto-start. The output device is the main subdevice, which gives one clock domain, and drift compensation is on. It records the aggregate's stream layout.
6. It registers and starts an IOProc.

The render state is built once and then only touched through atomics:

- routes pre-resolved to `usize`
- gain as `AtomicU32` holding `f32` bits (`set_gain` is lock-free)
- per-channel peaks, using `fetch_max` on the bit patterns of non-negative floats
- a cycle counter and the observed buffer counts

The callback does not allocate or lock. It zeroes the aggregate's output and
mixes the routed channels in. The tap's buffers start after the output
device's own capture streams.

**Teardown is `Drop`.** Field order makes the IO stop first, then
`AudioDeviceDestroyIOProcID` (which synchronises with the IO thread), then
the aggregate is destroyed, then the tap. The render `Arc` is freed only
after the proc is gone. If a destroy call fails because the device already
vanished, the callback state is leaked rather than risk a use-after-free.
`start_with_timeout` runs creation on a helper thread. If it times out, the
late result is dropped, which tears it down. The same applies to property
listeners.

`examples/ca_tap_monitor.rs <bundle-id-or-pid> <output-uid> [--seconds N] [--gain G] [--map 0:0,1:1] [--mute] [--timeout S]`
prints the permission preflight, the composition (`TapMonitorInfo`), a
per-second line with cycles, buffer counts and tap peaks in dBFS, and the
teardown result. It checks that the aggregate is gone and then prints a
diagnosis:

- IO never ran
- tap silent while the app was playing (a permission problem)
- tap silent because the app was idle
- OK

### What ran on the development Mac (macOS 27, Apple silicon, Mac mini)

- **`ca_list`.** 7 devices and 23 processes. Devices: Galaxy32 64×64 `thun`, UA Thunderbolt 36/34 `pci `, Axe-Fx III 8×8 `usb `, ATEM Mini Pro capture 2 ch, Mac mini Speakers, and two monitors over HDMI and DisplayPort. The processes included Brave, Claude, ControlCenter, sunshine and others. Defaults were Galaxy32 out and UA Thunderbolt in.
- **`ca_list --watch`** while a `say` process ran. Events arrived in order: `node_added` (say, not yet running), `node_changed` (Mac mini Speakers `running_somewhere` true), `node_changed` (say `running_output` true), `node_removed` (say), then `node_changed` (speakers idle again).
- **`ca_tap_monitor <say pid> BuiltInSpeakerDevice --mute --gain 0.01`.** Test sound: `say -a "Mac mini Speakers"` at `[[volm 0.001]]`, so nothing audible. Results:
  - tap and private aggregate created in about 25 ms
  - aggregate layout in `[2]`, out `[2]`
  - IO ran at about 93 cycles/s (512 frames at 48 kHz)
  - teardown clean, aggregate gone
  - The tap delivered **digital silence**. `TCCAccessPreflight(kTCCServiceAudioCapture)` returns `2` (not determined) for the responsible process, which is the Claude Code CLI binary. This is the documented behaviour when "System Audio Recording" isn't granted: the tap is created but its samples are zero. The example reports it instead of hanging.
- Tapping an **idle** app (ControlCenter) created everything but the aggregate never ran IO (0 cycles). A tap-bearing aggregate appears to clock only while the tapped process runs output. **Re-verify this once permission is granted.** It is harmless because the monitor simply idles until the app plays.
- Error paths:
  - unknown device → `NotFound`
  - unknown bundle id → `NotFound`
  - channel map out of range → `InvalidSpec`
  - capture-only output device → `InvalidSpec`

  No aggregates were left behind by any run.

`afplay` to the default output (Galaxy32) hung without ever starting IO on
this machine at the time, so tests used `say -a <device>`. No system default
was changed.

### TCC: "System Audio Recording" and Patchbay.app

**Who is asked.** Process taps require the *responsible* app to hold
"System Audio Recording". Without it the tap is created, but its samples
are zero. The grant is made in System Settings → Privacy & Security →
Screen & System Audio Recording → "System Audio Recording Only". The app
bundle also needs `NSAudioCaptureUsageDescription` in its Info.plist, or
macOS never shows the prompt.

- **Patchbay.app.** On macOS the engine runs inside `Patchbay.app`
  (`packaging/macos/`, `just macos-install`), so the app is the
  responsible app. The `patchbay` CLI is an RPC client and never needs a
  grant.
- **`cargo run` or `patchbay serve`.** The responsible app is the
  terminal, IDE or agent host that launched the process.

**What the bundle carries.**

- `Info.plist`: `NSAudioCaptureUsageDescription`,
  `NSMicrophoneUsageDescription`, `NSLocalNetworkUsageDescription`, and
  `LSMinimumSystemVersion` 14.2.
- Hardened runtime with these entitlements:
  - `com.apple.security.device.audio-input`
  - `com.apple.security.cs.allow-jit`

  vox's codec JIT (phon → weavy → copypatch) maps `MAP_JIT` pages. Without
  `allow-jit`, both the app and the CLI panic with `mmap(MAP_JIT) failed`.
  WKWebView needs nothing extra, because its JIT runs in Apple's
  WebContent process.

**Why the grant survives rebuilds.** TCC stores the grant against the
bundle id and the code-signing requirement. With a Developer ID
signature that requirement is `identifier app.fasttrackstudio.patchbay`
plus the team `28C2G63DA7`, so every rebuild signed with the same
certificate keeps the grant. This was verified: a grant made on one
build was still `granted` after a rebuild, re-sign and reinstall. An
ad-hoc signature pins the binary's hash instead, so every build would be
asked again.

**Checking the state.** `capture_permission()` preflights without
prompting. It uses the private `TCCAccessPreflight` SPI through `dlopen`,
as insidegui/AudioCap does. It returns `Granted`, `Denied`,
`NotDetermined`, or `Unknown` when the SPI is missing. libTCC caches the
answer **inside the process**. On macOS 27, after the user allowed the
prompt, tccd logged the grant (`AUTHREQ_RESULT authValue=2`), but every
later in-process preflight still said "not determined". Polling the
preflight therefore can't observe the user's answer.

**Asking.** `request_capture_permission()` does the following:

1. It creates a private, non-muting global tap that excludes Patchbay
   itself.
2. It wraps the tap in a private, tap-only aggregate. There is
   deliberately no output subdevice: with a duplex default output (the
   Galaxy32), `coreaudiod` asked for Microphone first and the tap
   blocked behind that prompt.
3. It runs a no-op `IOProc` for 200 ms, then tears everything down.

Creating the tap is what makes macOS show the prompt (tccd logs
`AUTHREQ_PROMPTING service=kTCCServiceAudioCapture` from `coreaudiod` on
behalf of Patchbay). Tap creation blocks until the user answers.

With the `tcc-spi` feature (the app enables it), the private
`TCCAccessRequest` is then used to *read the decision*. It shows no
second prompt once the decision is made. It also shows the prompt itself
if the tap probe could not be set up. This is private SPI, which is fine
for Developer ID but not for the App Store.

**Microphone.** `request_microphone_permission()` uses
`AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio`.

**Settings and alerts.** `open_capture_settings()` opens
`x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AudioCapture`.
That anchor is present in the macOS 27 PrivacySecurity extension. If it
can't be opened, it falls back to the legacy
`com.apple.preference.security` pane and then to the Screen Recording
anchor. `permission_alert()` shows an `NSAlert` on the main thread.

**The app's flow (`app/src/macos.rs`).**

- **When it runs.** Once the window is up. Touching TCC or AVFoundation
  while AppKit is still registering the app races that registration.
- **Undecided permissions.** The flow shows the system prompts.
- **Denied permissions.** It shows an alert with "Open System Settings"
  and "Not Now". After "Not Now" the alert isn't shown again until the
  next app version.
- **Local Network.** It is probed by sending one mDNS packet, because no
  API exists to query it. The probe reports `granted`, or `blocked` when
  the user denied it or hasn't answered yet.
- **Where the result goes.** It is logged to
  `~/Library/Logs/Patchbay/patchbay.log`, written to
  `~/Library/Application Support/fts/patchbay/permissions.json`, and
  served by the `permissions` RPC.

From the CLI:

- `patchbay permissions [--json]` shows the state.
- `patchbay permissions request` asks the running app to run the flow
  again. `open -a Patchbay --args --request-permissions` does the same at
  launch.

### Status

| | status |
|---|---|
| Device + process enumeration, stable ids, graph-direction mapping | **done** |
| Change notifications (device/process lists, defaults, per-object state) | **done** |
| `TapMonitor` (app → output device, channel map, gain, meters, mute option, reliable teardown, timeout) | **done, mechanically verified.** Audio content not yet verified because permission was not granted. |
| TCC preflight + silent-tap diagnosis | **done** (private SPI, best-effort) |
| Permission flow: tap-probe prompt, Microphone request, System Settings deep link, NSAlert, `permissions` RPC/CLI | **done**, in the signed `Patchbay.app` |
| `HostBackend::create_link` / `remove_link` | `Unsupported`, milestone 3 |
| `create_virtual_device` | validates the spec, refuses `PassThru` explicitly, otherwise `Unsupported`, milestone 3 |
| `set_volume` | `Unsupported`, milestone 3 |
| `HostCapabilities` | `app_capture: true` (via `TapMonitor`). All others false. |
| In the app | `CoreAudioBackend` backs the read-only **`system-audio`** device entry (devices, app streams, defaults as params; `patchbay device show system-audio`). See `devices.md` → System Audio. |

## Milestone 3: graph links and virtual devices on taps and aggregates

These need no driver.

- **App → device links.** Group links by `(source app, destination device)` into one `TapMonitor` whose `ChannelMap` is the union of the links, with per-link gains in an atomic gain matrix instead of the single gain. Changing a link rebuilds the route table with an RCU-style swap (`arc-swap` or a double buffer) instead of tearing down the monitor.
- **Device → device links.** Use one private aggregate with both devices as subdevices (the output device as main, drift compensation on the other), with the same Router.
- **`SystemAudio` source.** Use `initStereoGlobalTapButExcludeProcesses([])` and exclude patchbay's own process.
- **Virtual device without a driver.** Build a private aggregate per `VirtualDeviceSpec`. The monitors are the output subdevices, and the sources are taps plus input subdevices. A mix bus in the Router sums sources into `channels` and feeds the monitors. Other apps **cannot** select it, because private aggregates are invisible to other processes. That is what the HAL plug-in is for.
- `meters`: expose `TapMonitor::take_peaks` through a `HostEvent::Meters` or a separate polling API.
- `set_volume(Node)` means device main volume (`kAudioDevicePropertyVolumeScalar`, main element when settable). Only do it on explicit request.

## Milestone 4: the HAL AudioServerPlugIn — DONE (`crates/driver`)

Shipped as `Patchbay.driver`, forked from MARS `mars-hal` (MIT) with the
shared-memory transport replaced by an in-driver loopback ring and the
device list persisted in coreaudiod's plug-in storage. See
`crates/driver/README.md`. Three bugs cost a day of hardware debugging
and are each pinned by a test now:

- `kAudioServerPlugInIOOperationWriteMix` is **`'rite'`**, not `'wmix'`
  (the fork's value). Answering `WillDoIOOperation` about the wrong code
  means the HAL never asks the driver to write: the device enumerates,
  starts IO and polls the clock, but not one frame is written, and every
  read correctly returns silence.
- `AudioServerPlugInIOCycleInfo` is `{counter, nominal size,
  **current**, input, output}` — the current timestamp comes first.
- `StopIO` arrives per client: stopping the device (and re-anchoring the
  clock) when one of several clients stops moves the device's timeline
  backwards and wedges the HAL's IO engine.

The `'pbrs'` property exposes IO counters (writes, reads, silent reads,
`WillDo`/`Begin`/`AddClient`); they are what found all three.

### The original plan (kept for context)

Loopback's "pass-thru" source and devices that other apps can pick as their
output or input need a **HAL AudioServerPlugIn**. It is a bundle in
`/Library/Audio/Plug-Ins/HAL` that `coreaudiod` loads.

- **Keep the driver dumb.** It publishes N loopback devices, each with a name, UID, channel count and rate, as configured by patchbay. Each device is a ring buffer: whatever apps play into it comes out of its input side. All mixing, routing and monitoring stays in the patchbay process, which reads the device's input side (one more source kind) and writes monitor output. The driver holds no policy.
- **Configuration channel.** Use a custom property on the plug-in object (`kAudioObjectPropertyCustomPropertyInfoList` with a CFPropertyList payload), written by patchbay through `AudioObjectSetPropertyData`, and persist it in the plug-in's own settings storage (`WriteToStorage`) so devices survive reboots without patchbay running. Adding or removing devices raises `kAudioObjectPropertyOwnedObjects`/`DeviceList` changes, which the Milestone 1 listeners already pick up.
- **Implementation options:**
  - **tympan-aspl** (Rust AudioServerPlugIn framework). The preferred option if it is maintained enough: same language and the same lint regime, in a separate `crates/hal-driver` built as a `cdylib` into a `.driver` bundle. The FFI surface is large, so it needs its own unsafe-isolation module.
  - **libASPL** (C++, MIT). A mature framework; a thin C++ driver with the config property is small.
  - **BlackHole-derived C driver** (GPL-3.0). Its license is compatible with patchbay's GPL-3.0-or-later. It is simple and proven but fixed-shape, so it needs the config property added.
  - Swift isn't a realistic choice for the driver itself.
- **Real-time rules inside `coreaudiod`.** No allocation, locks or Objective-C in the IO path. Crashes take down every app's audio, so fuzz the property handlers.
- **Installation.** A signed and notarized `.pkg` needs admin rights to write to `/Library/Audio/Plug-Ins/HAL` and must restart `coreaudiod` (`launchctl kickstart -k system/com.apple.audio.coreaudiod`). That restart drops every app's audio for a moment, so do it only in the installer with user consent and **never from patchbay at runtime**. Uninstall removes the bundle and restarts again.
- **Signing and notarization.** A Developer ID Application certificate for the bundle, hardened runtime, `notarytool submit` for the `.pkg`, then staple. The plug-in is sandboxed by `coreaudiod` (`AudioServerPlugIn_MachServices` / `AudioServerPlugIn_Network` keys only as needed).
- With the driver present, `HostCapabilities::pass_thru_device` and `virtual_devices` report true, and virtual devices become `VirtualDevice` nodes other apps can see. Their links persist in the driver config, the same "system graph, not owned" semantics as PipeWire's lingering links.

## Migrating the PipeWire engine behind `HostBackend` (`patchbay-host-pipewire`)

The engine in `crates/patchbay` stays the reference behaviour. The migration
is mechanical and keeps the rules in `crates/CLAUDE.md`: one PipeWire thread,
identity by names, lingering links, no cleanup at exit.

1. **Adapter crate first.** Add `crates/host-pipewire` depending on `patchbay`. It implements `HostBackend` by translating the existing `GraphStore` mirror and `GraphEvent` stream:
   - Nodes: `media.class` gives the `NodeKind` (Audio/Sink or Source → `HardwareDevice`/`VirtualDevice` by `device.api` or `factory.name`; Stream/* → `AppStream` with `application.process.id` and `application.id`/`application.name` → `AppInfo`). The node id is `pipewire:<node.name>`, never the global id.
   - Ports are indexed per direction in `audio.channel`/port order.
   - Links map to `create_link` (with `object.linger`).
   - The existing `meters.rs` backs `HostCapabilities::meters`.
2. **Gain.** PipeWire links have no gain. `create_link` accepts `gain == 1.0` and otherwise returns `InvalidSpec` until a per-link volume exists (a `libpipewire-module-loopback` or filter-chain node per gained link).
3. **Virtual devices** are `support.null-audio-sink` / `module-loopback` instances created on the PipeWire thread with a stable `node.name` derived from the spec name. App sources are links from stream nodes, since PipeWire already exposes per-app streams, and monitors are links to sinks. `PassThru` is native: other apps just pick the null sink.
4. **Move the code.** Once the adapter is proven, move the engine thread, store and plan modules from `crates/patchbay` into `crates/host-pipewire`. `patchbay` keeps the service, presets, aliases, clock and Dante stack, and talks to a `Box<dyn DynHostBackend>` chosen by `cfg(target_os)`: `patchbay-host-pipewire` on Linux, `patchbay-host-coreaudio` on macOS.
5. `patchbay-proto` gains Facet mirrors of the `patchbay-host` types when the UI starts showing host graphs from both platforms. The host crate is serde-only for now, like `patchbay-device`.

## References

- Apple: `CATapDescription`, `AudioHardwareCreateProcessTap`, "Capturing system audio with Core Audio taps" (macOS 14.2+), `AudioHardware.h` aggregate and tap keys.
- `objc2-core-audio` 0.3.2 (generated from the SDK headers). This is the only binding source used.
- insidegui/AudioCap (Swift): the tap → private aggregate → IOProc recipe and the `TCCAccessPreflight` trick. The approach follows its design, but it was not fetched and no code was copied. Check its license before reusing anything from it.
- flexaudio-os-macos: not used.
