//! Preset + alias persistence — a styx document under the fts config
//! dir (`fts/patchbay/patchbay.styx`), the same config language the
//! rest of the FTS stack uses (signal rigs, keybinds, launcher). It's
//! hand-editable: channel names and colors are just `aliases`/`colors`
//! lists a human can bulk-edit in a text editor. A legacy
//! `patchbay.json` is auto-migrated to styx on first open.
//!
//! Presets are connection memory (`RaySession`'s jackpatch idea): links
//! remembered by stable (`node.name`, `port.name`) pairs, re-applied
//! incrementally against whatever half of the graph currently exists.

use std::fs;
use std::path::PathBuf;

use facet::Facet;
use parking_lot::Mutex;
use patchbay_proto::{
    AliasEntry, CanvasView, ColorEntry, DanteDeviceConfig, DeviceConfig, DeviceSettingSnapshot,
    NamedRoute, PresetLink, RoutingPreset, VirtualSink,
};

/// The whole patchbay config, one styx document. Every list defaults to
/// empty (`#[facet(default)]`) so a hand-written file can omit any
/// section, and `#[serde(default)]` keeps the legacy-JSON migration
/// reader lenient.
#[derive(Debug, Default, Facet, PartialEq, serde::Serialize, serde::Deserialize)]
struct FileFormat {
    #[serde(default)]
    #[facet(default)]
    presets: Vec<RoutingPreset>,
    #[serde(default)]
    #[facet(default)]
    aliases: Vec<AliasEntry>,
    #[serde(default)]
    #[facet(default)]
    latency_rules: Vec<patchbay_proto::LatencyRule>,
    #[serde(default)]
    #[facet(default)]
    colors: Vec<ColorEntry>,
    #[serde(default)]
    #[facet(default)]
    virtual_sinks: Vec<VirtualSink>,
    #[serde(default)]
    #[facet(default)]
    views: Vec<CanvasView>,
    #[serde(default)]
    #[facet(default)]
    routes: Vec<NamedRoute>,
    #[serde(default)]
    #[facet(default)]
    dante_devices: Vec<DanteDeviceConfig>,
    /// External devices to connect (hardware adapters). Empty = the
    /// built-in default set (system audio, Galaxy32, Yamaha TF, Dante —
    /// all auto-discovered, see `devices::registry::default_devices`).
    #[serde(default)]
    #[facet(default)]
    devices: Vec<DeviceConfig>,
    /// Named device snapshots (params + crosspoints by stable path).
    #[serde(default)]
    #[facet(default)]
    device_snapshots: Vec<DeviceSettingSnapshot>,
}

/// First-run channel names for a stock REAPER JACK client: the main
/// stereo pair + click, the studio's baseline output map. Pure aliases
/// (identity is names), so they apply whenever REAPER shows up and the
/// user extends/overwrites them like any other alias.
fn seed_defaults() -> Vec<AliasEntry> {
    [
        ("REAPER:out1", "Main Output ST L"),
        ("REAPER:out2", "Main Output ST R"),
        ("REAPER:out3", "Click"),
    ]
    .into_iter()
    .map(|(target, alias)| AliasEntry {
        target: target.into(),
        alias: alias.into(),
    })
    .collect()
}

pub(crate) struct PresetStore {
    path: PathBuf,
    data: Mutex<FileFormat>,
    /// Set when the on-disk config exists but does not parse. Every
    /// write becomes a no-op so a user's hand-edited file is never
    /// overwritten with the empty config we booted with.
    degraded: bool,
}

pub(crate) fn config_path() -> PathBuf {
    // Override for tests / scratch instances so smoke runs never touch
    // the real config.
    if let Ok(p) = std::env::var("PATCHBAY_CONFIG") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("fts/patchbay/patchbay.styx")
}

/// Outcome of reading the config off disk.
enum Loaded {
    /// Parsed cleanly (or migrated from legacy JSON).
    Config(FileFormat),
    /// The file exists but does not parse. We keep an empty in-memory
    /// config so the app still runs, but persisting is disabled — see
    /// [`PresetStore::degraded`].
    Unreadable,
    /// Nothing on disk yet — fresh install.
    Fresh,
}

