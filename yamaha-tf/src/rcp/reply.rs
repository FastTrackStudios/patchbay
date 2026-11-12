//! Reply parsing (console → client).

use super::command::{RcpValue, SceneBank};
use super::token::{Token, tokenize};
use crate::error::{Result, TfError};

/// One line from the console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// `OK <verb> <args…>` or `OKm <verb> <args…>` (`modified` set for
    /// `OKm`, whose meaning is undocumented; treated exactly like `OK`).
    Ok {
        /// Echoed command verb.
        verb: String,
        /// Everything after the verb.
        args: Vec<Token>,
        /// `OKm` rather than `OK`.
        modified: bool,
    },
    /// `NOTIFY <verb> <args…>`: an unsolicited change (another client,
    /// the surface, a scene recall).
    Notify {
        /// `set`, `sscurrent_ex`, `ssrecall_ex`, `devstatus`, `mtr`…
        verb: String,
        /// Everything after the verb.
        args: Vec<Token>,
    },
    /// `ERROR <verb> <reason>`. The console does not echo the address,
    /// so errors correlate by order only.
    Error {
        /// Echoed command verb.
        verb: String,
        /// Reason code (`InvalidArgument`, `UnknownAddress`, …).
        reason: String,
    },
    /// A blank line (keepalive).
    Empty,
    /// Anything else, verbatim.
    Unknown(String),
}

impl Line {
    /// Parse one line (LF already removed).
    #[must_use]
    pub fn parse(line: &str) -> Self {
        let mut toks = tokenize(line).into_iter();
        let Some(head) = toks.next() else {
            return Self::Empty;
        };
        if head.quoted {
            return Self::Unknown(line.to_owned());
        }
        let verb = toks.next().map(|t| t.text).unwrap_or_default();
        match head.text.as_str() {
            "OK" | "OKm" => Self::Ok {
                verb,
                args: toks.collect(),
                modified: head.text == "OKm",
            },
            "NOTIFY" => Self::Notify {
                verb,
                args: toks.collect(),
            },
            "ERROR" => Self::Error {
                verb,
                reason: toks.map(|t| t.text).collect::<Vec<_>>().join(" "),
            },
            _ => Self::Unknown(line.to_owned()),
        }
    }
}

/// The payload of `OK get|set` / `NOTIFY set`:
/// `<address> <x> <y> <value> ["<display>"]`.
///
/// The value is always the 4th argument (token 5 of the line), never
/// the last: set/NOTIFY replies may append a display rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamReply {
    /// `MIXER:Current/InCh/Fader/Level`.
    pub address: String,
    /// 0-based x.
    pub x: u16,
    /// 0-based y.
    pub y: u16,
    /// The value.
    pub value: RcpValue,
    /// Optional display rendering (`"-10.00"`, `"ON"`).
    pub display: Option<String>,
}

impl ParamReply {
    /// Parse the arguments after the verb.
    ///
    /// # Errors
    /// [`TfError::Protocol`] on a short or malformed argument list.
    pub fn from_args(args: &[Token]) -> Result<Self> {
        let bad = || TfError::Protocol(format!("malformed parameter reply: {args:?}"));
        let [address, x, y, value, rest @ ..] = args else {
            return Err(bad());
        };
        Ok(Self {
            address: address.text.clone(),
            x: x.text.parse().map_err(|_| bad())?,
            y: y.text.parse().map_err(|_| bad())?,
            value: RcpValue::from_token(value),
            display: rest.first().map(|t| t.text.clone()),
        })
    }
}

/// `sscurrent_ex <bank> <n> [modified]` (reply or NOTIFY).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneCurrent {
    /// Active bank.
    pub bank: SceneBank,
    /// Scene number 0–99.
    pub number: u8,
    /// The console reports the current scene as edited since recall.
    pub modified: bool,
}

impl SceneCurrent {
    /// Parse the arguments after the verb.
    ///
    /// # Errors
    /// [`TfError::Protocol`] on a malformed argument list.
    pub fn from_args(args: &[Token]) -> Result<Self> {
        let bad = || TfError::Protocol(format!("malformed sscurrent_ex: {args:?}"));
        let [bank, n, rest @ ..] = args else {
            return Err(bad());
        };
        Ok(Self {
            bank: SceneBank::from_wire(&bank.text).ok_or_else(bad)?,
            number: n.text.parse().map_err(|_| bad())?,
            modified: rest.iter().any(|t| t.text == "modified"),
        })
    }

