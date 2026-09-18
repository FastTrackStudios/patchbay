# Devices: the adapter layer

patchbay routes PipeWire today. The device layer extends it to the hardware
around it — interfaces, mixers, network audio — so one app can route and
control everything. It has two parts:

```
crates/device              patchbay-device    generic model + DeviceAdapter trait (no vendor code)
crates/adapters/antelope   patchbay-antelope  Antelope Galaxy32 via Antelope Manager Server
crates/adapters/yamaha     patchbay-yamaha    Yamaha TF-series over RCP (+ subnet discovery)
crates/adapters/dante      patchbay-dante     the Dante network over inferno-net (mDNS + ARC)
crates/adapters/<family>   patchbay-<family>  one crate per hardware family
```

Adapters depend on `patchbay-device`. `patchbay-device` never depends on an
adapter, and `patchbay-proto` depends on neither: the engine converts the
model into Facet wire types (see [In the app](#in-the-app-hub-rpc-cli-ui)).

## The generic model (`patchbay-device`)

| Concept | Type | Notes |
|---|---|---|
| Identity | `DeviceId`, `DeviceInfo` | `vendor:model:serial`, e.g. `antelope:galaxy32:4202524000109`. Build it from the hardware serial, never from an address. Presets persist it. |
| Router | `PortGroup` (inputs, outputs), `ChannelRef{group, channel}` (0-based), `Crosspoint{output, source: Option<ChannelRef>}` | **Router semantics.** Each output channel has at most one source. Antelope routing pages, Dante subscriptions and Yamaha input patching all work this way. A device without a router, such as the TF1 over RCP, has empty groups. |
| Parameters | `Param{path, label, kind, value, writable, disruptive}`, `ParamKind`, `ParamValue` | Slash paths such as `mixer/1/strip/16/level` (1-based, like the panel). Kinds are `Level{min_db,max_db}`, `Pan` (-1..1), `Toggle`, `Enum{options}`, `Int{min,max}` and `Text`. Channel names and colours are `Text` params. |
| Snapshot | `DeviceSnapshot{info, inputs, outputs, routes, params}` | Read from the device. Has lookup helpers. |
| Events | `DeviceEvent` | `ParamChanged`, `RouteChanged`, `Online`, `Offline`, `SnapshotReplaced` (re-read everything). |
| Behaviour | `DeviceAdapter` | `info`, `snapshot`, `apply_param(path, value, WriteGuard)` / `set_param`, `set_route`, `subscribe`. Uses native `async fn` in traits with `Send` futures. `DynDeviceAdapter` is the object-safe, boxed-future twin (blanket impl) for `Vec<Box<dyn DynDeviceAdapter>>`. |
| Errors | `DeviceError` | Clonable string variants: `Offline`, `Timeout`, `UnknownParam`, `UnknownPort`, `InvalidValue`, `ReadOnly`, `DisruptiveWrite`, `Unsupported`, `Protocol`, `Transport`. |

### Contract every adapter keeps

1. **The device is the source of truth.** Reads come from the device. After a
   write, the adapter reads the value back, or waits for it in the device's
   periodic state, before it reports success. Events carry the confirmed
   value. Hardware panels and other clients write too, and some vendor panels
   cache their state and later overwrite yours, so never trust a local cache
   over a read.
2. **Router semantics.** `set_route(output, source)` replaces the one source
   of that output channel. `None` means silence.
3. **Disruptive writes need an opt-in.** Params that drop audio carry
   `disruptive: true`, for example clock source and sample rate. The adapter
   refuses them with `DisruptiveWrite` unless the caller passes
   `WriteGuard::AllowDisruptive`.
4. **Stable identity.** `DeviceId` and group ids must survive reconnects and
   port changes. Prefer the vendor's own ids.
5. **No panics, strict lints.** Follow the workspace clippy profile: no
   unwrap, no indexing, checked arithmetic, no `as`. Add `[lints] workspace = true`.

## Antelope Galaxy32 (`patchbay-antelope`)

The adapter talks to Antelope's Manager Server, the vendor daemon, over JSON
on TCP. Everything below was verified on a Galaxy32 with fw 8.24 and Manager
Server 1.8.19. For the full protocol, see the `antelope-galaxy32-linux` repo
(`docs/PROTOCOL.md` and `notes/2026-09-18_live-session-1.md`).

```
discovery.rs   UDP multicast 239.192.5.8:5008 announces → dynamic control ports
protocol/      framing (u32 BE length incl. header) · ServerFrame (cyclic|single|notification) · Call ["m",[args],{kw}]
client.rs      Client: handshake (replays captured initialize_format), reader task,
               fire-and-forget send, (ext2,ext3)-correlated request, broadcast of cyclic/notifications
galaxy32/
  tables.rs    routing pages (outputs), source types (inputs), clock option tables, ext2 ids
  ops.rs       typed wire structs + pure call builders + Galaxy32 handle (get_/set_ ops, raw_call)
  afx.rs       AfxCatalog: effect types/fields parsed from the embedded server schema
  params.rs    path grammar, units (dB attenuation, pan 2..62, trims dBu), notification → events
  adapter.rs   Galaxy32Adapter: DeviceAdapter impl (RMW under a lock, read-back, event pump)
```

How the Galaxy32 maps to the generic model:

- **Router.** There are 19 output groups, one per routing page. `LINE_OUT0`,
  `MONITOR0`, `COM_REC0` / `COM_REC1` (DAW IN) and `DIGI_OUT0` / `DIGI_OUT1`
  (HDX) are examples. There are 18 input groups, one per source type:
  `LINE_IN0`, `COM_PLAY0`, `DANTE_IN0`, `MIXER_OUT0`, and so on. Type 18
  means no source. Group ids are the panel's own session-file ids.
  `set_routing` replaces a whole page, so `set_route` reads the page, changes
  one slot, writes the page and reads it back.
- **Params.** `mixer/{1-4}/{strip/{1-32}|master}/{level,pan,mute,solo,send}`,
  `mixer/{m}/reverb/*`, `monitor/{volume,mute,dim,mono}`,
  `trim/line_in/{control,1-32}`, `clock/{sync_source,sample_rate}` (enum,
  disruptive), `clock/{measured_rate,locked}` (read-only),
  `afx/strip/{s}/slot/{k}/effect` (enum index = effect type id) and
  `afx/strip/{s}/slot/{k}/{field}`.
- **AFX.** Inserting an effect allocates the lowest instance id that no strip
  uses, which is how the panel does it. Effect configs are positional
  `set_<name>_conf [type_id, inst_id, fields…]` calls taken from the schema.
  The server has no per-instance read-back yet, so field values are known only
  after a full `Galaxy32Adapter::set_afx_conf`, or after another client's
  write arrives as a notification.
- **Escape hatch.** Use `adapter.device().raw_call()` / `raw_request()` for
  methods that aren't modelled yet.

Quirks worth knowing:

- A device can announce more than one control endpoint. On 1.8.19 the
  loopback-only one streams only AFX meters and answers every `get_*` with a
  header-less `{"type":"single","contents":"","COMMAND_STATUS":"FAIL"}`.
  `discover_and_connect` probes the candidates best-first and keeps the first
  one that answers a read.
- Writes get no reply. A client does not receive notifications of its own
  writes, so confirmation must come from a read-back.
- The official panel doesn't refresh when other clients write. It keeps its
  cached state and resends a whole stale page or strip on the next edit.

Smoke tests:

```bash
cargo run -p patchbay-antelope --example galaxy_read         # read-only
cargo run -p patchbay-antelope --example galaxy_write_test   # guarded round-trips on cleared targets only
```

## Yamaha TF (`patchbay-yamaha`)

`TfAdapter` talks RCP (text lines on TCP 49280) straight to the console.
Verified read-only on a TF1 (V4.55).

- **Params only.** Paths are `in/{ch}/…`, `stin/…`, `fxrtn/…`, `aux/…`,
  `matrix/…`, `stereo/…`, `sub/…`, `dca/…`, `mutegroup/…` and `scene/…`, e.g.
  `in/1/level`, `in/1/on`, `in/1/name`, `in/1/send/aux/7/level`. A TF1 has
  about 4400. Levels go down to `-inf` (fully down), which the CLI prints and
  accepts as `-inf` and JSON carries as the string `"-inf"`.
- **No router.** Over RCP the TF has no input, output or Dante patch, so the
  adapter has no port groups and `set_route` returns `Unsupported`.
- **Scenes.** `scene/current`, `scene/title` and `scene/modified` are read-only.
  `scene/recall` (write `A05` / `B22`) is **disruptive** and needs
  `--allow-disruptive`. After any recall the adapter re-reads everything and
  emits `SnapshotReplaced`.
- **Identity.** The TF reports no serial, so the hub passes
  `TfOptions::device_id` from the config entry: `yamaha:tf1:<serial or
  name>` (default entry `tf1` → `yamaha:tf1:tf1`). The id stays stable when
  the console's IP changes.
- **Discovery** (`discover_consoles`, `scan_targets`, `probe_console`). RCP
  has no announce, so without `addr` the hub probes TCP 49280 on every host
  of the machine's private IPv4 networks with a `/24` or longer prefix
  (interfaces `lo*`, `utun*`, `awdl*`, `llw*`, `bridge*`, `gif*`, `stf*`,
  `anpi*` skipped; own addresses, network and broadcast excluded; at most
  1024 hosts, 256 probes in flight, 600 ms connect timeout). An open port
  is confirmed with `devinfo productname` starting with `TF` — the only
  line the probe sends. The address found is cached in
  `<config stem>.state.json` beside the config (`{"discovered": {"tf1":
  "192.168.1.214:49280"}}`); the next start probes that address first and
  only rescans if it doesn't answer. After a drop the supervisor retries
  (cache first, then a rescan) with backoff.
- **Session.** The hub connects with `ClientOptions::keepalive = None`, so
  patchbay never sends `scpmode`; liveness comes from the read-only
  `devstatus runmode` ping.
- Protocol research: `docs/yamaha-tf-rcp.md`.

## Dante (`patchbay-dante`)

The whole Dante network is **one** `DeviceAdapter` (`DanteNetworkAdapter`,
id `dante:network:<config name>`) — the Dante Controller matrix:

- **Router.** Every device with RX channels is an output group, every device
  with TX channels an input group; group id = the Dante device name (what
  subscriptions reference on the wire). Channel `n` of a group is Dante
  channel `n + 1`. A subscription is a crosspoint
  `rx-device:ch ← tx-device:ch`, e.g. `Apollo-x16D:10 ← Galaxy32:51`. The
  source is matched by TX channel **name**, as the subscription stores it.
  A subscription to a device that isn't on the network (e.g. an offline
  laptop) has no crosspoint source but stays visible in
  `<rx-device>/rx/<n>/subscription`.
- **Params** per device `<d>`: `<d>/name` and `<d>/{tx,rx}/<n>/name`
  (writable through inferno-net's `set_device_name` / `set_channel_name`;
  channel renames only up to channel 255 — ARC carries one byte),
  `<d>/model`, `<d>/address`, `<d>/reachable`, `<d>/sample_rate`,
  `<d>/latency_us`, `<d>/rx/<n>/subscription` (`channel@device`),
  `<d>/rx/<n>/status` — all read-only. Gain isn't readable over ARC, so it
  isn't exposed; sample rate and latency are read-only for now (a
  sample-rate change would be disruptive).
