//! Typed Galaxy32 operations: wire structs, pure call builders, and the
//! [`Galaxy32`] handle that sends them.
//!
//! Every `set_*` here is **fire-and-forget** and **whole-object**: a
//! mixer write carries the whole strip, a routing write the whole page, a
//! trim write all 32 channels. Single-field changes are read-modify-write
//! (the adapter does that under a lock and re-reads to confirm).
//!
//! The official Antelope panel does NOT refresh from other clients'
//! writes and later re-sends its stale cached page/strip, overwriting
//! ours. Treat the device (`get_*` + notifications) as the truth.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::tables::{
    self, AFX_SLOTS, AFX_STRIPS, MIXER_STRIPS, MIXERS, MONITOR_OUTPUT, ROUTING_PAGES,
    ROUTING_SLOTS, ext2,
};
use crate::client::Client;
use crate::error::{AntelopeError, Result};
use crate::protocol::call::Call;

/// One mixer strip (or master) as `get_mixer` returns it and `set_mixer` takes it.
///
/// Raw wire units:
/// `level`/`send` = dB of attenuation (0 = unity, 96 = -inf),
/// `pan` = 2 (L) … 32 (C) … 62 (R), `mute`/`solo` = 0/1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MixerStrip {
    /// Fader, dB of attenuation.
    pub level: u8,
    /// Pan, 2..=62.
    pub pan: u8,
    /// 1 = muted.
    pub mute: u8,
    /// 1 = soloed.
    pub solo: u8,
    /// Reverb send, dB of attenuation (96 = -inf).
    pub send: u8,
}

/// One routing slot: source `[type, ch]` (`ch` 0-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteSlot {
    /// Source type (see [`tables::SOURCE_TYPES`]; 18 = none).
    #[serde(rename = "in_periph_id")]
    pub ty: u8,
    /// 0-based channel within the source group.
    #[serde(rename = "in_chann")]
    pub ch: u8,
}

impl RouteSlot {
    /// The "no source" slot.
    pub const NONE: Self = Self {
        ty: tables::SOURCE_NONE,
        ch: 0,
    };
}

/// One routing page (first [`ROUTING_SLOTS`] slots).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingPage {
    /// Page index.
    pub page: u8,
    /// Exactly [`ROUTING_SLOTS`] slots.
    pub slots: Vec<RouteSlot>,
}

/// One trim level: dB of attenuation below 22.0 dBu, `whole` + `fract`
/// (`fract` assumed tenths — TODO confirm).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrimLevel {
    /// Whole dB of attenuation.
    pub whole: u8,
    /// Fractional part (assumed tenths of a dB).
    pub fract: u8,
}

impl TrimLevel {
    /// Level in dBu (`22.0 - whole - fract/10`).
    #[must_use]
    pub fn dbu(self) -> f64 {
        tables::TRIM_REFERENCE_DBU - f64::from(self.whole) - f64::from(self.fract) / 10.0
    }
}

/// A trim bank (`get_trim_configs` reply / `set_trim_config` args).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrimConfig {
    /// 2 = LINE IN (confirmed); 3 = LINE OUT (unconfirmed).
    pub trim_id: u8,
    /// 0 = ALL (ganged), 1 = MANUAL.
    pub control: u8,
    /// Per-channel levels (reads return 64; writes send the first 32).
    pub levels: Vec<TrimLevel>,
}

/// Per-mixer reverb (`get_reverb_config {ext3: mixer}`), raw 0..=255
/// values (units TODO).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverbConfig {
    /// Mixer 0..=3.
    pub mixer_id: u8,
    /// Room size.
    pub room_size: u8,
    /// Colour.
    pub color: u8,
    /// Pre-delay.
    pub predelay: u8,
    /// Density.
    pub density: u8,
    /// Early reflections gain.
    pub early_ref_gain: u8,
    /// Late reflections delay.
    pub late_ref_delay: u8,
    /// Richness.
    pub richness: u8,
    /// Reverb time.
    pub reverb_time: u8,
    /// Reverb level.
    pub reverb_level: u8,
    /// 1 = on.
    pub on: u8,
}

