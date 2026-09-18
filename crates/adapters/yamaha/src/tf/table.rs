//! The parameter table: `prminfo` rows → generic, path-addressed params.
//!
//! Path grammar (1-based, like the panel):
//!
//! ```text
//! in/{1-32}/{name,color,icon,category,level,on,pan,role,panmode}
//! in/{n}/send/aux/{1-20}/{level,on,pan,prepost}
//! in/{n}/send/fx/{1-2}/{level,on,prepost}
//! in/{n}/send/sub/{level,on}
//! stin/{1-4}/…      (ST IN 1L, 1R, 2L, 2R — same leaves as `in`)
//! fxrtn/{1-4}/…     (FX1 L/R, FX2 L/R — no fx sends)
//! aux/{1-20}/{name,color,icon,category,level,on,balance,panlink,role,bustype,panmode}
//! aux/{n}/send/matrix/{1-4}/{level,on}
//! matrix/{1-4}/{name,color,icon,category,level,on,role}
//! stereo/{l,r}/{name,color,icon,category,level,on,balance,panmode,role}
//! stereo/{l,r}/send/matrix/{1-4}/{level,on}
//! sub/{name,color,icon,category,level,on}, sub/send/matrix/{1-4}/{level,on}
//! dca/{1-8}/{name,color,icon,category,level,on}
//! mutegroup/{1-6}/{on,name}
//! scene/{current,title,modified,recall}
//! ```
//!
//! Skipped on purpose: the `DcaCh/*` and `ToStereo/Pan` aliases (their
//! canonical twins are mapped; NOTIFYs on the aliases are normalized) and
//! `MIXER:Setup/MonitorMix/Password` (security-sensitive — never read or
//! written).

use std::collections::HashMap;

use crate::error::{Result, TfError};
use crate::rcp::reply::PrmInfo;
use crate::tf::vocab::VocabKind;

/// `MIXER:Current/` prefix of every mapped address.
pub const CURRENT: &str = "MIXER:Current/";

/// The console's `prminfo` table captured from a TF1 V4.55 (108 rows,
/// verbatim reply lines). Fallback when live discovery fails.
pub const TF1_PRMINFO_JSON: &str = include_str!("../../assets/tf1_prminfo.json");

/// Parse [`TF1_PRMINFO_JSON`].
///
/// # Errors
/// Only if the embedded asset is corrupt.
pub fn embedded_prminfo() -> Result<Vec<PrmInfo>> {
    let lines: Vec<String> = serde_json::from_str(TF1_PRMINFO_JSON)
        .map_err(|e| TfError::Protocol(format!("embedded prminfo: {e}")))?;
    lines.iter().map(|l| PrmInfo::parse_line(l)).collect()
}

/// Physical channel counts per block. `prminfo` reports the shared TF
/// table (e.g. `InCh` x = 40 even on a TF1, where x 32–39 answer but are
/// not real channels), so counts are clamped to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelLimits {
    /// Mono input channels (`InCh`).
    pub in_ch: u16,
    /// Stereo input halves (`StInCh`).
    pub st_in: u16,
    /// FX return halves (`FxRtnCh`).
    pub fx_rtn: u16,
    /// FX buses (`ToFx` y).
    pub fx: u16,
    /// AUX buses (`Mix`).
    pub mix: u16,
    /// Matrices (`Mtrx`).
    pub mtrx: u16,
    /// Stereo master sides (`St`).
    pub st: u16,
    /// SUB bus (`Mono`).
    pub mono: u16,
    /// DCAs.
    pub dca: u16,
    /// Mute groups (`MuteMaster`).
    pub mute: u16,
}

impl ModelLimits {
    /// TF1: 32 mono + 2 stereo inputs, 2 stereo FX returns, 20 AUX,
    /// 2 FX, 4 matrix, ST L/R, SUB, 8 DCA, 6 mute groups.
    pub const TF1: Self = Self {
        in_ch: 32,
        st_in: 4,
        fx_rtn: 4,
        fx: 2,
        mix: 20,
        mtrx: 4,
        st: 2,
        mono: 1,
        dca: 8,
        mute: 6,
    };

    /// No clamping: take `prminfo` counts at face value.
    pub const UNLIMITED: Self = Self {
        in_ch: u16::MAX,
        st_in: u16::MAX,
        fx_rtn: u16::MAX,
        fx: u16::MAX,
        mix: u16::MAX,
        mtrx: u16::MAX,
        st: u16::MAX,
        mono: u16::MAX,
        dca: u16::MAX,
        mute: u16::MAX,
    };

    /// Limits for a `devinfo productname`, if known.
    #[must_use]
    pub fn for_product(name: &str) -> Option<Self> {
        match name {
            "TF1" => Some(Self::TF1),
            _ => None,
        }
    }
}