- **Writes** (`set_route` → `add_subscription` / `remove_subscription`,
  renames) are validated (Dante name rules), then confirmed by polling an
  ARC read-back — ARC acknowledges before the device applies. They are
  tested only against the in-memory fake (`DanteControl` is a trait; the
  real one is `InfernoControl`).
- **Discovery.** Connect is an 8 s mDNS browse (hardware answers lazily);
  no device → *not found*. A background task re-browses every 30 s; a
  device is dropped after 2 consecutive missed browses; any change emits
  `SnapshotReplaced`; an empty network emits `Offline` (the hub then goes
  back to discovering). `snapshot()` always reads every device over ARC.
- **Why one adapter, not one per box.** A subscription's source is another
  box's TX channel, and crosspoints are per adapter. One network adapter
  keeps every subscription a plain crosspoint and fits the hub's
  one-supervisor-per-config-entry model, so no dynamic "device provider"
  concept was needed. The Galaxy32's own Dante card shows up here as
  device `Galaxy32`, separate from the `galaxy32` Antelope adapter (linking
  the two views by name is future work).
- **Shared with the grid.** `scan()` and `InfernoControl` also back the
  existing `dante_network` / `dante_subscribe` / `dante_unsubscribe` RPCs
  (`crates/patchbay/src/dante_net.rs` only converts to the proto types).