impl ReverbConfig {
    /// Field names in wire (schema) order, excluding `mixer_id`.
    pub const FIELDS: [&'static str; 10] = [
        "room_size",
        "color",
        "predelay",
        "density",
        "early_ref_gain",
        "late_ref_delay",
        "richness",
        "reverb_time",
        "reverb_level",
        "on",
    ];

    /// Field value by name.
    #[must_use]
    pub fn get(&self, field: &str) -> Option<u8> {
        Some(match field {
            "room_size" => self.room_size,
            "color" => self.color,
            "predelay" => self.predelay,
            "density" => self.density,
            "early_ref_gain" => self.early_ref_gain,
            "late_ref_delay" => self.late_ref_delay,
            "richness" => self.richness,
            "reverb_time" => self.reverb_time,
            "reverb_level" => self.reverb_level,
            "on" => self.on,
            _ => return None,
        })
    }

    /// Set a field by name; `false` if unknown.
    pub fn set(&mut self, field: &str, v: u8) -> bool {
        let slot = match field {
            "room_size" => &mut self.room_size,
            "color" => &mut self.color,
            "predelay" => &mut self.predelay,
            "density" => &mut self.density,
            "early_ref_gain" => &mut self.early_ref_gain,
            "late_ref_delay" => &mut self.late_ref_delay,
            "richness" => &mut self.richness,
            "reverb_time" => &mut self.reverb_time,
            "reverb_level" => &mut self.reverb_level,
            "on" => &mut self.on,
            _ => return false,
        };
        *slot = v;
        true
    }
}

/// One AFX insert: effect type id + instance id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AfxSlot {
    /// Effect type id (0 = empty slot).
    #[serde(rename = "type")]
    pub effect: u8,
    /// Instance id (per type, allocated lowest-free across all strips).
    pub inst: u8,
}

/// `get_afx_available_instances` entry: remaining instances of a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AfxAvailability {
    /// Effect type id.
    pub type_id: u8,
    /// Instances still available.
    pub inst_count: i16,
}

/// Monitor-section toggles (`set_dim`/`set_mute`/`set_mono [output, 0|1]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorToggle {
    /// Dim (confirmed via cyclic `volumes_and_mutes[0].dim`).
    Dim,
    /// Mute.
    Mute,
    /// Mono.
    Mono,
}

impl MonitorToggle {
    /// Wire method.
    #[must_use]
    pub const fn method(self) -> &'static str {
        match self {
            Self::Dim => "set_dim",
            Self::Mute => "set_mute",
            Self::Mono => "set_mono",
        }
    }

    /// Key in cyclic `volumes_and_mutes[n]`.
    #[must_use]
    pub const fn state_key(self) -> &'static str {
        match self {
            Self::Dim => "dim",
            Self::Mute => "mute",
            Self::Mono => "mono",
        }
    }
}

/// Monitor output state from the cyclic report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorState {
    /// Raw volume (units TODO; `set_volume` takes the same scale).
    pub volume: u16,
    /// Muted.
    pub mute: bool,
    /// Dimmed.
    pub dim: bool,
    /// Mono.
    pub mono: bool,
}

/// Clock + monitor view of the cyclic-115 state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceState {
    /// Index into [`tables::SYNC_SOURCES`].
    pub sync_source: u8,
    /// Measured sample rate in Hz.
    pub sample_rate: u32,
    /// Clock locked.
    pub locked: bool,
    /// Main monitor (`volumes_and_mutes[0]`).
    pub monitor: MonitorState,
}

fn u8_field(v: &Value, key: &str) -> Option<u8> {
    v.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u8::try_from(n).ok())
}