/// Load the config: styx if present, else migrate a legacy
/// `patchbay.json` sitting beside it, else [`Loaded::Fresh`].
fn load(styx_path: &std::path::Path) -> Loaded {
    if let Ok(s) = fs::read_to_string(styx_path) {
        return match facet_styx::from_str::<FileFormat>(&s) {
            Ok(data) => Loaded::Config(data),
            Err(e) => {
                // Never clobber a file we can't parse: the store goes
                // read-only so a later mutation can't overwrite the
                // user's hand-edited presets with an empty document.
                tracing::error!(
                    path = %styx_path.display(),
                    "patchbay.styx parse failed ({e:?}); config is READ-ONLY until fixed"
                );
                Loaded::Unreadable
            }
        };
    }
    // Legacy JSON migration: read once; the caller writes styx on open.
    let json_path = styx_path.with_extension("json");
    let Ok(s) = fs::read_to_string(&json_path) else {
        return Loaded::Fresh;
    };
    match serde_json::from_str::<FileFormat>(&s) {
        Ok(data) => {
            tracing::info!("migrating patchbay config {} → styx", json_path.display());
            // Keep the old file as a backup rather than deleting it.
            drop(fs::rename(&json_path, json_path.with_extension("json.bak")));
            Loaded::Config(data)
        }
        Err(e) => {
            tracing::warn!("legacy patchbay.json parse failed: {e}");
            Loaded::Fresh
        }
    }
}

/// Upsert `item` into `list` keyed by `key`, keeping the list sorted by
/// that key. Every config section is an upsert-by-name list, so this is
/// the one place that logic lives.
fn upsert_by<T, K, F>(list: &mut Vec<T>, key: F, item: T)
where
    F: Fn(&T) -> K,
    K: Ord,
{
    let k = key(&item);
    list.retain(|existing| key(existing) != k);
    list.push(item);
    list.sort_by(|a, b| key(a).cmp(&key(b)));
}

/// Remove every element of `list` matching `pred`; returns whether any
/// were removed.
fn remove_where<T, F: Fn(&T) -> bool>(list: &mut Vec<T>, pred: F) -> bool {
    let before = list.len();
    list.retain(|item| !pred(item));
    list.len() != before
}

impl PresetStore {
    pub fn open() -> Self {
        let path = config_path();
        let (data, degraded) = match load(&path) {
            Loaded::Config(data) => (data, false),
            Loaded::Unreadable => (FileFormat::default(), true),
            // Fresh install: start from the REAPER baseline so the
            // main outs/click are named the first time it appears.
            Loaded::Fresh => (
                FileFormat {
                    aliases: seed_defaults(),
                    ..FileFormat::default()
                },
                false,
            ),
        };
        let store = Self {
            path,
            data: Mutex::new(data),
            degraded,
        };
        // Write the styx file now if it doesn't exist yet — materializes
        // a freshly-migrated or seeded config so it's hand-editable.
        if !store.degraded && !store.path.exists() {
            store.persist(&store.data.lock());
        }
        store
    }

    /// Serialize `data` over the config file, atomically: write a
    /// sibling temp file then rename it into place, so an interrupted
    /// write can never leave a truncated (and therefore unparseable —
    /// see [`Loaded::Unreadable`]) config behind.
    fn persist(&self, data: &FileFormat) {
        if self.degraded {
            tracing::warn!(
                path = %self.path.display(),
                "config change NOT saved: the on-disk config is unparseable; fix or remove it"
            );
            return;
        }
        if let Some(dir) = self.path.parent() {
            drop(fs::create_dir_all(dir));
        }
        let styx = match facet_styx::to_string(data) {
            Ok(styx) => styx,
            Err(e) => {
                tracing::warn!("patchbay config serialize failed: {e:?}");
                return;
            }
        };
        let tmp = self.path.with_extension("styx.tmp");
        if let Err(e) = fs::write(&tmp, styx) {
            tracing::warn!("patchbay config write failed: {e}");
            return;
        }
        if let Err(e) = fs::rename(&tmp, &self.path) {
            tracing::warn!("patchbay config rename failed: {e}");
            drop(fs::remove_file(&tmp));
        }
    }

