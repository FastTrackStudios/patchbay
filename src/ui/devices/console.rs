//! Turning a flat [`DeviceView`] into something a console can render.
//!
//! The wire model is a list of `path → value` params, which is right for
//! the protocol and wrong for a mixer: a channel strip needs its name,
//! colour, fader and mute together, addressed by channel, not found by
//! scanning a few thousand entries per repaint.
//!
//! So each device view indexes the params once ([`Params`]) and pulls
//! the strips it needs out of that ([`strips`]). Pure functions over the
//! wire types — no dioxus, no RPC — so the layouts are unit-testable
//! against devices that aren't plugged in.

use std::collections::HashMap;

use crate::proto::{DeviceParamKind, DeviceParamValue, DeviceView, ParamView};

/// Params indexed by path.
///
/// `mirror::apply_event` on the engine side is a linear scan, and so is
/// `params.iter().find(…)`; at the TF's ~4,400 params doing that per
/// control per repaint is the difference between a console and a
/// slideshow. Built once per view.
pub struct Params<'a> {
    by_path: HashMap<&'a str, &'a ParamView>,
}

impl<'a> Params<'a> {
    #[must_use]
    pub fn index(view: &'a DeviceView) -> Self {
        Self {
            by_path: view.params.iter().map(|p| (p.path.as_str(), p)).collect(),
        }
    }

    #[must_use]
    pub fn get(&self, path: &str) -> Option<&'a ParamView> {
        self.by_path.get(path).copied()
    }

    /// A level in dB, `None` when the path is absent or isn't a level.
    #[must_use]
    pub fn level(&self, path: &str) -> Option<f64> {
        match self.get(path)?.value {
            DeviceParamValue::Level(db) => Some(db),
            _ => None,
        }
    }

    #[must_use]
    pub fn pan(&self, path: &str) -> Option<f64> {
        match self.get(path)?.value {
            DeviceParamValue::Pan(p) => Some(p),
            _ => None,
        }
    }

    #[must_use]
    pub fn toggle(&self, path: &str) -> Option<bool> {
        match self.get(path)?.value {
            DeviceParamValue::Toggle(b) => Some(b),
            _ => None,
        }
    }

    #[must_use]
    pub fn text(&self, path: &str) -> Option<String> {
        match &self.get(path)?.value {
            DeviceParamValue::Text(t) => Some(t.clone()),
            _ => None,
        }
    }

    /// An enum's selected option, as its label.
    ///
    /// The label matters more than the index here: the TF interns
    /// console values it hasn't seen, so the option list is authoritative
    /// and a hard-coded table would go stale.
    #[must_use]
    pub fn choice(&self, path: &str) -> Option<String> {
        let p = self.get(path)?;
        let DeviceParamValue::Enum(i) = p.value else {
            return None;
        };
        let DeviceParamKind::Enum { options } = &p.kind else {
            return None;
        };
        options.get(usize::try_from(i).ok()?).cloned()
    }

    /// The options an enum param offers, in order.
    #[must_use]
    pub fn options(&self, path: &str) -> Vec<String> {
        match &self.get(path).map(|p| &p.kind) {
            Some(DeviceParamKind::Enum { options }) => options.clone(),
            _ => Vec::new(),
        }
    }

    /// A param rendered for display: the bare string for text, the
    /// device's own formatting for everything else.
    ///
    /// `DeviceParamValue::display` quotes text, which is right for the
    /// inspector (where seeing the quotes tells you the type) and wrong
    /// in a list of device names.
    #[must_use]
    pub fn show(&self, path: &str) -> String {
        self.get(path).map_or_else(String::new, |p| match &p.value {
            DeviceParamValue::Text(t) => t.clone(),
            other => other.display(Some(&p.kind)),
        })
    }

    /// Whether the device will accept a write to `path`.
    #[must_use]
    pub fn writable(&self, path: &str) -> bool {
        self.get(path).is_some_and(|p| p.writable)
    }
}

/// One channel strip, however the device spells it.
#[derive(Debug, Clone, PartialEq)]
pub struct Strip {
    /// Path prefix every leaf hangs off (`in/3`, `mixer/1/strip/16`).
    pub prefix: String,
    /// What the console calls this position (`CH 3`, `AUX 2`).
    pub position: String,
    /// The name someone gave it, empty when the device has no names.
    pub name: String,
    /// Console colour token (`SkyBlue`), empty when it has no colours.
    pub color: String,
    /// Console icon token (`E.Guitar`), empty when it has none.
    pub icon: String,
    /// Fader position in dB, `None` when the strip has no level.
    pub level_db: Option<f64>,
    /// Pan, -1.0 (L) ..= 1.0 (R).
    pub pan: Option<f64>,
    /// Passing audio. **A TF `on` key is not a mute** — it is already
    /// the right way round here.
    pub on: Option<bool>,
}