impl DeviceState {
    /// Extract from cyclic-115 `contents`.
    #[must_use]
    pub fn from_cyclic(c: &Value) -> Option<Self> {
        let mon = c
            .get("volumes_and_mutes")?
            .get(usize::from(MONITOR_OUTPUT))?;
        Some(Self {
            sync_source: u8_field(c, "sync_source")?,
            sample_rate: tables::sample_rate_from_bytes(
                u8_field(c, "sync_freq_hi")?,
                u8_field(c, "sync_freq_mid")?,
                u8_field(c, "sync_freq_low")?,
            ),
            locked: u8_field(c, "locked")? != 0,
            monitor: MonitorState {
                volume: mon
                    .get("volume")
                    .and_then(Value::as_u64)
                    .and_then(|n| u16::try_from(n).ok())?,
                mute: u8_field(mon, "mute")? != 0,
                dim: u8_field(mon, "dim")? != 0,
                mono: u8_field(mon, "mono")? != 0,
            },
        })
    }

    /// A monitor toggle's current value.
    #[must_use]
    pub const fn monitor_toggle(&self, t: MonitorToggle) -> bool {
        match t {
            MonitorToggle::Dim => self.monitor.dim,
            MonitorToggle::Mute => self.monitor.mute,
            MonitorToggle::Mono => self.monitor.mono,
        }
    }
}

// ── Pure call builders ────────────────────────────────────────────────

fn check(cond: bool, what: impl FnOnce() -> String) -> Result<()> {
    if cond {
        Ok(())
    } else {
        Err(AntelopeError::Invalid(what()))
    }
}

/// `["set_mixer", [mixer, ch], {sender: ch, level, pan, mute, solo, send}]`.
///
/// # Errors
/// Mixer ≥ 4 or channel > 32.
pub fn mixer_call(mixer: u8, ch: u8, s: &MixerStrip) -> Result<Call> {
    check(mixer < MIXERS, || format!("mixer {mixer} (0..{MIXERS})"))?;
    check(ch <= MIXER_STRIPS, || {
        format!("mixer channel {ch} (0..={MIXER_STRIPS})")
    })?;
    Ok(Call::new("set_mixer")
        .arg(mixer)
        .arg(ch)
        .kwarg("sender", ch)
        .kwarg("level", s.level)
        .kwarg("pan", s.pan)
        .kwarg("mute", s.mute)
        .kwarg("solo", s.solo)
        .kwarg("send", s.send))
}

/// `["set_routing", [page, [[type, ch] × 32]], {}]` — replaces the page.
///
/// # Errors
/// Unknown page or not exactly 32 slots.
pub fn routing_call(page: u8, slots: &[RouteSlot]) -> Result<Call> {
    check(page < ROUTING_PAGES, || format!("routing page {page}"))?;
    check(slots.len() == ROUTING_SLOTS, || {
        format!(
            "routing page needs {ROUTING_SLOTS} slots, got {}",
            slots.len()
        )
    })?;
    let cells: Vec<Value> = slots.iter().map(|s| json!([s.ty, s.ch])).collect();
    Ok(Call::new("set_routing").arg(page).arg(cells))
}

/// `["set_trim_config", [trim_id, control, [[whole, fract] × 32]], {}]`.
///
/// # Errors
/// Fewer than 32 levels or control > 1.
pub fn trim_call(cfg: &TrimConfig) -> Result<Call> {
    check(cfg.control <= 1, || format!("trim control {}", cfg.control))?;
    let levels = cfg.levels.get(..ROUTING_SLOTS).ok_or_else(|| {
        AntelopeError::Invalid(format!(
            "trim config needs 32 levels, got {}",
            cfg.levels.len()
        ))
    })?;
    let cells: Vec<Value> = levels.iter().map(|l| json!([l.whole, l.fract])).collect();
    Ok(Call::new("set_trim_config")
        .arg(cfg.trim_id)
        .arg(cfg.control)
        .arg(cells))
}

/// `["set_dim" | "set_mute" | "set_mono", [output, 0|1], {}]`.
#[must_use]
pub fn monitor_toggle_call(t: MonitorToggle, output: u8, on: bool) -> Call {
    Call::new(t.method()).arg(output).arg(u8::from(on))
}

/// `["set_volume", [output, value], {}]` (raw scale, TODO units).
#[must_use]
pub fn monitor_volume_call(output: u8, volume: u16) -> Call {
    Call::new("set_volume").arg(output).arg(volume)
}