    pub fn presets(&self) -> Vec<RoutingPreset> {
        self.data.lock().presets.clone()
    }

    pub fn preset(&self, name: &str) -> Option<RoutingPreset> {
        self.data
            .lock()
            .presets
            .iter()
            .find(|p| p.name == name)
            .cloned()
    }

    pub fn upsert_preset(
        &self,
        name: String,
        description: String,
        links: Vec<PresetLink>,
    ) -> RoutingPreset {
        let preset = RoutingPreset {
            name,
            description,
            links,
        };
        let mut data = self.data.lock();
        upsert_by(&mut data.presets, |p| p.name.clone(), preset.clone());
        self.persist(&data);
        preset
    }

    pub fn delete_preset(&self, name: &str) -> bool {
        let mut data = self.data.lock();
        let removed = remove_where(&mut data.presets, |p| p.name == name);
        if removed {
            self.persist(&data);
        }
        removed
    }

    pub fn aliases(&self) -> Vec<AliasEntry> {
        self.data.lock().aliases.clone()
    }

    pub fn latency_rules(&self) -> Vec<patchbay_proto::LatencyRule> {
        self.data.lock().latency_rules.clone()
    }

    pub fn set_latency_rule(
        &self,
        rule: patchbay_proto::LatencyRule,
    ) -> Vec<patchbay_proto::LatencyRule> {
        let mut data = self.data.lock();
        upsert_by(&mut data.latency_rules, |r| r.pattern.clone(), rule);
        self.persist(&data);
        data.latency_rules.clone()
    }

    pub fn remove_latency_rule(&self, pattern: &str) -> Option<Vec<patchbay_proto::LatencyRule>> {
        let mut data = self.data.lock();
        if !remove_where(&mut data.latency_rules, |r| r.pattern == pattern) {
            return None;
        }
        self.persist(&data);
        Some(data.latency_rules.clone())
    }

    pub fn virtual_sinks(&self) -> Vec<VirtualSink> {
        self.data.lock().virtual_sinks.clone()
    }

    pub fn add_virtual_sink(&self, sink: VirtualSink) {
        let mut data = self.data.lock();
        upsert_by(&mut data.virtual_sinks, |s| s.name.clone(), sink);
        self.persist(&data);
    }

    pub fn remove_virtual_sink(&self, name: &str) -> bool {
        let mut data = self.data.lock();
        let removed = remove_where(&mut data.virtual_sinks, |s| s.name == name);
        if removed {
            self.persist(&data);
        }
        removed
    }

    /// Does this alias target already have a value? (Used by the
    /// non-destructive auto chanmap import.)
    pub fn has_alias(&self, target: &str) -> bool {
        self.data.lock().aliases.iter().any(|a| a.target == target)
    }

    pub fn views(&self) -> Vec<CanvasView> {
        self.data.lock().views.clone()
    }

    pub fn save_view(&self, view: CanvasView) {
        let mut data = self.data.lock();
        upsert_by(&mut data.views, |v| v.name.clone(), view);
        self.persist(&data);
    }

    pub fn delete_view(&self, name: &str) -> bool {
        let mut data = self.data.lock();
        let removed = remove_where(&mut data.views, |v| v.name == name);
        if removed {
            self.persist(&data);
        }
        removed
    }

    pub fn colors(&self) -> Vec<ColorEntry> {
        self.data.lock().colors.clone()
    }

    /// Empty color clears the entry.
    pub fn set_color(&self, target: String, color: String) {
        let mut data = self.data.lock();
        if color.is_empty() {
            remove_where(&mut data.colors, |c| c.target == target);
        } else {
            upsert_by(
                &mut data.colors,
                |c| c.target.clone(),
                ColorEntry { target, color },
            );
        }
        self.persist(&data);
    }

