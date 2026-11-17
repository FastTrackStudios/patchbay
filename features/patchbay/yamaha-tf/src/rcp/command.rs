//! Command encoding (client → console).

use std::fmt;

use super::token::{Token, is_bare_word, quote};
use crate::error::{Result, TfError};

/// A parameter value on the wire: an integer or a (quoted) string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RcpValue {
    /// Integer (levels in dB×100, on/off, pan…).
    Int(i64),
    /// String (names, colours, icons…), quoted on the wire.
    Str(String),
}

impl RcpValue {
    /// Interpret a reply token: quoted → string, else integer, else the
    /// bare word as a string.
    #[must_use]
    pub fn from_token(t: &Token) -> Self {
        if t.quoted {
            return Self::Str(t.text.clone());
        }
        t.text
            .parse::<i64>()
            .map_or_else(|_| Self::Str(t.text.clone()), Self::Int)
    }

    /// The integer, if this is one.
    #[must_use]
    pub const fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            Self::Str(_) => None,
        }
    }

    /// The string, if this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Int(_) => None,
            Self::Str(s) => Some(s),
        }
    }
}

impl fmt::Display for RcpValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(n) => write!(f, "{n}"),
            Self::Str(s) => write!(f, "{s:?}"),
        }
    }
}

/// Scene bank (`scene_a` / `scene_b`). TF has 2 × 100 scenes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SceneBank {
    /// Bank A (`scene_a`).
    A,
    /// Bank B (`scene_b`).
    B,
}

impl SceneBank {
    /// Both banks.
    pub const ALL: [Self; 2] = [Self::A, Self::B];

    /// The wire token.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::A => "scene_a",
            Self::B => "scene_b",
        }
    }

    /// The panel letter.
    #[must_use]
    pub const fn letter(self) -> char {
        match self {
            Self::A => 'A',
            Self::B => 'B',
        }
    }

    /// Parse `scene_a` / `scene_b`.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "scene_a" => Some(Self::A),
            "scene_b" => Some(Self::B),
            _ => None,
        }
    }

    /// Parse the panel letter (`A`/`a`, `B`/`b`).
    #[must_use]
    pub const fn from_letter(c: char) -> Option<Self> {
        match c {
            'A' | 'a' => Some(Self::A),
            'B' | 'b' => Some(Self::B),
            _ => None,
        }
    }
}

/// Highest scene number per bank (0–99).
pub const MAX_SCENE: u8 = 99;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Arg {
    Word(String),
    Int(i64),
    Str(String),
}

/// One command line. Build with the constructors, send with
/// [`crate::Client::request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    verb: String,
    args: Vec<Arg>,
}

impl Command {
    /// `get <address> <x> <y>`.
    #[must_use]
    pub fn get(address: &str, x: u16, y: u16) -> Self {
        Self {
            verb: "get".to_owned(),
            args: vec![
                Arg::Word(address.to_owned()),
                Arg::Int(i64::from(x)),
                Arg::Int(i64::from(y)),
            ],
        }
    }

    /// `set <address> <x> <y> <value>` (strings quoted and escaped).
    ///
    /// **Changes console state.**
    #[must_use]
    pub fn set(address: &str, x: u16, y: u16, value: RcpValue) -> Self {
        Self {
            verb: "set".to_owned(),
            args: vec![
                Arg::Word(address.to_owned()),
                Arg::Int(i64::from(x)),
                Arg::Int(i64::from(y)),
                match value {
                    RcpValue::Int(n) => Arg::Int(n),
                    RcpValue::Str(s) => Arg::Str(s),
                },
            ],
        }
    }

    /// `devinfo <what>` (`productname`, `version`, `devicename`, `serialno`…).
    #[must_use]
    pub fn devinfo(what: &str) -> Self {
        Self::raw("devinfo", &[what])
    }

    /// `devstatus <what>` (`runmode`).
    #[must_use]
    pub fn devstatus(what: &str) -> Self {
        Self::raw("devstatus", &[what])
    }

