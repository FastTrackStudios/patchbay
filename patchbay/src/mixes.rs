//! Host audio mixes (Loopback / OBS-style): saved [`MixConfig`]s kept
//! running by a supervisor.
//!
//! macOS renders them with `patchbay-host-coreaudio`'s `Mix` (process
//! taps + a private aggregate, one real-time `IOProc`). Everywhere else
//! mixes stay stopped with an explanation — on Linux the `PipeWire`
//! graph itself is the router.
//!
//! The supervisor (every [`TICK`]) starts enabled mixes that aren't
//! running, retries failed ones, and **rebuilds a mix when the processes
//! behind its app sources change** — an app launched after the mix, or
//! quit and relaunched, is picked up without the user doing anything.
//! Gains and mutes change live and are saved.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use patchbay_host_coreaudio::survey::{DefaultRole, DeviceKind, Survey};
use patchbay_proto::{HostOverview, HostTargets, MixConfig, MixMeters, MixView, PatchbayError};

use crate::presets::PresetStore;

/// Supervisor period.
const TICK: Duration = Duration::from_secs(2);

/// `dB → linear`, clamped to the host's gain range.
fn db_to_gain(db: f64) -> f32 {
    let lin = 10_f64.powf(db / 20.0);
    // Clamp before narrowing: the host accepts 0..=MAX_GAIN.
    let lin = lin.clamp(0.0, f64::from(patchbay_host::MAX_GAIN));
    #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
    let g = lin as f32;
    g
}

#[cfg(target_os = "macos")]
mod platform {
    use std::fmt::Write as _;

    use patchbay_host::{
        AppSelector, ChannelMap, HostError, MonitorSpec, SourceKind, SourceSpec, TapMute,
        VirtualDeviceSpec,
    };
    use patchbay_host_coreaudio::survey::Survey;
    use patchbay_proto::{MixConfig, source_kind};

    pub(super) use patchbay_host_coreaudio::Mix as Running;

    pub(super) const SUPPORTED: bool = true;

    fn map(s: &str) -> Result<ChannelMap, HostError> {
        if s.trim().is_empty() {
            Ok(ChannelMap::identity(2))
        } else {
            ChannelMap::parse(s.trim())
        }
    }