    pub fn routes(&self) -> Vec<NamedRoute> {
        self.data.lock().routes.clone()
    }

    pub fn dante_config(&self) -> Vec<DanteDeviceConfig> {
        self.data.lock().dante_devices.clone()
    }

    /// Replace the whole saved Dante snapshot.
    pub fn set_dante_config(&self, devices: Vec<DanteDeviceConfig>) {
        let mut data = self.data.lock();
        data.dante_devices = devices;
        self.persist(&data);
    }

    /// Upsert a named route (by `name`).
    pub fn set_route(&self, route: NamedRoute) {
        let mut data = self.data.lock();
        upsert_by(&mut data.routes, |r| r.name.clone(), route);
        self.persist(&data);
    }

    pub fn delete_route(&self, name: &str) -> bool {
        let mut data = self.data.lock();
        let removed = remove_where(&mut data.routes, |r| r.name == name);
        if removed {
            self.persist(&data);
        }
        removed
    }

    pub fn device_configs(&self) -> Vec<DeviceConfig> {
        self.data.lock().devices.clone()
    }

    pub fn device_snapshots(&self) -> Vec<DeviceSettingSnapshot> {
        self.data.lock().device_snapshots.clone()
    }

    pub fn device_snapshot(&self, name: &str) -> Option<DeviceSettingSnapshot> {
        self.data
            .lock()
            .device_snapshots
            .iter()
            .find(|s| s.name == name)
            .cloned()
    }

    /// Upsert a device snapshot (by `name`).
    pub fn save_device_snapshot(&self, snapshot: DeviceSettingSnapshot) {
        let mut data = self.data.lock();
        upsert_by(&mut data.device_snapshots, |s| s.name.clone(), snapshot);
        self.persist(&data);
    }

    pub fn delete_device_snapshot(&self, name: &str) -> bool {
        let mut data = self.data.lock();
        let removed = remove_where(&mut data.device_snapshots, |s| s.name == name);
        if removed {
            self.persist(&data);
        }
        removed
    }

    /// Empty alias clears the entry.
    pub fn set_alias(&self, target: String, alias: String) {
        self.set_aliases(std::iter::once((target, alias)));
    }

    /// Bulk alias upsert — ONE persist for the whole batch. A chanmap
    /// or Dante-name import touches every channel on a 128-port node;
    /// doing that one `set_alias` at a time rewrote the entire config
    /// file 128 times.
    pub fn set_aliases<I: IntoIterator<Item = (String, String)>>(&self, entries: I) {
        let mut data = self.data.lock();
        let mut touched = false;
        for (target, alias) in entries {
            touched = true;
            if alias.is_empty() {
                remove_where(&mut data.aliases, |a| a.target == target);
            } else {
                upsert_by(
                    &mut data.aliases,
                    |a| a.target.clone(),
                    AliasEntry { target, alias },
                );
            }
        }
        if touched {
            self.persist(&data);
        }
    }
}

#[cfg(test)]
mod persistence {
    use super::*;