/// `["set_reverb_config", [mixer_id, room_size, …, on], {}]` — positional
/// in schema order (TODO: confirm positional vs kwargs on the wire).
///
/// # Errors
/// Mixer ≥ 4.
pub fn reverb_call(r: &ReverbConfig) -> Result<Call> {
    check(r.mixer_id < MIXERS, || format!("mixer {}", r.mixer_id))?;
    let mut c = Call::new("set_reverb_config").arg(r.mixer_id);
    for f in ReverbConfig::FIELDS {
        c = c.arg(r.get(f).unwrap_or_default());
    }
    Ok(c)
}

/// `["set_afx_order", [strip_index, [{"type", "inst"}, …]], {}]` —
/// replaces the strip's whole insert chain (empty list clears it).
///
/// # Errors
/// Strip ≥ 32 or more than 8 slots.
pub fn afx_order_call(strip_index: u8, slots: &[AfxSlot]) -> Result<Call> {
    check(strip_index < AFX_STRIPS, || {
        format!("AFX strip index {strip_index}")
    })?;
    check(slots.len() <= AFX_SLOTS, || {
        format!("{} AFX slots (max {AFX_SLOTS})", slots.len())
    })?;
    let v = serde_json::to_value(slots)?;
    Ok(Call::new("set_afx_order").arg(strip_index).arg(v))
}

/// `["set_samp_rate", [srate_idx], {}]` — **interrupts audio**.
///
/// # Errors
/// Index outside [`tables::SAMPLE_RATES`].
pub fn sample_rate_call(index: u8) -> Result<Call> {
    check(usize::from(index) < tables::SAMPLE_RATES.len(), || {
        format!("sample rate index {index}")
    })?;
    Ok(Call::new("set_samp_rate").arg(index))
}

/// `["set_sync_source", [src_index], {}]` — **interrupts audio**.
///
/// # Errors
/// Index outside [`tables::SYNC_SOURCES`].
pub fn sync_source_call(index: u8) -> Result<Call> {
    check(usize::from(index) < tables::SYNC_SOURCES.len(), || {
        format!("sync source index {index}")
    })?;
    Ok(Call::new("set_sync_source").arg(index))
}

/// Parse a `get_routing` reply (`{bank_idx, bank_configs: 64 × {…}}`),
/// keeping the first 32 slots.
///
/// # Errors
/// Shape mismatch.
pub fn parse_routing_reply(page: u8, contents: &Value) -> Result<RoutingPage> {
    #[derive(Deserialize)]
    struct Reply {
        bank_configs: Vec<RouteSlot>,
    }
    let r: Reply = serde_json::from_value(contents.clone())?;
    let slots = r
        .bank_configs
        .get(..ROUTING_SLOTS)
        .ok_or_else(|| AntelopeError::Protocol(format!("get_routing {page}: short reply")))?
        .to_vec();
    Ok(RoutingPage { page, slots })
}

/// Parse a `get_afx_strip_order` reply into its non-empty slots.
///
/// # Errors
/// Shape mismatch.
pub fn parse_afx_strip_reply(contents: &Value) -> Result<Vec<AfxSlot>> {
    #[derive(Deserialize)]
    struct Entry {
        slots: Vec<AfxSlot>,
    }
    let entries: Vec<Entry> = serde_json::from_value(contents.clone())?;
    Ok(entries
        .into_iter()
        .next()
        .map(|e| e.slots.into_iter().filter(|s| s.effect != 0).collect())
        .unwrap_or_default())
}

// ── The handle ────────────────────────────────────────────────────────

/// Typed Galaxy32 operations over a [`Client`].
#[derive(Debug, Clone)]
pub struct Galaxy32 {
    client: Client,
}

fn ext3(i: u8) -> Vec<(String, Value)> {
    vec![("ext3".to_owned(), Value::from(i))]
}

impl Galaxy32 {
    /// Wrap a connected client.
    #[must_use]
    pub const fn new(client: Client) -> Self {
        Self { client }
    }

    /// The underlying client (raw calls, event stream).
    #[must_use]
    pub const fn client(&self) -> &Client {
        &self.client
    }

