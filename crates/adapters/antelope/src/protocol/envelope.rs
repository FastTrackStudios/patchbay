//! Server → client envelopes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::call::Call;
use crate::error::Result;

/// Header of `cyclic` / `single` frames.
///
/// - `cyclic`: `cmd` 115 = Galaxy32 master state (~10 Hz), 131 = AFX
///   meters.
/// - `single` (cmd 117) replies to `get_*`: `ext2` = the method's schema
///   `ext2` id, `ext3` = the requested index. Replies are correlated by
///   `(ext2, ext3)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// Report class.
    #[serde(default)]
    pub cmd: u32,
    /// Sequence counter.
    #[serde(default)]
    pub seq: u64,
    /// Method id (replies) / validity bitmap (cyclic).
    #[serde(default)]
    pub ext2: u64,
    /// Index (replies).
    #[serde(default)]
    pub ext3: u64,
}

impl Header {
    /// `(ext2, ext3)` — the reply correlation key.
    #[must_use]
    pub const fn key(&self) -> (u64, u64) {
        (self.ext2, self.ext3)
    }
}

/// One server frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ServerFrame {
    /// Periodic full device state.
    Cyclic {
        /// Always 1 so far.
        #[serde(default)]
        protocol_version: Option<u32>,
        /// Report header.
        header: Header,
        /// State object (untyped; see the `galaxy32` views).
        contents: Value,
    },
    /// Reply to a `get_*` request. Failed requests come back header-less
    /// with `COMMAND_STATUS: "FAIL"` and empty contents.
    Single {
        /// Always 1 when present.
        #[serde(default)]
        protocol_version: Option<u32>,
        /// Absent on failures.
        #[serde(default)]
        header: Option<Header>,
        /// Reply payload.
        #[serde(default)]
        contents: Value,
        /// `"FAIL"` on failures.
        #[serde(default, rename = "COMMAND_STATUS")]
        command_status: Option<String>,
    },
    /// Control port: rebroadcast of another client's write,
    /// `contents = ["set_mixer", [args], {kwargs}]` (full, untruncated).
    /// Admin port: human-readable status text + `state`.
    Notification {
        /// Call array (control port) or status string (admin port).
        contents: Value,
        /// Admin-port server state, e.g. `running`.
        #[serde(default)]
        state: Option<String>,
    },
}

impl ServerFrame {
    /// Parse a frame body.
    ///
    /// # Errors
    /// Invalid JSON or an unknown `type`.
    pub fn parse(body: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(body)?)
    }

    /// The header, if the frame has one.
    #[must_use]
    pub const fn header(&self) -> Option<&Header> {
        match self {
            Self::Cyclic { header, .. } => Some(header),
            Self::Single { header, .. } => header.as_ref(),
            Self::Notification { .. } => None,
        }
    }

    /// Whether this is a failed `single` reply.
    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Single { command_status: Some(s), .. } if s.eq_ignore_ascii_case("fail"))
    }

    /// For control-port notifications: the rebroadcast call.
    #[must_use]
    pub fn notified_call(&self) -> Option<Call> {
        match self {
            Self::Notification { contents, .. } => Call::from_value(contents),
            _ => None,
        }
    }
}