    fn tmpdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "patchbay-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store_at(path: PathBuf) -> PresetStore {
        let (data, degraded) = match load(&path) {
            Loaded::Config(d) => (d, false),
            Loaded::Unreadable => (FileFormat::default(), true),
            Loaded::Fresh => (FileFormat::default(), false),
        };
        PresetStore {
            path,
            data: Mutex::new(data),
            degraded,
        }
    }

    /// The regression this guards: a config that fails to parse used to
    /// boot as an EMPTY in-memory config, and the next mutation (the
    /// automatic chanmap import fires ~2s after REAPER appears) wrote
    /// that empty config straight over the user's file.
    #[test]
    fn unparseable_config_is_never_overwritten() {
        let path = tmpdir().join("corrupt.styx");
        let garbage = "presets ({{{ this is not styx";
        std::fs::write(&path, garbage).unwrap();

        let store = store_at(path.clone());
        assert!(store.degraded, "a bad parse must mark the store degraded");

        // Any mutation must be a no-op on disk.
        store.set_alias("REAPER:out1".into(), "Main L".into());
        store.set_route(NamedRoute {
            name: "r".into(),
            from: patchbay_proto::RouteEndpoint::default(),
            to: patchbay_proto::RouteEndpoint::default(),
            enabled: true,
        });
        store.set_color("REAPER".into(), "#fff".into());

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            garbage,
            "the user's unparseable config must survive byte-for-byte"
        );
    }

    #[test]
    fn writes_are_atomic_and_leave_no_temp_file() {
        let path = tmpdir().join("atomic.styx");
        let store = store_at(path.clone());
        store.set_alias("REAPER:out1".into(), "Main L".into());

        assert!(path.exists());
        assert!(
            !path.with_extension("styx.tmp").exists(),
            "the temp file must be renamed away, not left behind"
        );
        let reread = store_at(path);
        assert_eq!(reread.aliases().len(), 1);
    }

    #[test]
    fn bulk_alias_import_persists_once_and_round_trips() {
        let path = tmpdir().join("bulk.styx");
        let store = store_at(path.clone());
        let entries: Vec<(String, String)> = (1..=128)
            .map(|n| (format!("REAPER:in{n}"), format!("Channel {n}")))
            .collect();
        store.set_aliases(entries);

        let reread = store_at(path);
        let aliases = reread.aliases();
        assert_eq!(aliases.len(), 128);
        assert!(
            aliases
                .iter()
                .any(|a| a.target == "REAPER:in42" && a.alias == "Channel 42")
        );
    }

    #[test]
    fn empty_alias_clears_the_entry() {
        let path = tmpdir().join("clear.styx");
        let store = store_at(path);
        store.set_alias("REAPER:out1".into(), "Main L".into());
        assert!(store.has_alias("REAPER:out1"));
        store.set_alias("REAPER:out1".into(), String::new());
        assert!(!store.has_alias("REAPER:out1"));
    }

    #[test]
    fn upsert_replaces_rather_than_duplicates() {
        let path = tmpdir().join("upsert.styx");
        let store = store_at(path);
        store.set_alias("REAPER:out1".into(), "First".into());
        store.set_alias("REAPER:out1".into(), "Second".into());
        let aliases = store.aliases();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].alias, "Second");
    }
}

#[cfg(test)]
mod styx_roundtrip {
    use super::*;

