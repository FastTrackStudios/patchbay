//! Galaxy32 constant tables (confirmed on fw 8.24 / Manager Server
//! 1.8.19 unless marked TODO).
//!
//! Group ids are the panel's own session-file ids (`LINE_IN0`,
//! `COM_PLAY1`, …) so they stay stable and recognisable.

/// Routing slots per `set_routing` page (`get_routing` returns 64; only
/// the first 32 are meaningful, the rest read `[0,0]`).
pub const ROUTING_SLOTS: usize = 32;
/// Number of routing pages (outputs) on Galaxy32.
pub const ROUTING_PAGES: u8 = 19;
/// Source type meaning "no source" (rendered `M` in the panel). The
/// server stores any type ≥ 21 as 18.
pub const SOURCE_NONE: u8 = 18;

/// TODO(unconfirmed on the wire): MONITOR page width. From the panel
/// session file (`MONITOR0`, 2 ch) and the baseline (`[17,0],[17,1]`).
pub const MONITOR_CHANNELS: u16 = 2;
/// TODO(unconfirmed on the wire): MIC EMU width, from the session file.
pub const MIC_EMU_CHANNELS: u16 = 8;
/// TODO(unconfirmed on the wire): SURROUND width, from the session file.
pub const SURROUND_CHANNELS: u16 = 16;

/// Mixers (`set_mixer` arg 0 is `0..MIXERS`).
pub const MIXERS: u8 = 4;
/// Strips per mixer (1-based; 0 is the master).
pub const MIXER_STRIPS: u8 = 32;
/// `set_mixer` channel index of the mixer master.
pub const MIXER_MASTER: u8 = 0;
/// Pan hard left.
pub const PAN_LEFT: u8 = 2;
/// Pan centre.
pub const PAN_CENTER: u8 = 32;
/// Pan hard right.
pub const PAN_RIGHT: u8 = 62;
/// Level/send attenuation meaning -inf (confirmed for `send`).
pub const ATTENUATION_OFF: u8 = 96;

/// `set_trim_config` / `get_trim_configs` id of the LINE IN trims (confirmed).
pub const TRIM_LINE_IN: u8 = 2;
/// TODO(unconfirmed): probable LINE OUT trim id.
pub const TRIM_LINE_OUT: u8 = 3;
/// Trim reference: `whole`/`fract` are dB of attenuation below this (dBu).
pub const TRIM_REFERENCE_DBU: f64 = 22.0;
/// Trim `control`: 0 = ALL (ganged), 1 = MANUAL (per channel).
pub const TRIM_CONTROL_OPTIONS: [&str; 2] = ["ALL", "MANUAL"];

/// `set_dim` / `set_mute` / `set_mono` / `set_volume` output id of the
/// main monitor.
pub const MONITOR_OUTPUT: u8 = 0;

/// AFX strips (`set_afx_order` arg 0 is `0..AFX_STRIPS`).
pub const AFX_STRIPS: u8 = 32;
/// Insert slots per AFX strip.
pub const AFX_SLOTS: usize = 8;

/// Schema `ext2` ids of the reads we use (reply header correlation).
pub mod ext2 {
    /// `get_routing {ext3: page}`.
    pub const ROUTING: u64 = 3;
    /// `get_mixer {ext3: mixer}`.
    pub const MIXER: u64 = 4;
    /// `get_reverb_config {ext3: mixer}`.
    pub const REVERB: u64 = 10;
    /// `get_afx_available_instances`.
    pub const AFX_AVAILABLE: u64 = 12;
    /// `get_trim_configs {ext3: trim_id}`.
    pub const TRIM: u64 = 15;
    /// `get_afx_remaining_featured_instances`.
    pub const AFX_REMAINING: u64 = 21;
    /// `get_afx_strip_order {ext3: strip}`.
    pub const AFX_STRIP_ORDER: u64 = 25;
}

/// Clock sources in panel order. Index = `set_sync_source` arg (inferred
/// from UI order; the device reports `sync_source` = 0 with INTERNAL
/// selected).
pub const SYNC_SOURCES: [&str; 12] = [
    "INTERNAL", "W.C.", "ADAT", "ADAT x2", "ADAT x4", "MADI", "MADI x2", "MADI x4", "S/PDIF",
    "HDX/INT", "HDX/LS", "DANTE",
];

/// Sample rates in panel order. Index = `set_samp_rate` arg (inferred).
pub const SAMPLE_RATES: [u32; 7] = [32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// One router output group = one routing page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputPage {
    /// `set_routing` / `get_routing` page index.
    pub page: u8,
    /// Stable group id.
    pub id: &'static str,
    /// Panel name.
    pub name: &'static str,
    /// Meaningful slots on this page.
    pub channels: u16,
}

/// One router input group = one source type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceType {
    /// `[type, ch]` type index.
    pub ty: u8,
    /// Stable group id.
    pub id: &'static str,
    /// Panel name.
    pub name: &'static str,
    /// Channels (`ch` is 0-based below this).
    pub channels: u16,
}

