//! Preset + alias persistence — a styx document under the fts config
//! dir (`fts/patchbay/patchbay.styx`), the same config language the
//! rest of the FTS stack uses (signal rigs, keybinds, launcher). It's
//! hand-editable: channel names and colors are just `aliases`/`colors`
//! lists a human can bulk-edit in a text editor. A legacy
//! `patchbay.json` is auto-migrated to styx on first open.
//!
//! Presets are connection memory (RaySession's jackpatch idea): links
//! remembered by stable (node.name, port.name) pairs, re-applied
//! incrementally against whatever half of the graph currently exists.

use std::fs;
use std::path::PathBuf;

use facet::Facet;
use parking_lot::Mutex;
use patchbay_proto::{
    AliasEntry, CanvasView, ColorEntry, DanteDeviceConfig, NamedRoute, PresetLink, RoutingPreset,
    VirtualSink,
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
}

fn config_path() -> PathBuf {
    // Override for tests / scratch instances so smoke runs never touch
    // the real config.
    if let Ok(p) = std::env::var("PATCHBAY_CONFIG") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("fts/patchbay/patchbay.styx")
}

/// Load the config: styx if present, else migrate a legacy
/// `patchbay.json` sitting beside it, else `None` (fresh install).
fn load(styx_path: &std::path::Path) -> Option<FileFormat> {
    if let Ok(s) = fs::read_to_string(styx_path) {
        match facet_styx::from_str::<FileFormat>(&s) {
            Ok(data) => return Some(data),
            Err(e) => {
                // Don't clobber a file we can't parse — surface it and
                // fall through so the user can fix it by hand.
                tracing::error!("patchbay.styx parse failed ({e:?}); leaving it untouched");
                return Some(FileFormat::default());
            }
        }
    }
    // Legacy JSON migration: read once; the caller writes styx on open.
    let json_path = styx_path.with_extension("json");
    let s = fs::read_to_string(&json_path).ok()?;
    match serde_json::from_str::<FileFormat>(&s) {
        Ok(data) => {
            tracing::info!("migrating patchbay config {} → styx", json_path.display());
            // Keep the old file as a backup rather than deleting it.
            let _ = fs::rename(&json_path, json_path.with_extension("json.bak"));
            Some(data)
        }
        Err(e) => {
            tracing::warn!("legacy patchbay.json parse failed: {e}");
            None
        }
    }
}

impl PresetStore {
    pub fn open() -> Self {
        let path = config_path();
        let data = load(&path).unwrap_or_else(|| {
            // Fresh install: start from the REAPER baseline so the
            // main outs/click are named the first time it appears.
            FileFormat {
                aliases: seed_defaults(),
                ..FileFormat::default()
            }
        });
        let store = Self {
            path,
            data: Mutex::new(data),
        };
        // Write the styx file now if it doesn't exist yet — materializes
        // a freshly-migrated or seeded config so it's hand-editable.
        if !store.path.exists() {
            store.persist(&store.data.lock());
        }
        store
    }

    fn persist(&self, data: &FileFormat) {
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        match facet_styx::to_string(data) {
            Ok(styx) => {
                if let Err(e) = fs::write(&self.path, styx) {
                    tracing::warn!("patchbay config write failed: {e}");
                }
            }
            Err(e) => tracing::warn!("patchbay config serialize failed: {e:?}"),
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
        data.presets.retain(|p| p.name != preset.name);
        data.presets.push(preset.clone());
        data.presets.sort_by(|a, b| a.name.cmp(&b.name));
        self.persist(&data);
        preset
    }

    pub fn delete_preset(&self, name: &str) -> bool {
        let mut data = self.data.lock();
        let before = data.presets.len();
        data.presets.retain(|p| p.name != name);
        let removed = data.presets.len() != before;
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
        data.latency_rules.retain(|r| r.pattern != rule.pattern);
        data.latency_rules.push(rule);
        data.latency_rules.sort_by(|a, b| a.pattern.cmp(&b.pattern));
        self.persist(&data);
        data.latency_rules.clone()
    }

    pub fn remove_latency_rule(&self, pattern: &str) -> Option<Vec<patchbay_proto::LatencyRule>> {
        let mut data = self.data.lock();
        let before = data.latency_rules.len();
        data.latency_rules.retain(|r| r.pattern != pattern);
        if data.latency_rules.len() == before {
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
        data.virtual_sinks.retain(|s| s.name != sink.name);
        data.virtual_sinks.push(sink);
        data.virtual_sinks.sort_by(|a, b| a.name.cmp(&b.name));
        self.persist(&data);
    }

    pub fn remove_virtual_sink(&self, name: &str) -> bool {
        let mut data = self.data.lock();
        let before = data.virtual_sinks.len();
        data.virtual_sinks.retain(|s| s.name != name);
        let removed = data.virtual_sinks.len() != before;
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
        data.views.retain(|v| v.name != view.name);
        data.views.push(view);
        data.views.sort_by(|a, b| a.name.cmp(&b.name));
        self.persist(&data);
    }

    pub fn delete_view(&self, name: &str) -> bool {
        let mut data = self.data.lock();
        let before = data.views.len();
        data.views.retain(|v| v.name != name);
        let removed = data.views.len() != before;
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
        data.colors.retain(|c| c.target != target);
        if !color.is_empty() {
            data.colors.push(ColorEntry { target, color });
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
        data.routes.retain(|r| r.name != route.name);
        data.routes.push(route);
        data.routes.sort_by(|a, b| a.name.cmp(&b.name));
        self.persist(&data);
    }

    pub fn delete_route(&self, name: &str) -> bool {
        let mut data = self.data.lock();
        let before = data.routes.len();
        data.routes.retain(|r| r.name != name);
        let removed = data.routes.len() != before;
        if removed {
            self.persist(&data);
        }
        removed
    }

    /// Empty alias clears the entry.
    pub fn set_alias(&self, target: String, alias: String) {
        let mut data = self.data.lock();
        data.aliases.retain(|a| a.target != target);
        if !alias.is_empty() {
            data.aliases.push(AliasEntry { target, alias });
        }
        self.persist(&data);
    }
}

#[cfg(test)]
mod styx_roundtrip {
    use super::*;

    #[test]
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
        };

        let styx = facet_styx::to_string(&original).expect("serialize");
        let parsed: FileFormat = facet_styx::from_str(&styx).expect("parse");
        assert_eq!(original, parsed, "styx round-trip must be lossless\n{styx}");
    }

    #[test]
    fn missing_sections_default_to_empty() {
        // A hand-written file with only aliases parses fine.
        let styx = "aliases ({target \"REAPER:out3\", alias Click})\n";
        let parsed: FileFormat = facet_styx::from_str(styx).expect("parse partial");
        assert_eq!(parsed.aliases.len(), 1);
        assert!(parsed.presets.is_empty() && parsed.views.is_empty());
    }
}