/// Channel blocks under `MIXER:Current/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Block {
    /// `InCh` → `in`.
    InCh,
    /// `StInCh` → `stin`.
    StInCh,
    /// `FxRtnCh` → `fxrtn`.
    FxRtnCh,
    /// `Mix` → `aux`.
    Mix,
    /// `Mtrx` → `matrix`.
    Mtrx,
    /// `St` → `stereo`.
    St,
    /// `Mono` → `sub`.
    Mono,
    /// `DCA` → `dca`.
    Dca,
    /// `MuteMaster` → `mutegroup`.
    MuteMaster,
}

impl Block {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "InCh" => Self::InCh,
            "StInCh" => Self::StInCh,
            "FxRtnCh" => Self::FxRtnCh,
            "Mix" => Self::Mix,
            "Mtrx" => Self::Mtrx,
            "St" => Self::St,
            "Mono" => Self::Mono,
            "DCA" => Self::Dca,
            "MuteMaster" => Self::MuteMaster,
            _ => return None,
        })
    }

    /// Path prefix.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::InCh => "in",
            Self::StInCh => "stin",
            Self::FxRtnCh => "fxrtn",
            Self::Mix => "aux",
            Self::Mtrx => "matrix",
            Self::St => "stereo",
            Self::Mono => "sub",
            Self::Dca => "dca",
            Self::MuteMaster => "mutegroup",
        }
    }

    const fn display(self) -> &'static str {
        match self {
            Self::InCh => "CH",
            Self::StInCh => "ST IN",
            Self::FxRtnCh => "FX RTN",
            Self::Mix => "AUX",
            Self::Mtrx => "MATRIX",
            Self::St => "ST",
            Self::Mono => "SUB",
            Self::Dca => "DCA",
            Self::MuteMaster => "MUTE",
        }
    }

    const fn limit(self, l: &ModelLimits) -> u16 {
        match self {
            Self::InCh => l.in_ch,
            Self::StInCh => l.st_in,
            Self::FxRtnCh => l.fx_rtn,
            Self::Mix => l.mix,
            Self::Mtrx => l.mtrx,
            Self::St => l.st,
            Self::Mono => l.mono,
            Self::Dca => l.dca,
            Self::MuteMaster => l.mute,
        }
    }

    /// `(path segment, display label)` of instance `x`; the path segment
    /// is `None` for the single SUB bus.
    fn instance(self, x: u16) -> (Option<String>, String) {
        let n = u32::from(x).saturating_add(1);
        match self {
            Self::Mono => (None, "SUB".to_owned()),
            Self::St => {
                let side = if x == 0 { "l" } else { "r" };
                let seg = if x < 2 {
                    side.to_owned()
                } else {
                    n.to_string()
                };
                (Some(seg), format!("ST {}", side.to_uppercase()))
            }
            Self::StInCh | Self::FxRtnCh => {
                let pair = x
                    .checked_div(2)
                    .map_or(0, |p| u32::from(p).saturating_add(1));
                let side = if x.checked_rem(2) == Some(0) {
                    "L"
                } else {
                    "R"
                };
                (
                    Some(n.to_string()),
                    format!("{} {pair}{side}", self.display()),
                )
            }
            _ => (Some(n.to_string()), format!("{} {n}", self.display())),
        }
    }
}

/// How a param's wire value maps to a generic value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// dB×100 integer, -32768 = -inf → [`patchbay_device::ParamValue::Level`].
    Level,
    /// 0/1 → `Toggle`.
    Toggle,
    /// -63..63 → `Pan` (-1..1).
    Pan,
    /// Send pre/post: `Enum` over `["post", "pre"]` (index = wire value;
    /// inferred from the console defaults `ToMix` = 1 / `ToFx` = 0, the usual
    /// AUX-pre / FX-post convention — unverified).
    PrePost,
    /// Plain integer.
    Int {
        /// Minimum.
        min: i64,
        /// Maximum.
        max: i64,
    },
    /// Quoted string of at most `max_len` ASCII characters.
    Text {
        /// Maximum length (prminfo max).
        max_len: usize,
    },
    /// One of a label vocabulary (colour/icon/category).
    Label(VocabKind),
    /// Read-only opaque value (`Role`, `BusType`) rendered as text.
    Opaque,
}

/// Scene pseudo-params.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SceneField {
    /// `scene/current` — `A00`…`B99` (read-only).
    Current,
    /// `scene/title` (read-only).
    Title,
    /// `scene/modified` — edited since recall (read-only).
    Modified,
    /// `scene/recall` — write `A05`/`B22` to recall (disruptive).
    Recall,
}

/// Where a param lives on the console.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// A `get`/`set` address.
    Rcp {
        /// Wire address.
        address: String,
        /// 0-based x.
        x: u16,
        /// 0-based y.
        y: u16,
    },
    /// A scene pseudo-param.
    Scene(SceneField),
}