    /// Escape hatch: fire-and-forget any method (AFX, surround, …).
    ///
    /// # Errors
    /// Socket failure.
    pub async fn raw_call(
        &self,
        method: &str,
        args: Vec<Value>,
        kwargs: Vec<(String, Value)>,
    ) -> Result<()> {
        self.client.call(method, args, kwargs).await
    }

    /// Escape hatch: any read, correlated by `(ext2, ext3)`.
    ///
    /// # Errors
    /// Timeout / FAIL / socket failure.
    pub async fn raw_request(
        &self,
        method: &str,
        args: Vec<Value>,
        kwargs: Vec<(String, Value)>,
        key: (u64, u64),
    ) -> Result<Value> {
        self.client.request(method, args, kwargs, key).await
    }

    async fn get(&self, method: &str, ext2: u64, index: u8) -> Result<Value> {
        self.client
            .request(method, Vec::new(), ext3(index), (ext2, u64::from(index)))
            .await
    }

    /// `get_mixer {ext3: mixer}` → 33 entries (0 = master, 1..=32 strips).
    ///
    /// # Errors
    /// Request failure or shape mismatch.
    pub async fn get_mixer(&self, mixer: u8) -> Result<Vec<MixerStrip>> {
        let v = self.get("get_mixer", ext2::MIXER, mixer).await?;
        Ok(serde_json::from_value(v)?)
    }

    /// One strip (0 = master) of `get_mixer`.
    ///
    /// # Errors
    /// Request failure or channel out of range.
    pub async fn get_mixer_strip(&self, mixer: u8, ch: u8) -> Result<MixerStrip> {
        self.get_mixer(mixer)
            .await?
            .get(usize::from(ch))
            .copied()
            .ok_or_else(|| AntelopeError::Protocol(format!("get_mixer {mixer}: no channel {ch}")))
    }

    /// Send a whole strip (fire-and-forget).
    ///
    /// # Errors
    /// Invalid indices or socket failure.
    pub async fn set_mixer_strip(&self, mixer: u8, ch: u8, strip: &MixerStrip) -> Result<()> {
        self.client.send(&mixer_call(mixer, ch, strip)?).await
    }

    /// `get_routing {ext3: page}`.
    ///
    /// # Errors
    /// Request failure or shape mismatch.
    pub async fn get_routing(&self, page: u8) -> Result<RoutingPage> {
        let v = self.get("get_routing", ext2::ROUTING, page).await?;
        parse_routing_reply(page, &v)
    }

    /// Replace a whole routing page (fire-and-forget).
    ///
    /// # Errors
    /// Invalid page/slots or socket failure.
    pub async fn set_routing(&self, page: u8, slots: &[RouteSlot]) -> Result<()> {
        self.client.send(&routing_call(page, slots)?).await
    }