    /// `prminfo <index>`.
    #[must_use]
    pub fn prminfo(index: u32) -> Self {
        Self {
            verb: "prminfo".to_owned(),
            args: vec![Arg::Int(i64::from(index))],
        }
    }

    /// `sscurrent_ex <bank>` (only the active bank answers OK).
    #[must_use]
    pub fn sscurrent(bank: SceneBank) -> Self {
        Self::raw("sscurrent_ex", &[bank.wire()])
    }

    /// `ssinfo_ex <bank> <n>`.
    #[must_use]
    pub fn ssinfo(bank: SceneBank, n: u8) -> Self {
        Self {
            verb: "ssinfo_ex".to_owned(),
            args: vec![Arg::Word(bank.wire().to_owned()), Arg::Int(i64::from(n))],
        }
    }

    /// `ssrecall_ex <bank> <n>`. **Recalls a scene: changes everything.**
    #[must_use]
    pub fn ssrecall(bank: SceneBank, n: u8) -> Self {
        Self {
            verb: "ssrecall_ex".to_owned(),
            args: vec![Arg::Word(bank.wire().to_owned()), Arg::Int(i64::from(n))],
        }
    }

    /// `scpmode keepalive <ms>`: the console drops the session when
    /// nothing arrives within `ms`. A session setting.
    #[must_use]
    pub fn scpmode_keepalive(ms: u32) -> Self {
        Self {
            verb: "scpmode".to_owned(),
            args: vec![Arg::Word("keepalive".to_owned()), Arg::Int(i64::from(ms))],
        }
    }

    /// Any verb with bare-word arguments (escape hatch).
    #[must_use]
    pub fn raw(verb: &str, words: &[&str]) -> Self {
        Self {
            verb: verb.to_owned(),
            args: words.iter().map(|w| Arg::Word((*w).to_owned())).collect(),
        }
    }

    /// The verb (`get`, `set`, …).
    #[must_use]
    pub fn verb(&self) -> &str {
        &self.verb
    }

    /// Whether this command changes console state (and is rate limited).
    #[must_use]
    pub fn is_write(&self) -> bool {
        matches!(
            self.verb.as_str(),
            "set" | "ssrecall_ex" | "ssupdate_ex" | "event"
        )
    }

    /// Reply correlation key: the leading arguments a matching `OK` reply
    /// echoes (`<address> <x> <y>` for get/set, the first argument
    /// otherwise).
    #[must_use]
    pub fn match_key(&self) -> Vec<String> {
        let n = if matches!(self.verb.as_str(), "get" | "set") {
            3
        } else {
            1
        };
        self.args
            .iter()
            .take(n)
            .map(|a| match a {
                Arg::Word(s) | Arg::Str(s) => s.clone(),
                Arg::Int(i) => i.to_string(),
            })
            .collect()
    }

    /// The wire line including the trailing LF.
    ///
    /// # Errors
    /// [`TfError::Invalid`] if the verb or a bare word contains spaces,
    /// quotes or control characters, or a string contains a line break.
    pub fn encode(&self) -> Result<String> {
        if !is_bare_word(&self.verb) {
            return Err(TfError::Invalid(format!("bad verb {:?}", self.verb)));
        }
        let mut line = self.verb.clone();
        for a in &self.args {
            line.push(' ');
            match a {
                Arg::Word(w) => {
                    if !is_bare_word(w) {
                        return Err(TfError::Invalid(format!("bad bare argument {w:?}")));
                    }
                    line.push_str(w);
                }
                Arg::Int(n) => line.push_str(&n.to_string()),
                Arg::Str(s) => line.push_str(&quote(s)?),
            }
        }
        line.push('\n');
        Ok(line)
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let line = self.encode().unwrap_or_else(|_| format!("{} …", self.verb));
        f.write_str(line.trim_end())
    }
}