impl Strip {
    /// What to show as the strip's title: its name, or its position when
    /// nobody has named it.
    #[must_use]
    pub fn title(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.position
        } else {
            &self.name
        }
    }

    /// Whether the strip is silent — off, or fader all the way down.
    #[must_use]
    pub fn silent(&self) -> bool {
        self.on == Some(false) || self.level_db.is_some_and(|db| db <= SILENT_FADER_DB)
    }
}

/// At or below this a fader is off rather than quiet (the TF sends
/// `-32768` = -327.68 dB for -inf; the Galaxy's floor is -96).
const SILENT_FADER_DB: f64 = -96.0;

/// Which leaf names a device family uses for a strip's parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leaves {
    pub name: &'static str,
    pub color: &'static str,
    pub icon: &'static str,
    pub level: &'static str,
    pub pan: &'static str,
    /// The leaf that means "passing audio", and whether it is inverted
    /// (a `mute` is, an `on` is not).
    pub on: &'static str,
    pub on_inverted: bool,
}

/// Yamaha TF: named, coloured strips with an ON key.
pub const TF_LEAVES: Leaves = Leaves {
    name: "name",
    color: "color",
    icon: "icon",
    level: "level",
    pan: "pan",
    on: "on",
    on_inverted: false,
};

/// Antelope Galaxy 32 mixer: no names or colours, and a mute.
pub const GALAXY_LEAVES: Leaves = Leaves {
    name: "",
    color: "",
    icon: "",
    level: "level",
    pan: "pan",
    on: "mute",
    on_inverted: true,
};

/// Build strips for `prefixes`, reading `leaves` off each.
///
/// A prefix with no level at all is dropped: it isn't a strip, it is a
/// path that happens to share the shape.
#[must_use]
pub fn strips(
    params: &Params<'_>,
    prefixes: impl IntoIterator<Item = (String, String)>,
    leaves: Leaves,
) -> Vec<Strip> {
    prefixes
        .into_iter()
        .filter_map(|(prefix, position)| {
            let at = |leaf: &str| {
                if leaf.is_empty() {
                    String::new()
                } else {
                    format!("{prefix}/{leaf}")
                }
            };
            let level_db = params.level(&at(leaves.level));
            level_db?;
            let on = params
                .toggle(&at(leaves.on))
                .map(|v| if leaves.on_inverted { !v } else { v });
            Some(Strip {
                name: params.text(&at(leaves.name)).unwrap_or_default(),
                color: params.choice(&at(leaves.color)).unwrap_or_default(),
                icon: params.choice(&at(leaves.icon)).unwrap_or_default(),
                pan: params.pan(&at(leaves.pan)),
                level_db,
                on,
                prefix,
                position,
            })
        })
        .collect()
}

/// `(prefix, position label)` for `count` 1-based channels under `base`.
#[must_use]
pub fn numbered(base: &str, label: &str, count: u32) -> Vec<(String, String)> {
    (1..=count)
        .map(|n| (format!("{base}/{n}"), format!("{label} {n}")))
        .collect()
}

/// How many 1-based children `base` has, by looking at what the device
/// actually sent — the TF1 has 32 inputs and a TF5 has 48, and the param
/// table already knows which.
#[must_use]
pub fn count_under(view: &DeviceView, base: &str) -> u32 {
    let prefix = format!("{base}/");
    view.params
        .iter()
        .filter_map(|p| {
            let rest = p.path.strip_prefix(&prefix)?;
            rest.split('/').next()?.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0)
}

/// The eight TF colour tokens as something to paint with.
///
/// The console names colours (`SkyBlue`), it doesn't send hex, and it
/// can name one we've never seen — anything unknown falls back to a
/// stable hash so a new token is still tellable-apart.
#[must_use]
pub fn color_hex(token: &str) -> String {
    match token {
        "Blue" => "#3d6ff0".to_owned(),
        "Orange" => "#f08a2a".to_owned(),
        "Yellow" => "#e8c62e".to_owned(),
        "Purple" => "#9a5cf0".to_owned(),
        "SkyBlue" => "#46b6e8".to_owned(),
        "Pink" => "#ef6ba8".to_owned(),
        "Red" => "#e0503f".to_owned(),
        "Green" => "#4ab861".to_owned(),
        "" => String::new(),
        other => crate::ui::state::auto_color(other),
    }
}