    /// Config → host spec.
    pub(super) fn spec(cfg: &MixConfig) -> Result<VirtualDeviceSpec, HostError> {
        let sources = cfg
            .sources
            .iter()
            .map(|s| {
                let kind = match s.kind.as_str() {
                    source_kind::APP if !s.device.is_empty() => SourceKind::AppOnDevice {
                        app: AppSelector::BundleId(s.target.clone()),
                        device_uid: s.device.clone(),
                    },
                    source_kind::APP => SourceKind::App {
                        app: AppSelector::BundleId(s.target.clone()),
                    },
                    source_kind::INPUT => SourceKind::InputDevice {
                        uid: s.target.clone(),
                    },
                    source_kind::SYSTEM => SourceKind::SystemAudio,
                    other => {
                        return Err(HostError::InvalidSpec(format!(
                            "unknown source kind `{other}` (app, input, system)"
                        )));
                    }
                };
                Ok(SourceSpec {
                    kind,
                    // Muting only means anything for a tap, and it is
                    // held for as long as the tap exists rather than
                    // only while it is read — a mix that is running is
                    // taking the audio, whether or not anything is
                    // listening at this instant.
                    mute: if s.exclusive {
                        TapMute::Muted
                    } else {
                        TapMute::Unmuted
                    },
                    channel_map: map(&s.map)?,
                    volume: super::db_to_gain(s.gain_db),
                    enabled: !s.muted,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let monitors = cfg
            .outputs
            .iter()
            .map(|o| {
                Ok(MonitorSpec {
                    device_uid: o.device.clone(),
                    channel_map: map(&o.map)?,
                    volume: super::db_to_gain(o.gain_db),
                    enabled: !o.muted,
                })
            })
            .collect::<Result<Vec<_>, HostError>>()?;
        Ok(VirtualDeviceSpec {
            name: cfg.name.clone(),
            channels: cfg.channels,
            sources,
            monitors,
        })
    }

    pub(super) fn start(cfg: &MixConfig) -> Result<Running, String> {
        let spec = spec(cfg).map_err(|e| e.to_string())?;
        Running::start(&spec).map_err(|e| e.to_string())
    }

    /// What a mix's app sources currently resolve to (sorted pids per
    /// source) plus which input/output devices are present — when this
    /// changes, the mix is rebuilt.
    pub(super) fn signature(cfg: &MixConfig, host: &Survey) -> String {
        let present = |uid: &str| host.devices.iter().any(|d| d.uid == uid);
        let mut sig = String::new();
        for s in &cfg.sources {
            if s.kind == source_kind::APP {
                let mut pids: Vec<i32> = host
                    .processes
                    .iter()
                    .filter(|p| {
                        p.bundle_id
                            .as_deref()
                            .is_some_and(|b| patchbay_host::bundle_matches(&s.target, b))
                    })
                    .map(|p| p.pid)
                    .collect();
                pids.sort_unstable();
                if !s.device.is_empty() {
                    let _ = write!(sig, "{}={};", s.device, present(&s.device));
                }
                let _ = write!(
                    sig,
                    "{}={pids:?}{};",
                    s.target,
                    if s.exclusive { "!" } else { "" }
                );
            } else if s.kind == source_kind::INPUT {
                let _ = write!(sig, "{}={};", s.target, present(&s.target));
            }
        }
        for o in &cfg.outputs {
            let _ = write!(sig, "{}={};", o.device, present(&o.device));
        }
        sig
    }

    /// Live per-source / per-output state of a running mix.
    pub(super) fn status(
        r: &Running,
    ) -> (
        Vec<patchbay_proto::MixSourceStatus>,
        Vec<patchbay_proto::MixOutputStatus>,
    ) {
        let info = r.info();
        (
            info.sources
                .iter()
                .map(|s| patchbay_proto::MixSourceStatus {
                    label: s
                        .processes
                        .first()
                        .map_or_else(|| s.label.clone(), |p| p.name.clone()),
                    active: s.active,
                    reason: s.reason.clone().unwrap_or_default(),
                    channels: s.channels,
                })
                .collect(),
            info.monitors
                .iter()
                .map(|m| patchbay_proto::MixOutputStatus {
                    device_name: m.device.clone(),
                    clock: m.clock,
                })
                .collect(),
        )
    }

    pub(super) fn snapshot() -> Survey {
        patchbay_host_coreaudio::survey::survey()
    }

    pub(super) use patchbay_host_coreaudio::MAX_APPS;
    pub(super) use patchbay_host_coreaudio::Probe as Meters;

    /// Start metering `apps` (bundle id → its pids).
    pub(super) fn start_meters(apps: &[(String, Vec<i32>)]) -> Result<Meters, String> {
        let apps: Vec<patchbay_host_coreaudio::ProbeApp> = apps
            .iter()
            .map(|(key, pids)| patchbay_host_coreaudio::ProbeApp {
                key: key.clone(),
                pids: pids.clone(),
            })
            .collect();
        Meters::start(&apps).map_err(|e| e.to_string())
    }

    pub(super) fn meter_peaks(m: &Meters) -> Vec<patchbay_proto::AppMeter> {
        m.take_peaks()
            .into_iter()
            .map(|p| patchbay_proto::AppMeter {
                bundle_id: p.key,
                peak: p.peak,
            })
            .collect()
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use patchbay_host_coreaudio::survey::Survey;
    use patchbay_proto::MixConfig;

    /// Never constructed off macOS.
    pub(super) enum Running {}

    pub(super) fn status(
        r: &Running,
    ) -> (
        Vec<patchbay_proto::MixSourceStatus>,
        Vec<patchbay_proto::MixOutputStatus>,
    ) {
        match *r {}
    }

    impl Running {
        pub(super) fn set_source(&self, _: usize, _: f32, _: bool) -> Result<(), String> {
            match *self {}
        }
        pub(super) fn set_monitor(&self, _: usize, _: f32, _: bool) -> Result<(), String> {
            match *self {}
        }
        pub(super) fn take_source_peaks(&self) -> Vec<Vec<f32>> {
            match *self {}
        }
        pub(super) fn take_monitor_peaks(&self) -> Vec<Vec<f32>> {
            match *self {}
        }
    }

    pub(super) const SUPPORTED: bool = false;

    pub(super) fn start(_: &MixConfig) -> Result<Running, String> {
        Err("mixes need macOS (on Linux the PipeWire graph is the router)".to_owned())
    }

    pub(super) fn signature(_: &MixConfig, _: &Survey) -> String {
        String::new()
    }

    pub(super) fn snapshot() -> Survey {
        Survey::default()
    }

    /// Never constructed off macOS.
    pub(super) enum Meters {}

    pub(super) const MAX_APPS: usize = 0;

    pub(super) fn start_meters(_: &[(String, Vec<i32>)]) -> Result<Meters, String> {
        Err("app metering needs macOS".to_owned())
    }

    pub(super) fn meter_peaks(m: &Meters) -> Vec<patchbay_proto::AppMeter> {
        match *m {}
    }
}

/// One mix's runtime state.
#[derive(Default)]
struct Slot {
    running: Option<platform::Running>,
    error: String,
    /// `signature` the running mix was built for.
    built_for: String,
}

/// The app-metering probe and what it was built for.
///
/// Taps make macOS show Patchbay as recording, so the probe only exists
/// while something is actually reading meters: `app_meters` stamps
/// `last_poll`, and the supervisor drops the probe once nobody has asked
/// for [`METER_IDLE`].
#[derive(Default)]
struct ProbeSlot {
    running: Option<platform::Meters>,
    /// `meter_signature` it was built for.
    built_for: String,
    last_poll: Option<Instant>,
    /// When we last tried to build one (failed builds aren't retried in
    /// a tight loop).
    last_try: Option<Instant>,
}

/// How long the metering probe outlives the last `app_meters` call.
const METER_IDLE: Duration = Duration::from_secs(3);
/// Shortest gap between attempts to build the probe.
const METER_RETRY: Duration = Duration::from_secs(1);

/// Owns every running mix.
pub(crate) struct MixHub {
    presets: Arc<PresetStore>,
    slots: Arc<Mutex<BTreeMap<String, Slot>>>,
    /// Serializes (re)builds so a save and the supervisor never race.
    build: Arc<tokio::sync::Mutex<()>>,
    probe: Arc<Mutex<ProbeSlot>>,
}

impl MixHub {
    pub(crate) fn new(presets: Arc<PresetStore>) -> Self {
        Self {
            presets,
            slots: Arc::default(),
            build: Arc::default(),
            probe: Arc::default(),
        }
    }

    /// Start the supervisor (idempotent per hub; call once).
    pub(crate) fn start(&self) {
        if !platform::SUPPORTED {
            return;
        }
        let (presets, slots, build, probe) = (
            Arc::clone(&self.presets),
            Arc::clone(&self.slots),
            Arc::clone(&self.build),
            Arc::clone(&self.probe),
        );
        tokio::spawn(async move {
            let hub = Self {
                presets,
                slots,
                build,
                probe,
            };
            loop {
                supervise(&hub.presets, &hub.slots, &hub.build, false).await;
                // Follow what is playing, and let the taps go when
                // nobody is watching the meters.
                let watching = hub
                    .probe
                    .lock()
                    .last_poll
                    .is_some_and(|t| t.elapsed() < METER_IDLE);
                if watching || hub.probe.lock().running.is_some() {
                    let host = hub.host().await;
                    hub.rebuild_meters(&host).await;
                }
                tokio::time::sleep(TICK).await;
            }
        });
    }

    pub(crate) fn views(&self) -> Vec<MixView> {
        let slots = self.slots.lock();
        self.presets
            .mixes()
            .into_iter()
            .map(|config| {
                let slot = slots.get(&config.name);
                let running = slot.and_then(|s| s.running.as_ref());
                let (sources, outputs) =
                    running.map_or_else(|| (Vec::new(), Vec::new()), platform::status);
                MixView {
                    running: running.is_some(),
                    error: slot.map(|s| s.error.clone()).unwrap_or_default(),
                    sources,
                    outputs,
                    config,
                }
            })
            .collect()
    }

    fn view(&self, name: &str) -> Result<MixView, PatchbayError> {
        self.views()
            .into_iter()
            .find(|v| v.config.name == name)
            .ok_or_else(|| PatchbayError::not_found("mix", &name))
    }

    /// Save (create or replace) and rebuild now.
    pub(crate) async fn save(&self, mix: MixConfig) -> Result<MixView, PatchbayError> {
        let name = mix.name.trim().to_owned();
        if name.is_empty() {
            return Err(PatchbayError::Internal("mix name is empty".into()));
        }
        let mix = MixConfig {
            name: name.clone(),
            ..mix
        };
        let presets = Arc::clone(&self.presets);
        tokio::task::spawn_blocking(move || presets.save_mix(mix))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))?;
        // Force a rebuild of this mix with the new config.
        if let Some(slot) = self.slots.lock().get_mut(&name) {
            slot.built_for.clear();
            slot.running = None;
        }
        supervise(&self.presets, &self.slots, &self.build, true).await;
        self.view(&name)
    }

    pub(crate) async fn delete(&self, name: &str) -> Result<(), PatchbayError> {
        let presets = Arc::clone(&self.presets);
        let n = name.to_owned();
        let removed = tokio::task::spawn_blocking(move || presets.delete_mix(&n))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))?;
        let _stopped = self.slots.lock().remove(name);
        if removed {
            Ok(())
        } else {
            Err(PatchbayError::not_found("mix", &name))
        }
    }

    /// Live gain/mute for a source (`output = false`) or an output, saved.
    pub(crate) async fn set_level(
        &self,
        name: &str,
        index: u32,
        gain_db: f64,
        muted: bool,
        output: bool,
    ) -> Result<(), PatchbayError> {
        let mut cfg = self
            .presets
            .mixes()
            .into_iter()
            .find(|m| m.name == name)
            .ok_or_else(|| PatchbayError::not_found("mix", &name))?;
        let i = usize::try_from(index).map_err(|e| PatchbayError::Internal(e.to_string()))?;
        let what = if output { "mix output" } else { "mix source" };
        if output {
            let o = cfg
                .outputs
                .get_mut(i)
                .ok_or_else(|| PatchbayError::not_found(what, &index))?;
            o.gain_db = gain_db;
            o.muted = muted;
        } else {
            let s = cfg
                .sources
                .get_mut(i)
                .ok_or_else(|| PatchbayError::not_found(what, &index))?;
            s.gain_db = gain_db;
            s.muted = muted;
        }
        if let Some(r) = self.slots.lock().get(name).and_then(|s| s.running.as_ref()) {
            let res = if output {
                r.set_monitor(i, db_to_gain(gain_db), !muted)
            } else {
                r.set_source(i, db_to_gain(gain_db), !muted)
            };
            res.map_err(|e| PatchbayError::Internal(e.to_string()))?;
        }
        let presets = Arc::clone(&self.presets);
        tokio::task::spawn_blocking(move || presets.save_mix(cfg))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))
    }

    pub(crate) fn meters(&self) -> Vec<MixMeters> {
        self.slots
            .lock()
            .iter()
            .filter_map(|(name, slot)| {
                let r = slot.running.as_ref()?;
                Some(MixMeters {
                    name: name.clone(),
                    sources: r.take_source_peaks(),
                    outputs: r.take_monitor_peaks(),
                })
            })
            .collect()
    }

    pub(crate) async fn targets(&self) -> HostTargets {
        let host = self.host().await;
        HostTargets {
            supported: platform::SUPPORTED,
            apps: host_apps(&host),
            devices: host_devices(&host),
        }
    }

    /// One read of the host, off the async threads.
    async fn host(&self) -> Survey {
        tokio::task::spawn_blocking(platform::snapshot)
            .await
            .unwrap_or_default()
    }

    /// Everything the dashboard shows, in one pass.
    pub(crate) async fn overview(&self, extra: Vec<patchbay_proto::HostProblem>) -> HostOverview {
        let (host, vdevs, aggregates) =
            tokio::join!(self.host(), self.virtual_devices(), self.aggregates());
        let mixes = self.views();
        let mut problems = extra;
        if !vdevs.driver_loaded {
            problems.push(patchbay_proto::HostProblem {
                severity: "warn".to_owned(),
                summary: "Patchbay.driver isn't loaded".to_owned(),
                detail: "Without it there are no Patchbay virtual devices, so apps have \
                         nothing to play into and mixes have nothing to be heard through."
                    .to_owned(),
                fix: "install_driver".to_owned(),
            });
        }
        for m in &mixes {
            if !m.error.is_empty() {
                problems.push(patchbay_proto::HostProblem {
                    severity: "error".to_owned(),
                    summary: format!("mix `{}` isn't running", m.config.name),
                    detail: m.error.clone(),
                    fix: "mix_failed".to_owned(),
                });
            }
        }
        // Playing first, then recording, then by name: the dashboard
        // shows what is making sound without the user hunting for it.
        let mut apps = host_apps(&host);
        apps.sort_by(|a, b| {
            b.playing
                .cmp(&a.playing)
                .then_with(|| b.recording.cmp(&a.recording))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        HostOverview {
            supported: platform::SUPPORTED,
            apps,
            devices: host_devices(&host),
            virtual_devices: vdevs,
            aggregates,
            mixes,
            problems,
        }
    }

    /// Peaks for every app the probe covers, and keep it alive.
    ///
    /// Building the probe costs a tap per app, so it is created on the
    /// first call and dropped once nobody has called for
    /// [`METER_IDLE`] — the meters exist while someone is watching them.
    pub(crate) async fn app_meters(&self) -> Vec<patchbay_proto::AppMeter> {
        let needs_build = {
            let mut p = self.probe.lock();
            p.last_poll = Some(Instant::now());
            p.running.is_none() && p.last_try.is_none_or(|t| t.elapsed() >= METER_RETRY)
        };
        if needs_build {
            let host = self.host().await;
            self.rebuild_meters(&host).await;
        }
        let p = self.probe.lock();
        p.running
            .as_ref()
            .map(platform::meter_peaks)
            .unwrap_or_default()
    }

    /// (Re)build the probe for what is playing now, if anyone is watching.
    async fn rebuild_meters(&self, host: &Survey) {
        let watching = self
            .probe
            .lock()
            .last_poll
            .is_some_and(|t| t.elapsed() < METER_IDLE);
        if !watching {
            // Nobody is looking: drop the taps (and the recording
            // indicator that comes with them).
            let mut p = self.probe.lock();
            p.running = None;
            p.built_for.clear();
            return;
        }
        let wanted = meter_apps(host);
        let sig = meter_signature(&wanted);
        {
            let mut p = self.probe.lock();
            if p.running.is_some() && p.built_for == sig {
                return;
            }
            // Drop the old taps before making new ones.
            p.running = None;
            p.last_try = Some(Instant::now());
        }
        if wanted.is_empty() {
            self.probe.lock().built_for = sig;
            return;
        }
        let built = tokio::task::spawn_blocking(move || platform::start_meters(&wanted))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        let mut p = self.probe.lock();
        p.built_for = sig;
        match built {
            Ok(m) => p.running = Some(m),
            Err(e) => tracing::debug!("app meters unavailable: {e}"),
        }
    }
}

/// Core Audio processes folded into the apps a person would name.
///
/// A browser is a dozen processes and a helper is not a separate app, so
/// everything collapses onto its parent bundle id — which is also what a
/// mix source addresses, so what is listed here can be routed.
fn host_apps(host: &Survey) -> Vec<patchbay_proto::HostApp> {
    let mut by_bundle: BTreeMap<String, patchbay_proto::HostApp> = BTreeMap::new();
    for p in &host.processes {
        let Some(bundle) = p.bundle_id.as_deref() else {
            continue;
        };
        // Helpers (`….helper…`) collapse into their app.
        let parent = patchbay_host::parent_bundle(bundle).to_owned();
        let is_parent = parent == bundle;
        let name = if is_parent {
            p.name.clone()
        } else {
            p.name
                .find(" Helper")
                .and_then(|i| p.name.get(..i))
                .unwrap_or(&p.name)
                .to_owned()
        };
        let e = by_bundle
            .entry(parent.clone())
            .or_insert_with(|| patchbay_proto::HostApp {
                bundle_id: parent,
                name: name.clone(),
                playing: false,
                recording: false,
                pids: Vec::new(),
                output_devices: Vec::new(),
                input_devices: Vec::new(),
            });
        if is_parent {
            e.name = name;
        }
        e.playing |= p.running_output;
        e.recording |= p.running_input;
        e.pids.push(p.pid);
        for uid in &p.output_devices {
            if !e.output_devices.contains(uid) {
                e.output_devices.push(uid.clone());
            }
        }
        for uid in &p.input_devices {
            if !e.input_devices.contains(uid) {
                e.input_devices.push(uid.clone());
            }
        }
    }
    for app in by_bundle.values_mut() {
        app.pids.sort_unstable();
    }
    by_bundle.into_values().collect()
}

fn host_devices(host: &Survey) -> Vec<patchbay_proto::HostDevice> {
    host.devices
        .iter()
        .map(|d| patchbay_proto::HostDevice {
            uid: d.uid.clone(),
            name: d.name.clone(),
            input_channels: d.input_channels,
            output_channels: d.output_channels,
            kind: match d.kind {
                DeviceKind::Hardware => "hardware",
                DeviceKind::Virtual => "virtual",
                DeviceKind::Aggregate => "aggregate",
            }
            .to_owned(),
            transport: d.transport.clone(),
            sample_rate: d.sample_rate,
            default_role: match d.default_role {
                DefaultRole::None => "none",
                DefaultRole::Output => "output",
                DefaultRole::Input => "input",
                DefaultRole::Both => "both",
            }
            .to_owned(),
            in_use: d.in_use,
        })
        .collect()
}

/// Which apps are worth a meter tap: the ones actually playing, capped.
///
/// Recording-only apps are left out — tapping an app reads what it
/// *plays*, so a meter for one would always read silence.
fn meter_apps(host: &Survey) -> Vec<(String, Vec<i32>)> {
    let mut apps: Vec<(String, Vec<i32>)> = host_apps(host)
        .into_iter()
        .filter(|a| a.playing && !a.pids.is_empty())
        .map(|a| (a.bundle_id, a.pids))
        .collect();
    apps.sort_by(|a, b| a.0.cmp(&b.0));
    apps.truncate(platform::MAX_APPS);
    apps
}

/// What the probe was built for: the apps and their pids. A launch, a
/// quit or a helper coming and going changes it, and the probe is
/// rebuilt so its taps still point at real processes.
fn meter_signature(apps: &[(String, Vec<i32>)]) -> String {
    let mut sig = String::new();
    for (key, pids) in apps {
        let _ = write!(sig, "{key}={pids:?};");
    }
    sig
}

/// One supervisor pass: start what should run, rebuild what changed,
/// stop what was disabled or deleted. `force` retries failed mixes now.
async fn supervise(
    presets: &PresetStore,
    slots: &Arc<Mutex<BTreeMap<String, Slot>>>,
    build: &tokio::sync::Mutex<()>,
    force: bool,
) {
    let _one_at_a_time = build.lock().await;
    let configs = presets.mixes();
    let Ok(host) = tokio::task::spawn_blocking(platform::snapshot).await else {
        return;
    };
    {
        let mut s = slots.lock();
        s.retain(|name, _| configs.iter().any(|c| &c.name == name && c.is_enabled()));
    }
    for cfg in configs.into_iter().filter(MixConfig::is_enabled) {
        let sig = platform::signature(&cfg, &host);
        let needs_build = {
            let s = slots.lock();
            match s.get(&cfg.name) {
                None => true,
                Some(slot) if slot.running.is_none() => force || slot.built_for != sig,
                Some(slot) => slot.built_for != sig,
            }
        };
        if !needs_build {
            continue;
        }
        // Stop the old one first: its taps/aggregate go away before the
        // new ones are made.
        if let Some(slot) = slots.lock().get_mut(&cfg.name) {
            slot.running = None;
        }
        let c = cfg.clone();
        let result = tokio::task::spawn_blocking(move || platform::start(&c))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        let mut s = slots.lock();
        let slot = s.entry(cfg.name.clone()).or_default();
        slot.built_for = sig;
        match result {
            Ok(r) => {
                tracing::info!(mix = %cfg.name, "mix running");
                slot.running = Some(r);
                slot.error.clear();
            }
            Err(e) => {
                if slot.error != e {
                    tracing::warn!(mix = %cfg.name, error = %e, "mix failed to start");
                }
                slot.error = e;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::db_to_gain;

    #[test]
    fn db_conversion() {
        assert!((db_to_gain(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_gain(-6.0) - 0.501).abs() < 1e-3);
        assert!(db_to_gain(-200.0) < 1e-9);
    }
}

// ── Virtual devices (Patchbay.driver) ─────────────────────────────────

#[cfg(target_os = "macos")]
mod virtual_devices {
    use patchbay_host_coreaudio::virtual_devices as vd;
    use patchbay_proto::{PatchbayError, VirtualDeviceView, VirtualDevicesStatus};

    fn err(e: &patchbay_host::HostError) -> PatchbayError {
        match e {
            patchbay_host::HostError::NotFound(m) => PatchbayError::device("not_found", m.clone()),
            other => PatchbayError::device("virtual_device", other.to_string()),
        }
    }

    fn view(d: vd::VirtualDevice) -> VirtualDeviceView {
        VirtualDeviceView {
            uid: d.uid,
            name: d.name,
            channels: u32::from(d.channels),
            hidden: d.hidden,
        }
    }

    pub(super) fn status() -> VirtualDevicesStatus {
        let loaded = vd::driver_loaded();
        VirtualDevicesStatus {
            driver_loaded: loaded,
            devices: if loaded {
                vd::list()
                    .unwrap_or_default()
                    .into_iter()
                    .map(view)
                    .collect()
            } else {
                Vec::new()
            },
        }
    }

    /// uid, or a unique display name (case-insensitive).
    fn resolve(device: &str) -> Result<String, PatchbayError> {
        let list = vd::list().map_err(|e| err(&e))?;
        if list.iter().any(|d| d.uid == device) {
            return Ok(device.to_owned());
        }
        let q = device.to_lowercase();
        let hits: Vec<&vd::VirtualDevice> =
            list.iter().filter(|d| d.name.to_lowercase() == q).collect();
        match hits.as_slice() {
            [one] => Ok(one.uid.clone()),
            [] => Err(PatchbayError::not_found("virtual device", &device)),
            _ => Err(PatchbayError::device(
                "ambiguous_device",
                format!("several virtual devices are named '{device}'; use the uid"),
            )),
        }
    }

    pub(super) fn create(name: &str, channels: u32) -> Result<VirtualDeviceView, PatchbayError> {
        let ch = u16::try_from(channels)
            .map_err(|_| PatchbayError::device("invalid", "channels must be 1–64"))?;
        vd::create(name.trim(), ch).map(view).map_err(|e| err(&e))
    }

    pub(super) fn rename(device: &str, name: &str) -> Result<(), PatchbayError> {
        vd::rename(&resolve(device)?, name.trim()).map_err(|e| err(&e))
    }

    pub(super) fn remove(device: &str) -> Result<(), PatchbayError> {
        vd::remove(&resolve(device)?).map_err(|e| err(&e))
    }

    fn agg(a: vd::AggregateInfo) -> patchbay_proto::AggregateView {
        patchbay_proto::AggregateView {
            uid: a.uid,
            name: a.name,
            input_channels: a.input_channels,
            output_channels: a.output_channels,
        }
    }

    pub(super) fn aggregates() -> Vec<patchbay_proto::AggregateView> {
        vd::aggregates().into_iter().map(agg).collect()
    }

    pub(super) fn create_aggregate(
        name: &str,
        devices: &[String],
    ) -> Result<patchbay_proto::AggregateView, PatchbayError> {
        vd::create_aggregate(name.trim(), devices)
            .map(agg)
            .map_err(|e| err(&e))
    }

    pub(super) fn remove_aggregate(uid: &str) -> Result<(), PatchbayError> {
        vd::remove_aggregate(uid).map_err(|e| err(&e))
    }
}

#[cfg(not(target_os = "macos"))]
mod virtual_devices {
    use patchbay_proto::{PatchbayError, VirtualDeviceView, VirtualDevicesStatus};

    fn unsupported() -> PatchbayError {
        PatchbayError::device(
            "unsupported",
            "virtual devices here come from PipeWire virtual sinks (see `patchbay sink`)",
        )
    }

    pub(super) fn status() -> VirtualDevicesStatus {
        VirtualDevicesStatus::default()
    }

    pub(super) fn create(_: &str, _: u32) -> Result<VirtualDeviceView, PatchbayError> {
        Err(unsupported())
    }

    pub(super) fn rename(_: &str, _: &str) -> Result<(), PatchbayError> {
        Err(unsupported())
    }

    pub(super) fn remove(_: &str) -> Result<(), PatchbayError> {
        Err(unsupported())
    }

    pub(super) fn aggregates() -> Vec<patchbay_proto::AggregateView> {
        Vec::new()
    }

    pub(super) fn create_aggregate(
        _: &str,
        _: &[String],
    ) -> Result<patchbay_proto::AggregateView, PatchbayError> {
        Err(unsupported())
    }

    pub(super) fn remove_aggregate(_: &str) -> Result<(), PatchbayError> {
        Err(unsupported())
    }
}

impl MixHub {
    pub(crate) async fn virtual_devices(&self) -> patchbay_proto::VirtualDevicesStatus {
        tokio::task::spawn_blocking(virtual_devices::status)
            .await
            .unwrap_or_default()
    }

    pub(crate) async fn create_virtual_device(
        &self,
        name: String,
        channels: u32,
    ) -> Result<patchbay_proto::VirtualDeviceView, PatchbayError> {
        tokio::task::spawn_blocking(move || virtual_devices::create(&name, channels))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))?
    }

    pub(crate) async fn rename_virtual_device(
        &self,
        device: String,
        name: String,
    ) -> Result<(), PatchbayError> {
        tokio::task::spawn_blocking(move || virtual_devices::rename(&device, &name))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))?
    }

    pub(crate) async fn aggregates(&self) -> Vec<patchbay_proto::AggregateView> {
        tokio::task::spawn_blocking(virtual_devices::aggregates)
            .await
            .unwrap_or_default()
    }

    pub(crate) async fn create_aggregate(
        &self,
        name: String,
        devices: Vec<String>,
    ) -> Result<patchbay_proto::AggregateView, PatchbayError> {
        tokio::task::spawn_blocking(move || virtual_devices::create_aggregate(&name, &devices))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))?
    }

    pub(crate) async fn remove_aggregate(&self, uid: String) -> Result<(), PatchbayError> {
        tokio::task::spawn_blocking(move || virtual_devices::remove_aggregate(&uid))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))?
    }

    pub(crate) async fn remove_virtual_device(&self, device: String) -> Result<(), PatchbayError> {
        tokio::task::spawn_blocking(move || virtual_devices::remove(&device))
            .await
            .map_err(|e| PatchbayError::Internal(e.to_string()))?
    }
}