const fn page(page: u8, id: &'static str, name: &'static str, channels: u16) -> OutputPage {
    OutputPage {
        page,
        id,
        name,
        channels,
    }
}

const fn source(ty: u8, id: &'static str, name: &'static str, channels: u16) -> SourceType {
    SourceType {
        ty,
        id,
        name,
        channels,
    }
}

/// Routing pages, panel order (confirmed by live `set_routing`/`get_routing`).
pub const OUTPUT_PAGES: [OutputPage; 19] = [
    page(0, "LINE_OUT0", "LINE OUT 1-32", 32),
    page(1, "MONITOR0", "MONITOR", MONITOR_CHANNELS),
    page(2, "COM_REC0", "DAW IN 1-32", 32),
    page(3, "COM_REC1", "DAW IN 33-64", 32),
    page(4, "DANTE_OUT0", "DANTE OUT 1-32", 32),
    page(5, "DANTE_OUT1", "DANTE OUT 33-64", 32),
    page(6, "DIGI_OUT0", "HDX OUT 1-32", 32),
    page(7, "DIGI_OUT1", "HDX OUT 33-64", 32),
    page(8, "MADI_OUT0", "MADI OUT 1-32", 32),
    page(9, "MADI_OUT1", "MADI OUT 33-64", 32),
    page(10, "ADAT_OUT0", "ADAT OUT 1-8", 8),
    page(11, "SPDIF_OUT0", "SPDIF OUT", 2),
    page(12, "AFX_IN0", "AFX IN 1-32", 32),
    page(13, "MIXER_IN0", "MIX1 IN", 32),
    page(14, "MIXER_IN1", "MIX2 IN", 32),
    page(15, "MIXER_IN2", "MIX3 IN", 32),
    page(16, "MIXER_IN3", "MIX4 IN", 32),
    page(17, "MIC_IN0", "MIC EMU IN", MIC_EMU_CHANNELS),
    page(18, "SURROUND_IN0", "SURROUND IN", SURROUND_CHANNELS),
];

/// Source types, panel order. 6, 7, 8 and 16 are inferred from order
/// (their panel tabs are greyed on this unit); 17 from the MONITOR page.
pub const SOURCE_TYPES: [SourceType; 18] = [
    source(0, "LINE_IN0", "LINE IN 1-32", 32),
    source(1, "COM_PLAY0", "DAW OUT 1-32", 32),
    source(2, "COM_PLAY1", "DAW OUT 33-64", 32),
    source(3, "DANTE_IN0", "DANTE IN 1-32", 32),
    source(4, "DANTE_IN1", "DANTE IN 33-64", 32),
    source(5, "DIGI_IN0", "HDX IN 1-32", 32),
    source(6, "DIGI_IN1", "HDX IN 33-64", 32),
    source(7, "MADI_IN0", "MADI IN 1-32", 32),
    source(8, "MADI_IN1", "MADI IN 33-64", 32),
    source(9, "ADAT_IN0", "ADAT IN 1-8", 8),
    source(10, "SPDIF_IN0", "SPDIF IN", 2),
    source(11, "AFX_OUT0", "AFX OUT 1-32", 32),
    source(12, "MIXER_OUT0", "MIX1 OUT", 2),
    source(13, "MIXER_OUT1", "MIX2 OUT", 2),
    source(14, "MIXER_OUT2", "MIX3 OUT", 2),
    source(15, "MIXER_OUT3", "MIX4 OUT", 2),
    source(16, "MIC_OUT0", "MIC EMU OUT", MIC_EMU_CHANNELS),
    source(17, "SURROUND_OUT0", "SURROUND OUT", SURROUND_CHANNELS),
];

/// Output page by index.
#[must_use]
pub fn output_page(page: u8) -> Option<&'static OutputPage> {
    OUTPUT_PAGES.iter().find(|p| p.page == page)
}

/// Output page by group id.
#[must_use]
pub fn output_page_by_id(id: &str) -> Option<&'static OutputPage> {
    OUTPUT_PAGES.iter().find(|p| p.id == id)
}

/// Source type by index (`None` for 18 = no source, and unknown types).
#[must_use]
pub fn source_type(ty: u8) -> Option<&'static SourceType> {
    SOURCE_TYPES.iter().find(|s| s.ty == ty)
}

/// Source type by group id.
#[must_use]
pub fn source_type_by_id(id: &str) -> Option<&'static SourceType> {
    SOURCE_TYPES.iter().find(|s| s.id == id)
}

/// Decode the cyclic `sync_freq_hi/mid/low` bytes (24-bit big-endian Hz):
/// `(0, 187, 128)` → 48000.
#[must_use]
pub const fn sample_rate_from_bytes(hi: u8, mid: u8, low: u8) -> u32 {
    u32::from_be_bytes([0, hi, mid, low])
}

/// Index into [`SAMPLE_RATES`] of an exact rate.
#[must_use]
pub fn sample_rate_index(hz: u32) -> Option<usize> {
    SAMPLE_RATES.iter().position(|r| *r == hz)
}