    #[test]
    #[allow(clippy::too_many_lines)]
    fn full_config_survives_styx_roundtrip() {
        let original = FileFormat {
            presets: vec![RoutingPreset {
                name: "FOH".into(),
                description: "front of house".into(),
                links: vec![PresetLink {
                    output_node: "REAPER".into(),
                    output_port: "out1".into(),
                    input_node: "Inferno sink".into(),
                    input_port: "playback_1".into(),
                }],
            }],
            aliases: vec![AliasEntry {
                target: "REAPER:out1".into(),
                alias: "Main Output ST L".into(),
            }],
            latency_rules: vec![patchbay_proto::LatencyRule {
                pattern: "REAPER".into(),
                quantum: 64,
                force: true,
            }],
            colors: vec![ColorEntry {
                target: "REAPER:in25".into(),
                color: "#4a90d9".into(),
            }],
            virtual_sinks: vec![VirtualSink {
                name: "Stems".into(),
                channels: 8,
                capturable: true,
            }],
            views: vec![CanvasView {
                name: "Broadcast".into(),
                zoom: 1.25,
                pan_x: -340.5,
                pan_y: 12.0,
                collapsed_cols: vec![false, true, false, true],
                hide_unconnected: true,
                hide_monitors: false,
            }],
            routes: vec![NamedRoute {
                name: "Engineer TB → REAPER".into(),
                from: patchbay_proto::RouteEndpoint {
                    node: "Inferno source".into(),
                    port: "Engineer TB [DSP]".into(),
                },
                to: patchbay_proto::RouteEndpoint {
                    node: "REAPER".into(),
                    port: "Engineer TB".into(),
                },
                enabled: true,
            }],
            dante_devices: vec![DanteDeviceConfig {
                name: "Galaxy32".into(),
                tx: vec![patchbay_proto::DanteChannel {
                    number: 1,
                    name: "Engineer Talkback DSP".into(),
                }],
                rx: vec![patchbay_proto::DanteChannel {
                    number: 5,
                    name: "Monitor L".into(),
                }],
                subscriptions: vec![patchbay_proto::DanteSubscription {
                    rx_channel: 5,
                    tx_channel: "Main L".into(),
                    tx_device: "Console".into(),
                    status: 1,
                }],
            }],
            devices: vec![
                DeviceConfig {
                    name: "galaxy32".into(),
                    kind: "antelope-galaxy32".into(),
                    serial: "4202524000109".into(),
                    addr: String::new(),
                    disabled: false,
                    enabled: None,
                },
                DeviceConfig {
                    name: "tf1".into(),
                    kind: "yamaha-tf".into(),
                    serial: String::new(),
                    addr: "192.168.1.214:49280".into(),
                    disabled: true,
                    enabled: None,
                },
                DeviceConfig {
                    enabled: Some(false),
                    ..DeviceConfig::new("dante", "dante")
                },
            ],
            device_snapshots: vec![DeviceSettingSnapshot {
                name: "sunday".into(),
                device: "antelope:galaxy32:4202524000109".into(),
                created: 1_789_756_133,
                include: vec!["mixer/1/strip/16".into(), "route/DIGI_OUT0".into()],
                exclude: vec!["clock".into()],
                params: vec![
                    patchbay_proto::DeviceParamSetting::new(
                        "mixer/1/strip/16/level",
                        &patchbay_proto::DeviceParamValue::Level(-12.5),
                    ),
                    patchbay_proto::DeviceParamSetting::new(
                        "mixer/1/strip/16/mute",
                        &patchbay_proto::DeviceParamValue::Toggle(true),
                    ),
                    patchbay_proto::DeviceParamSetting::new(
                        "mixer/1/strip/16/pan",
                        &patchbay_proto::DeviceParamValue::Pan(-0.5),
                    ),
                    patchbay_proto::DeviceParamSetting::new(
                        "trim/line_in/control",
                        &patchbay_proto::DeviceParamValue::Enum(1),
                    ),
                    patchbay_proto::DeviceParamSetting::new(
                        "mixer/1/reverb/room_size",
                        &patchbay_proto::DeviceParamValue::Int(200),
                    ),
                    patchbay_proto::DeviceParamSetting::new(
                        "names/in/1",
                        &patchbay_proto::DeviceParamValue::Text("Kick <in>".into()),
                    ),
                ],
                routes: vec![
                    patchbay_proto::DeviceRouteSetting {
                        path: "route/DIGI_OUT0/1".into(),
                        source: "COM_PLAY1:1".into(),
                    },
                    patchbay_proto::DeviceRouteSetting {
                        path: "route/DIGI_OUT0/2".into(),
                        source: String::new(),
                    },
                ],
            }],
        };

        let styx = facet_styx::to_string(&original).expect("serialize");
        let parsed: FileFormat =
            facet_styx::from_str(&styx).unwrap_or_else(|e| panic!("parse: {e:?}\n{styx}"));
        assert_eq!(original, parsed, "styx round-trip must be lossless\n{styx}");
    }

    #[test]
    fn missing_sections_default_to_empty() {
        // A hand-written file with only aliases parses fine.
        let styx = "aliases ({target \"REAPER:out3\", alias Click})\n";
        let parsed: FileFormat = facet_styx::from_str(styx).expect("parse partial");
        assert_eq!(parsed.aliases.len(), 1);
        assert!(parsed.presets.is_empty() && parsed.views.is_empty());
        assert!(parsed.devices.is_empty() && parsed.device_snapshots.is_empty());
    }