    /// Panel form, e.g. `B22`.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{}{:02}", self.bank.letter(), self.number)
    }
}

/// `ssinfo_ex <bank> <n> "<label>" "<title>" "<comment>" <type>`
/// (TF1 V4.55: `OK ssinfo_ex scene_b 22 "B22" "dave" "" user`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneInfo {
    /// Bank.
    pub bank: SceneBank,
    /// Number 0–99.
    pub number: u8,
    /// Scene title.
    pub title: String,
    /// Scene comment.
    pub comment: String,
}

impl SceneInfo {
    /// Parse the arguments after the verb.
    ///
    /// # Errors
    /// [`TfError::Protocol`] on a malformed argument list.
    pub fn from_args(args: &[Token]) -> Result<Self> {
        let bad = || TfError::Protocol(format!("malformed ssinfo_ex: {args:?}"));
        let [bank, n, _label, title, rest @ ..] = args else {
            return Err(bad());
        };
        Ok(Self {
            bank: SceneBank::from_wire(&bank.text).ok_or_else(bad)?,
            number: n.text.parse().map_err(|_| bad())?,
            title: title.text.clone(),
            comment: rest.first().map(|t| t.text.clone()).unwrap_or_default(),
        })
    }
}

/// Parameter data type column of `prminfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrmType {
    /// `integer`.
    Integer,
    /// `binary` (TF labels: quoted strings on the wire).
    Binary,
    /// `string`.
    String,
    /// Anything else, verbatim.
    Other(String),
}

/// One `prminfo` row:
/// `OK prminfo <i> "<address>" <x> <y> <min> <max> <default> "<unit>" <type> <ui> <rw> <scale>`.
///
/// `x`/`y` are **counts** (exclusive upper bounds); `y = 0` means "pass 0".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrmInfo {
    /// Row index.
    pub index: u32,
    /// Wire address, e.g. `MIXER:Current/InCh/Fader/Level`.
    pub address: String,
    /// x count.
    pub x_count: u16,
    /// y count (0 = no y dimension).
    pub y_count: u16,
    /// Minimum (for strings: minimum length).
    pub min: i64,
    /// Maximum (for strings: maximum length).
    pub max: i64,
    /// Default value.
    pub default: RcpValue,
    /// Unit (`dB` or empty).
    pub unit: String,
    /// Data type.
    pub ty: PrmType,
    /// UI hint (`any`).
    pub ui: String,
    /// Readable.
    pub readable: bool,
    /// Writable.
    pub writable: bool,
    /// Display divisor (100 for dB×100).
    pub scale: i64,
}

impl PrmInfo {
    /// Parse the arguments after `OK prminfo`.
    ///
    /// # Errors
    /// [`TfError::Protocol`] on a malformed row.
    pub fn from_args(args: &[Token]) -> Result<Self> {
        let bad = || TfError::Protocol(format!("malformed prminfo row: {args:?}"));
        let [
            index,
            address,
            x,
            y,
            min,
            max,
            default,
            unit,
            ty,
            ui,
            rw,
            scale,
            ..,
        ] = args
        else {
            return Err(bad());
        };
        let int = |t: &Token| t.text.parse::<i64>().map_err(|_| bad());
        Ok(Self {
            index: index.text.parse().map_err(|_| bad())?,
            address: address.text.clone(),
            x_count: x.text.parse().map_err(|_| bad())?,
            y_count: y.text.parse().map_err(|_| bad())?,
            min: int(min)?,
            max: int(max)?,
            default: RcpValue::from_token(default),
            unit: unit.text.clone(),
            ty: match ty.text.as_str() {
                "integer" => PrmType::Integer,
                "binary" => PrmType::Binary,
                "string" => PrmType::String,
                other => PrmType::Other(other.to_owned()),
            },
            ui: ui.text.clone(),
            readable: rw.text.contains('r'),
            writable: rw.text.contains('w'),
            scale: int(scale)?,
        })
    }

    /// Parse a whole `OK prminfo …` line.
    ///
    /// # Errors
    /// [`TfError::Protocol`] if the line is not an `OK prminfo` row.
    pub fn parse_line(line: &str) -> Result<Self> {
        match Line::parse(line) {
            Line::Ok { verb, args, .. } if verb == "prminfo" => Self::from_args(&args),
            _ => Err(TfError::Protocol(format!("not a prminfo row: {line:?}"))),
        }
    }
}
