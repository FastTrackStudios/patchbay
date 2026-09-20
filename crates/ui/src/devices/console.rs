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

use patchbay_proto::{DeviceParamKind, DeviceParamValue, DeviceView, ParamView};

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
        other => crate::state::auto_color(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patchbay_proto::{DeviceLinkState, DeviceSummary};

    fn param(path: &str, kind: DeviceParamKind, value: DeviceParamValue) -> ParamView {
        ParamView {
            path: path.to_owned(),
            label: path.to_owned(),
            kind,
            value,
            writable: true,
            disruptive: false,
        }
    }

    fn enum_param(path: &str, options: &[&str], index: u32) -> ParamView {
        param(
            path,
            DeviceParamKind::Enum {
                options: options.iter().map(|s| (*s).to_owned()).collect(),
            },
            DeviceParamValue::Enum(index),
        )
    }

    fn level(path: &str, db: f64) -> ParamView {
        param(
            path,
            DeviceParamKind::Level {
                min_db: -138.0,
                max_db: 10.0,
            },
            DeviceParamValue::Level(db),
        )
    }

    fn view(params: Vec<ParamView>) -> DeviceView {
        DeviceView {
            summary: DeviceSummary {
                id: "yamaha:tf:1".into(),
                name: "TF1".into(),
                kind: "yamaha-tf".into(),
                vendor: String::new(),
                model: String::new(),
                serial: String::new(),
                firmware: String::new(),
                transport: String::new(),
                state: DeviceLinkState::Online,
                error: String::new(),
            },
            inputs: Vec::new(),
            outputs: Vec::new(),
            routes: Vec::new(),
            params,
        }
    }

    fn tf_channel(n: u32, name: &str, color: &str, db: f64, on: bool) -> Vec<ParamView> {
        vec![
            param(
                &format!("in/{n}/name"),
                DeviceParamKind::Text,
                DeviceParamValue::Text(name.to_owned()),
            ),
            enum_param(
                &format!("in/{n}/color"),
                &["Blue", "Orange", "SkyBlue"],
                match color {
                    "Orange" => 1,
                    "SkyBlue" => 2,
                    _ => 0,
                },
            ),
            enum_param(&format!("in/{n}/icon"), &["Blank", "E.Guitar"], 1),
            level(&format!("in/{n}/level"), db),
            param(
                &format!("in/{n}/on"),
                DeviceParamKind::Toggle,
                DeviceParamValue::Toggle(on),
            ),
            param(
                &format!("in/{n}/pan"),
                DeviceParamKind::Pan,
                DeviceParamValue::Pan(-0.5),
            ),
        ]
    }

    #[test]
    fn a_tf_channel_becomes_a_strip() {
        let v = view(tf_channel(4, "GTR DI", "Orange", -6.0, true));
        let p = Params::index(&v);
        let s = strips(&p, numbered("in", "CH", 4), TF_LEAVES);
        // Only channel 4 has params, so only channel 4 is a strip.
        assert_eq!(s.len(), 1);
        let s = s.first().expect("one strip");
        assert_eq!(s.prefix, "in/4");
        assert_eq!(s.position, "CH 4");
        assert_eq!(s.name, "GTR DI");
        assert_eq!(s.color, "Orange");
        assert_eq!(s.icon, "E.Guitar");
        assert_eq!(s.level_db, Some(-6.0));
        assert_eq!(s.pan, Some(-0.5));
        // An `on` key is NOT a mute: on = passing audio.
        assert_eq!(s.on, Some(true));
        assert_eq!(s.title(), "GTR DI");
        assert!(!s.silent());
    }

    #[test]
    fn an_unnamed_strip_falls_back_to_its_position() {
        let v = view(tf_channel(7, "   ", "Blue", 0.0, true));
        let p = Params::index(&v);
        let s = strips(&p, numbered("in", "CH", 8), TF_LEAVES);
        assert_eq!(s.first().map(Strip::title), Some("CH 7"));
    }

    #[test]
    fn off_and_fully_down_both_read_as_silent() {
        let off = view(tf_channel(1, "X", "Blue", 0.0, false));
        let down = view(tf_channel(1, "X", "Blue", -327.68, true));
        for v in [off, down] {
            let p = Params::index(&v);
            let s = strips(&p, numbered("in", "CH", 1), TF_LEAVES);
            assert!(s.first().expect("strip").silent());
        }
    }

    #[test]
    fn a_galaxy_mute_is_inverted_into_on() {
        let v = view(vec![
            level("mixer/1/strip/16/level", -3.0),
            param(
                "mixer/1/strip/16/mute",
                DeviceParamKind::Toggle,
                DeviceParamValue::Toggle(true),
            ),
        ]);
        let p = Params::index(&v);
        let s = strips(
            &p,
            vec![("mixer/1/strip/16".to_owned(), "16".to_owned())],
            GALAXY_LEAVES,
        );
        let s = s.first().expect("strip");
        // muted = true means NOT passing audio.
        assert_eq!(s.on, Some(false));
        assert!(s.silent());
        // The Galaxy has no names or colours; the strip says so rather
        // than inventing them.
        assert_eq!(s.name, "");
        assert_eq!(s.color, "");
        assert_eq!(s.title(), "16");
    }

    #[test]
    fn channel_counts_come_from_the_device_not_a_guess() {
        let mut params = tf_channel(1, "a", "Blue", 0.0, true);
        params.extend(tf_channel(32, "b", "Blue", 0.0, true));
        let v = view(params);
        assert_eq!(count_under(&v, "in"), 32);
        assert_eq!(count_under(&v, "aux"), 0);
    }

    #[test]
    fn an_unknown_colour_token_still_gets_a_colour() {
        assert_eq!(color_hex("Red"), "#e0503f");
        assert_eq!(color_hex(""), "");
        // Not one of the eight: stable, and not empty.
        let made_up = color_hex("Chartreuse");
        assert!(!made_up.is_empty());
        assert_eq!(made_up, color_hex("Chartreuse"));
    }
}