    #[test]
    fn infinite_levels_survive_styx() {
        let snap = DeviceSettingSnapshot {
            name: "inf".into(),
            device: "yamaha:tf1:foh".into(),
            created: 0,
            include: Vec::new(),
            exclude: Vec::new(),
            params: vec![patchbay_proto::DeviceParamSetting::new(
                "in/1/level",
                &patchbay_proto::DeviceParamValue::Level(f64::NEG_INFINITY),
            )],
            routes: Vec::new(),
        };
        let original = FileFormat {
            device_snapshots: vec![snap],
            ..FileFormat::default()
        };
        let styx = facet_styx::to_string(&original).expect("serialize");
        let parsed: FileFormat =
            facet_styx::from_str(&styx).unwrap_or_else(|e| panic!("parse: {e:?}\n{styx}"));
        assert_eq!(
            parsed.device_snapshots[0].params[0].value(),
            Some(patchbay_proto::DeviceParamValue::Level(f64::NEG_INFINITY)),
            "{styx}"
        );
    }

    #[test]
    fn hand_written_devices_section_parses_with_defaults() {
        let styx = "devices ({name galaxy32, kind antelope-galaxy32})\n";
        let parsed: FileFormat = facet_styx::from_str(styx).expect("parse devices");
        assert_eq!(
            parsed.devices,
            vec![DeviceConfig {
                name: "galaxy32".into(),
                kind: "antelope-galaxy32".into(),
                serial: String::new(),
                addr: String::new(),
                disabled: false,
                enabled: None,
            }]
        );
        // The README's example, verbatim.
        let styx = "devices ({name system-audio, kind system-audio}\n         {name galaxy32, kind antelope-galaxy32}\n         {name tf1, kind yamaha-tf, addr \"192.168.1.214\"}\n         {name dante, kind dante, enabled false})\n";
        let parsed: FileFormat = facet_styx::from_str(styx).expect("parse readme example");
        assert_eq!(parsed.devices.len(), 4);
        assert_eq!(parsed.devices[2].addr, "192.168.1.214");
        assert!(!parsed.devices[3].is_enabled());
        let styx = "devices ({name tf1, kind yamaha-tf, enabled false})\n";
        let parsed: FileFormat = facet_styx::from_str(styx).expect("parse enabled");
        assert_eq!(parsed.devices[0].enabled, Some(false));
        assert!(!parsed.devices[0].is_enabled());
    }

    #[test]
    fn device_snapshot_upsert_and_delete_persist() {
        let path = std::env::temp_dir().join(format!(
            "patchbay-devsnap-{}-{:?}.styx",
            std::process::id(),
            std::thread::current().id()
        ));
        drop(std::fs::remove_file(&path));
        let store = PresetStore {
            path: path.clone(),
            data: Mutex::new(FileFormat::default()),
            degraded: false,
        };
        let snap = |n: &str, level: f64| DeviceSettingSnapshot {
            name: n.into(),
            device: "antelope:galaxy32:1".into(),
            created: 0,
            include: Vec::new(),
            exclude: Vec::new(),
            params: vec![patchbay_proto::DeviceParamSetting::new(
                "mixer/1/strip/16/level",
                &patchbay_proto::DeviceParamValue::Level(level),
            )],
            routes: Vec::new(),
        };
        store.save_device_snapshot(snap("a", -1.0));
        store.save_device_snapshot(snap("a", -2.0));
        store.save_device_snapshot(snap("b", -3.0));
        let text = std::fs::read_to_string(&path).expect("persisted");
        let reread: FileFormat = facet_styx::from_str(&text).expect("reparse");
        assert_eq!(reread.device_snapshots.len(), 2, "upsert by name\n{text}");
        assert_eq!(reread.device_snapshots[0], snap("a", -2.0));
        assert!(store.delete_device_snapshot("a"));
        assert!(!store.delete_device_snapshot("a"));
        assert_eq!(store.device_snapshots().len(), 1);
    }
}