/// One mapped parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamDef {
    /// Generic path, e.g. `in/3/send/aux/2/level`.
    pub path: String,
    /// Display label, e.g. `CH 3 → AUX 2 level`.
    pub label: String,
    /// Console location.
    pub target: Target,
    /// Value mapping.
    pub kind: ValueKind,
    /// Writable.
    pub writable: bool,
    /// Interrupts the show (scene recall).
    pub disruptive: bool,
}

struct Leaf {
    /// Path tail after the instance segment (may contain `{y}`).
    tail: &'static str,
    kind: LeafKind,
    /// Which y-bus this is a send to (None = no y dimension).
    bus: Option<Bus>,
    /// Force read-only regardless of prminfo.
    read_only: bool,
}

#[derive(Clone, Copy)]
enum LeafKind {
    Level,
    Toggle,
    Pan,
    PrePost,
    Int,
    Text,
    Label(VocabKind),
    Opaque,
}

#[derive(Clone, Copy)]
enum Bus {
    Fx,
    Aux,
    Sub,
    Matrix,
}

impl Bus {
    const fn limit(self, l: &ModelLimits) -> u16 {
        match self {
            Self::Fx => l.fx,
            Self::Aux => l.mix,
            Self::Sub => 1,
            Self::Matrix => l.mtrx,
        }
    }

    const fn display(self) -> &'static str {
        match self {
            Self::Fx => "FX",
            Self::Aux => "AUX",
            Self::Sub => "SUB",
            Self::Matrix => "MATRIX",
        }
    }
}

const fn leaf(tail: &'static str, kind: LeafKind) -> Leaf {
    Leaf {
        tail,
        kind,
        bus: None,
        read_only: false,
    }
}

const fn send(bus: Bus, tail: &'static str, kind: LeafKind) -> Leaf {
    Leaf {
        tail,
        kind,
        bus: Some(bus),
        read_only: false,
    }
}

const fn info(tail: &'static str, kind: LeafKind) -> Leaf {
    Leaf {
        tail,
        kind,
        bus: None,
        read_only: true,
    }
}

fn map_leaf(rest: &str) -> Option<Leaf> {
    use LeafKind as K;
    Some(match rest {
        "Fader/Level" => leaf("level", K::Level),
        "Fader/On" | "On" => leaf("on", K::Toggle),
        "Label/Name" => leaf("name", K::Text),
        "Label/Color" => leaf("color", K::Label(VocabKind::Color)),
        "Label/Icon" => leaf("icon", K::Label(VocabKind::Icon)),
        "Label/Category" => leaf("category", K::Label(VocabKind::Category)),
        "ToSt/Pan" => leaf("pan", K::Pan),
        "Out/Balance" => leaf("balance", K::Pan),
        "PanLink" => leaf("panlink", K::Toggle),
        "Role" => info("role", K::Opaque),
        "BusType" => info("bustype", K::Opaque),
        "PanMode" => info("panmode", K::Int),
        "ToFx/Level" => send(Bus::Fx, "level", K::Level),
        "ToFx/On" => send(Bus::Fx, "on", K::Toggle),
        "ToFx/PrePost" => send(Bus::Fx, "prepost", K::PrePost),
        "ToMix/Level" => send(Bus::Aux, "level", K::Level),
        "ToMix/On" => send(Bus::Aux, "on", K::Toggle),
        "ToMix/Pan" => send(Bus::Aux, "pan", K::Pan),
        "ToMix/PrePost" => send(Bus::Aux, "prepost", K::PrePost),
        "ToMono/Level" => send(Bus::Sub, "level", K::Level),
        "ToMono/On" => send(Bus::Sub, "on", K::Toggle),
        "ToMtrx/Level" => send(Bus::Matrix, "level", K::Level),
        "ToMtrx/On" => send(Bus::Matrix, "on", K::Toggle),
        _ => return None,
    })
}

/// Canonicalize alias addresses (`DcaCh/…` → `DCA/…`,
/// `…/ToStereo/Pan` → `…/ToSt/Pan`) so NOTIFYs on either form map.
#[must_use]
pub fn canonical_address(address: &str) -> String {
    let a = address.replacen("MIXER:Current/DcaCh/", "MIXER:Current/DCA/", 1);
    if a.ends_with("/ToStereo/Pan") {
        a.replacen("/ToStereo/Pan", "/ToSt/Pan", 1)
    } else {
        a
    }
}

/// The mapped parameter table.
#[derive(Debug, Clone, Default)]
pub struct ParamTable {
    defs: Vec<ParamDef>,
    by_path: HashMap<String, usize>,
    by_rcp: HashMap<(String, u16, u16), usize>,
    skipped: Vec<String>,
}