- Everything is pure Rust (inferno-net); it builds and runs on macOS.

## System Audio (`system-audio`)

The machine's own audio system appears as a **read-only** device
(`crates/patchbay/src/devices/system_audio.rs`): id
`host:coreaudio:<name>` on macOS (`patchbay-host-coreaudio`'s
`CoreAudioBackend`), `host:pipewire:<name>` elsewhere (the existing
PipeWire engine's graph mirror, polled; engine behaviour unchanged).

- **Params** (all read-only): `backend`, `summary/{devices,apps,playing,
  recording}`, `default/{output,input}`,
  `device/<uid>/{name,kind,inputs,outputs,sample_rate,transport,
  manufacturer,running}`, `app/<pid>/{name,bundle_id,pid,playing,
  recording}` (PipeWire keys are `node.name`).
- **No router.** No port groups; `set_route` → `Unsupported`, writes →
  `ReadOnly`. Host events (hot-plug, apps starting to play) become
  `SnapshotReplaced` (debounced 250 ms).
- **Why an adapter and not a `host` RPC section.** The requirement today is
  presence and inspection — `device list`, `device show/params --json`, the
  Devices tab, events, failure isolation — and the adapter path gives all of
  it with no new wire surface. The host graph (mixing, many-to-many links)
  doesn't fit router semantics, so routing isn't mapped at all; when host
  links land (`docs/host-backends.md` milestone 3) they get their own RPC
  section over `patchbay-host` types and this entry stays the summary.

## In the app: hub, RPC, CLI, UI

Humans (desktop app, browser remote) and agents (`patchbay device … --json`)
use the same surface: `PatchbayService` in `patchbay-proto`.

```
crates/proto/src/devices.rs             wire types: DeviceSummary, DeviceView, ParamView,
                                        DeviceParamValue/Kind, DeviceEventWire, DeviceConfig,
                                        DeviceSettingSnapshot, DeviceRestoreReport
crates/patchbay/src/devices/registry.rs config `kind` → connect fn (one entry per family), default set
crates/patchbay/src/devices/cache.rs    last-found address of auto-discovered devices (<config>.state.json)
crates/patchbay/src/devices/system_audio.rs  the host audio system as a read-only device
crates/patchbay/src/devices/hub.rs      DeviceHub: supervision, events, calls, snapshots
crates/patchbay/src/devices/wire.rs     patchbay-device ↔ proto conversion
crates/patchbay/src/plan/devices.rs     pure snapshot capture + restore planner
app/src/cli_device.rs                   `patchbay device …`
crates/ui/src/devices.rs                the Devices tab
```

- **Defaults.** With no `devices` section the hub starts four entries,
  all auto-discovered: `system-audio` (kind `system-audio`), `galaxy32`
  (`antelope-galaxy32`), `tf1` (`yamaha-tf`) and `dante` (`dante`)
  (`registry::default_devices`).
- **Config.** A styx `devices` section replaces the defaults:
  `devices ({name galaxy32, kind antelope-galaxy32, serial "4202524000109"}
  {name foh, kind yamaha-tf, addr "192.168.1.214"} {name dante, kind dante,
  enabled false})`. `serial` and `addr` are optional and pin one unit or
  endpoint. `enabled false` (or the older `disabled true`) keeps an entry
  without connecting. `PATCHBAY_DEVICES=off` turns the device layer off.
- **States** (`DeviceLinkState`): `searching` (auto-discovery running,
  nothing found yet), `connecting` (pinned `addr`, first attempt),
  `online`, `not found` (discovery found nothing; retried in the
  background), `offline` (was reached before, or a pinned/known endpoint
  refuses; retried), `disabled`. Connect functions return
  `ConnectError::NotFound` vs `::Failed` to tell *not found* from
  *offline*.
- **Supervision.** Each entry gets a task that connects, forwards events to
  the `device_events` stream and reconnects with backoff (2 s doubling to
  60 s) after a drop or a failed discovery. A device being offline or not
  found only fails calls to that device — never the others, never
  startup. The device layer doesn't depend on the PipeWire engine, so it
  runs on macOS too (`patchbay serve` there, no window needed).
- **Addressing.** Every `id` argument accepts the full id, the config name,
  or a unique case-insensitive substring of id, model or serial. Router
  channels are 0-based on the wire and 1-based for humans
  (`DIGI_OUT0:17`).
- **RPC.** `list_devices`, `device`, `device_params(prefix)`,
  `set_device_param(…, allow_disruptive)`, `set_device_route`, the
  `#[subscribe] device_events` stream, and `save_/list_/delete_/diff_/
  restore_device_snapshot`. These are methods on `PatchbayService` rather
  than a sibling service. Each architect service is its own vox client, and
  `establish` negotiates one service per link, so a sibling service would
  need an extra connection and client in every shell (desktop, web, CLI).
  Device failures come back as `PatchbayError::Device { code, message }`,
  where `code` is stable for agents (`offline`, `disruptive_write`,
  `read_only`, `unknown_param`, `ambiguous_device`, …).
- **Writes** return the device's read-back: the param after
  `set_device_param`, the crosspoint after `set_device_route`.
- **Snapshots** live in the config (`device_snapshots`). Each one stores
  the writable params and all crosspoints by stable path, filtered by
  include/exclude prefixes. Prefixes match whole segments, so
  `mixer/1/strip/1` does not match strip 16. Route paths are
  `route/<OUTPUT_GROUP>/<n>`, so `--include route/DIGI_OUT0` works. A
  restore reads the device, plans with `plan::devices` and writes only
  what differs. It skips read-only and missing targets, and skips
  disruptive params unless `allow_disruptive` is set. Disruptive writes go
  first (a scene recall or clock change lands before the fine params) and
  every item gets a status. `diff` is the same plan with nothing written.

## Adding an adapter

1. Create `crates/adapters/<family>` (package `patchbay-<family>`) with
   `[lints] workspace = true`. Register it in the root `members` and
   `[workspace.dependencies]`, then add one `AdapterKind` entry (`kind`,
   `discovers`, and its `connect(DeviceConfig, ConnectCtx)` function
   returning `ConnectError::NotFound` / `::Failed`) to
   `crates/patchbay/src/devices/registry.rs`. The hub,
   RPC, CLI, UI and snapshots then work without further changes.
2. Keep modules private and expose a small `pub use` surface from `lib.rs`:
   the adapter type, discovery, a typed ops handle and a raw escape hatch.
3. Split the code into layers the same way: **protocol** (pure, unit-tested
   against captured bytes) → **client** (one connection, reader task,
   request correlation, broadcast of unsolicited frames) → **device ops**
   (typed, 1:1 with the wire) → **adapter** (the generic mapping plus the
   contract above).
4. Capture real traffic into `tests/fixtures/` and test encoders byte-exact
   against what the vendor's own software sends. Test the adapter against an
   in-process fake server. `crates/adapters/antelope/tests/fake_server.rs` is
   the template.
5. Live testing: read-only first. Write only to targets the operator has
   cleared, and always restore them: read, write, verify, restore, verify.