    /// `get_trim_configs {ext3: trim_id}`.
    ///
    /// # Errors
    /// Request failure or shape mismatch.
    pub async fn get_trim_config(&self, trim_id: u8) -> Result<TrimConfig> {
        let v = self.get("get_trim_configs", ext2::TRIM, trim_id).await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Replace a trim bank (fire-and-forget).
    ///
    /// # Errors
    /// Invalid config or socket failure.
    pub async fn set_trim_config(&self, cfg: &TrimConfig) -> Result<()> {
        self.client.send(&trim_call(cfg)?).await
    }

    /// Monitor dim/mute/mono on `output` (0 = main monitor).
    ///
    /// # Errors
    /// Socket failure.
    pub async fn set_monitor_toggle(&self, t: MonitorToggle, output: u8, on: bool) -> Result<()> {
        self.client.send(&monitor_toggle_call(t, output, on)).await
    }

    /// Monitor volume (raw scale).
    ///
    /// # Errors
    /// Socket failure.
    pub async fn set_monitor_volume(&self, output: u8, volume: u16) -> Result<()> {
        self.client.send(&monitor_volume_call(output, volume)).await
    }

    /// `get_reverb_config {ext3: mixer}`.
    ///
    /// # Errors
    /// Request failure or shape mismatch.
    pub async fn get_reverb(&self, mixer: u8) -> Result<ReverbConfig> {
        let v = self.get("get_reverb_config", ext2::REVERB, mixer).await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Replace a mixer's reverb config (fire-and-forget).
    ///
    /// # Errors
    /// Invalid mixer or socket failure.
    pub async fn set_reverb(&self, cfg: &ReverbConfig) -> Result<()> {
        self.client.send(&reverb_call(cfg)?).await
    }

    /// `get_afx_strip_order {ext3: strip_index}` → non-empty slots.
    ///
    /// # Errors
    /// Request failure or shape mismatch.
    pub async fn get_afx_strip(&self, strip_index: u8) -> Result<Vec<AfxSlot>> {
        let v = self
            .get("get_afx_strip_order", ext2::AFX_STRIP_ORDER, strip_index)
            .await?;
        parse_afx_strip_reply(&v)
    }

    /// Replace a strip's insert chain (fire-and-forget; `[]` clears).
    ///
    /// # Errors
    /// Invalid strip/slots or socket failure.
    pub async fn set_afx_strip(&self, strip_index: u8, slots: &[AfxSlot]) -> Result<()> {
        self.client.send(&afx_order_call(strip_index, slots)?).await
    }

    /// `get_afx_available_instances` (remaining instances per type).
    ///
    /// # Errors
    /// Request failure or shape mismatch.
    pub async fn get_afx_available(&self) -> Result<Vec<AfxAvailability>> {
        let v = self
            .client
            .request(
                "get_afx_available_instances",
                Vec::new(),
                Vec::new(),
                (ext2::AFX_AVAILABLE, 0),
            )
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Lowest instance id of `effect` not used on any strip, provided the
    /// server still has an instance available. This is how the panel
    /// allocates (Opto 2A with 0–5 in use → 6).
    ///
    /// # Errors
    /// Request failure, or no instance left.
    pub async fn allocate_afx_instance(&self, effect: u8) -> Result<u8> {
        let available = self
            .get_afx_available()
            .await?
            .into_iter()
            .find(|a| a.type_id == effect)
            .map_or(0, |a| a.inst_count);
        if available <= 0 {
            return Err(AntelopeError::Invalid(format!(
                "no AFX instance of type {effect} available"
            )));
        }
        let mut used = std::collections::HashSet::new();
        for strip in 0..AFX_STRIPS {
            for s in self.get_afx_strip(strip).await? {
                if s.effect == effect {
                    used.insert(s.inst);
                }
            }
        }
        (0..=u8::MAX).find(|i| !used.contains(i)).ok_or_else(|| {
            AntelopeError::Invalid(format!("AFX type {effect}: instance ids exhausted"))
        })
    }

    /// Latest cyclic state view.
    #[must_use]
    pub fn state(&self) -> Option<DeviceState> {
        self.client
            .latest_state()
            .as_deref()
            .and_then(DeviceState::from_cyclic)
    }

    /// Poll the cyclic state until `pred` holds (or `timeout`).
    pub async fn wait_state(
        &self,
        timeout: Duration,
        pred: impl Fn(&DeviceState) -> bool + Send + Sync,
    ) -> Option<DeviceState> {
        let poll = async {
            loop {
                if let Some(s) = self.state().filter(|s| pred(s)) {
                    return s;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        tokio::time::timeout(timeout, poll).await.ok()
    }

    /// Select sample rate by [`tables::SAMPLE_RATES`] index. **Drops audio
    /// while the clock relocks.** Index mapping is inferred from UI order.
    ///
    /// # Errors
    /// Invalid index or socket failure.
    pub async fn set_sample_rate_index(&self, index: u8) -> Result<()> {
        self.client.send(&sample_rate_call(index)?).await
    }

    /// Select clock source by [`tables::SYNC_SOURCES`] index. **Drops
    /// audio while the clock relocks.** Index mapping is inferred.
    ///
    /// # Errors
    /// Invalid index or socket failure.
    pub async fn set_sync_source_index(&self, index: u8) -> Result<()> {
        self.client.send(&sync_source_call(index)?).await
    }
}