impl ParamTable {
    /// Build from `prminfo` rows, clamping counts to `limits`, then add
    /// the scene pseudo-params.
    #[must_use]
    pub fn build(rows: &[PrmInfo], limits: &ModelLimits) -> Self {
        let mut t = Self::default();
        for row in rows {
            if !t.add_row(row, limits) {
                t.skipped.push(row.address.clone());
            }
        }
        for (field, tail, kind, writable) in [
            (
                SceneField::Current,
                "current",
                ValueKind::Text { max_len: 3 },
                false,
            ),
            (
                SceneField::Title,
                "title",
                ValueKind::Text { max_len: 64 },
                false,
            ),
            (SceneField::Modified, "modified", ValueKind::Toggle, false),
            (
                SceneField::Recall,
                "recall",
                ValueKind::Text { max_len: 3 },
                true,
            ),
        ] {
            t.push(ParamDef {
                path: format!("scene/{tail}"),
                label: format!("Scene {tail}"),
                target: Target::Scene(field),
                kind,
                writable,
                disruptive: field == SceneField::Recall,
            });
        }
        t
    }

    fn push(&mut self, def: ParamDef) {
        let i = self.defs.len();
        self.by_path.insert(def.path.clone(), i);
        if let Target::Rcp { address, x, y } = &def.target {
            self.by_rcp.insert((address.clone(), *x, *y), i);
        }
        self.defs.push(def);
    }

    /// Map one row; `false` when the row is not mapped.
    fn add_row(&mut self, row: &PrmInfo, limits: &ModelLimits) -> bool {
        let Some(rest) = row.address.strip_prefix(CURRENT) else {
            return false;
        };
        let Some((block, rest)) = rest.split_once('/') else {
            return false;
        };
        // `DcaCh` is an alias of `DCA` and not mapped.
        let Some(block) = Block::parse(block) else {
            return false;
        };
        let Some(leaf) = map_leaf(rest) else {
            return false;
        };
        let kind = match leaf.kind {
            LeafKind::Level => ValueKind::Level,
            LeafKind::Toggle => ValueKind::Toggle,
            LeafKind::Pan => ValueKind::Pan,
            LeafKind::PrePost => ValueKind::PrePost,
            LeafKind::Int => ValueKind::Int {
                min: row.min,
                max: row.max,
            },
            LeafKind::Text => ValueKind::Text {
                max_len: usize::try_from(row.max).unwrap_or(0),
            },
            LeafKind::Label(v) => ValueKind::Label(v),
            LeafKind::Opaque => ValueKind::Opaque,
        };
        let writable = row.writable && !leaf.read_only;
        let xs = row.x_count.min(block.limit(limits));
        let ys = leaf
            .bus
            .map_or(1, |bus| row.y_count.max(1).min(bus.limit(limits)));
        for x in 0..xs {
            let (seg, inst) = block.instance(x);
            let base = seg.map_or_else(
                || block.slug().to_owned(),
                |s| format!("{}/{s}", block.slug()),
            );
            for y in 0..ys {
                let n = u32::from(y).saturating_add(1);
                let (path, label) = match leaf.bus {
                    None => (
                        format!("{base}/{}", leaf.tail),
                        format!("{inst} {}", leaf.tail),
                    ),
                    Some(Bus::Sub) => (
                        format!("{base}/send/sub/{}", leaf.tail),
                        format!("{inst} → SUB {}", leaf.tail),
                    ),
                    Some(bus) => (
                        format!(
                            "{base}/send/{}/{n}/{}",
                            bus.display().to_lowercase(),
                            leaf.tail
                        ),
                        format!("{inst} → {} {n} {}", bus.display(), leaf.tail),
                    ),
                };
                self.push(ParamDef {
                    path,
                    label,
                    target: Target::Rcp {
                        address: row.address.clone(),
                        x,
                        y,
                    },
                    kind,
                    writable,
                    disruptive: false,
                });
            }
        }
        true
    }

    /// All definitions, in table order.
    #[must_use]
    pub fn defs(&self) -> &[ParamDef] {
        &self.defs
    }

    /// Number of definitions (including scene params).
    #[must_use]
    pub const fn len(&self) -> usize {
        self.defs.len()
    }

    /// Whether the table is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Look up by generic path.
    #[must_use]
    pub fn by_path(&self, path: &str) -> Option<&ParamDef> {
        self.by_path.get(path).and_then(|&i| self.defs.get(i))
    }

    /// Look up by wire address (aliases canonicalized) and x/y.
    #[must_use]
    pub fn by_rcp(&self, address: &str, x: u16, y: u16) -> Option<&ParamDef> {
        self.by_rcp
            .get(&(canonical_address(address), x, y))
            .and_then(|&i| self.defs.get(i))
    }

    /// Addresses of `prminfo` rows that were not mapped (aliases, the
    /// `MonitorMix` password, unknown future rows).
    #[must_use]
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }
}
