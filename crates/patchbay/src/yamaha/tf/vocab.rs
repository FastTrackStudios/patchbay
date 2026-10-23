//! Label vocabularies (colours, icons, categories) for TF consoles.
//!
//! These are `Enum` params whose option list is the vocabulary below.
//! Values the console reports that are not listed (other firmware, other
//! models) are appended at read time, so an enum index always round-trips
//! to the exact wire string.

/// TF label colours.
///
/// Order follows the `yamaha-rcp` crate's `LabelColor` (MIT, TF1-tested; used as reference only). TF uses
/// `SkyBlue`/`Pink` where CL/QL use `Cyan`/`Magenta`. Whether the console
/// accepts `Off` is unverified, so it is not offered.
pub const TF_COLORS: &[&str] = &[
    "Blue", "Orange", "Yellow", "Purple", "SkyBlue", "Pink", "Red", "Green",
];

/// Icon names.
///
/// The Companion module's list (CL/QL-derived, MIT) plus `Media3` and `SubWoofer`, which a live TF1 V4.55 reports. The live
/// console confirmed `Blank`, `Drumkit`, `DynamicMic`, `E.Bass`,
/// `E.Guitar`, `Effect`, `In-Ear`, `Keyboard`, `Media2`, `Media3`, `Organ`,
/// `PC`, `Piano`, `Speaker`, `SubWoofer`, `Wedge`, `WirelessMic`; the rest
/// are unverified on TF (a rejected write surfaces as an error).
pub const TF_ICONS: &[&str] = &[
    "Kick",
    "Snare",
    "Hi-Hat",
    "FloorTom",
    "Drumkit",
    "Perc.",
    "A.Bass",
    "E.Bass",
    "BassAmp",
    "A.Guitar",
    "E.Guitar",
    "GuitarAmp",
    "Trumpet",
    "Trombone",
    "Saxophone",
    "Strings",
    "Piano",
    "Organ",
    "Keyboard",
    "Male",
    "Female",
    "Choir",
    "DynamicMic",
    "CondenserMic",
    "WirelessMic",
    "SpeechMic",
    "Speaker",
    "SubWoofer",
    "Wedge",
    "In-Ear",
    "Effect",
    "Processor",
    "Media1",
    "Media2",
    "Media3",
    "Video",
    "Mixer",
    "PC",
    "Audience",
    "Star1",
    "Star2",
    "Blank",
];

/// Icon categories observed on a live TF1 V4.55. The full TF list is not
/// documented anywhere public; unseen categories are appended at read time.
pub const TF_CATEGORIES: &[&str] = &["Vocal", "Guitars", "Others", "Output", "FX RTN"];

/// Which vocabulary a label param uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VocabKind {
    /// `Label/Color`.
    Color,
    /// `Label/Icon`.
    Icon,
    /// `Label/Category`.
    Category,
}

/// The live vocabularies (seeded from the constants, grown by reads).
#[derive(Debug, Clone)]
pub struct Vocab {
    colors: Vec<String>,
    icons: Vec<String>,
    categories: Vec<String>,
}

impl Default for Vocab {
    fn default() -> Self {
        let own = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect();
        Self {
            colors: own(TF_COLORS),
            icons: own(TF_ICONS),
            categories: own(TF_CATEGORIES),
        }
    }
}

impl Vocab {
    /// The option list for `kind`.
    #[must_use]
    pub fn options(&self, kind: VocabKind) -> &[String] {
        match kind {
            VocabKind::Color => &self.colors,
            VocabKind::Icon => &self.icons,
            VocabKind::Category => &self.categories,
        }
    }

    const fn options_mut(&mut self, kind: VocabKind) -> &mut Vec<String> {
        match kind {
            VocabKind::Color => &mut self.colors,
            VocabKind::Icon => &mut self.icons,
            VocabKind::Category => &mut self.categories,
        }
    }

    /// Index of `name` (exact match first, then case-insensitive).
    #[must_use]
    pub fn index_of(&self, kind: VocabKind, name: &str) -> Option<u32> {
        let opts = self.options(kind);
        opts.iter()
            .position(|o| o == name)
            .or_else(|| opts.iter().position(|o| o.eq_ignore_ascii_case(name)))
            .and_then(|i| u32::try_from(i).ok())
    }

    /// Index of `name`, appending it if unseen.
    pub fn intern(&mut self, kind: VocabKind, name: &str) -> Option<u32> {
        if let Some(i) = self.index_of(kind, name) {
            return Some(i);
        }
        let opts = self.options_mut(kind);
        let i = u32::try_from(opts.len()).ok()?;
        tracing::debug!(?kind, name, "yamaha: new label vocabulary entry");
        opts.push(name.to_owned());
        Some(i)
    }

    /// The wire string for index `i`.
    #[must_use]
    pub fn name(&self, kind: VocabKind, i: u32) -> Option<&str> {
        let i = usize::try_from(i).ok()?;
        self.options(kind).get(i).map(String::as_str)
    }
}
