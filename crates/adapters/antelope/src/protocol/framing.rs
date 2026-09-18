//! Length-prefixed framing.
//!
//! ```text
//! [ u32 big-endian length, INCLUDING these 4 bytes ][ UTF-8 JSON body ]
//! ```
//!
//! Frames concatenate with no delimiter.

use crate::error::{AntelopeError, Result};

/// Size of the length prefix.
pub(crate) const HEADER_LEN: usize = 4;

/// Refuse frames larger than this (the biggest seen is the ~235 KB
/// `initialize_format`; cyclic state is ~9 KB).
pub(crate) const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// Prefix `body` with its inclusive length.
///
/// # Errors
/// [`AntelopeError::Protocol`] if the frame would exceed `u32::MAX` /
/// [`MAX_FRAME_LEN`].
pub fn encode_frame(body: &[u8]) -> Result<Vec<u8>> {
    let total = body
        .len()
        .checked_add(HEADER_LEN)
        .filter(|t| *t <= MAX_FRAME_LEN)
        .ok_or_else(|| {
            AntelopeError::Protocol(format!("frame body too large ({} bytes)", body.len()))
        })?;
    let len = u32::try_from(total)
        .map_err(|_| AntelopeError::Protocol("frame length overflows u32".into()))?;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(body);
    Ok(out)
}

/// Incremental decoder: feed it whatever `read()` returned, pull complete
/// frame bodies out. Handles frames split across reads and several frames
/// per read.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    /// Empty decoder.
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Append received bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes buffered but not yet returned as a frame.
    #[must_use]
    pub const fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Pop the next complete frame body, or `Ok(None)` if more bytes are
    /// needed.
    ///
    /// # Errors
    /// [`AntelopeError::Protocol`] on an impossible length (below the
    /// header size or above [`MAX_FRAME_LEN`]). The stream is then out of
    /// sync and the connection should be dropped.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>> {
        let Some(header) = self.buf.get(..HEADER_LEN) else {
            return Ok(None);
        };
        let header: [u8; HEADER_LEN] = header
            .try_into()
            .map_err(|_| AntelopeError::Protocol("short header".into()))?;
        let len = usize::try_from(u32::from_be_bytes(header))
            .map_err(|_| AntelopeError::Protocol("frame length overflows usize".into()))?;
        if !(HEADER_LEN..=MAX_FRAME_LEN).contains(&len) {
            return Err(AntelopeError::Protocol(format!(
                "impossible frame length {len}"
            )));
        }
        let Some(body) = self.buf.get(HEADER_LEN..len) else {
            return Ok(None);
        };
        let body = body.to_vec();
        self.buf.drain(..len);
        Ok(Some(body))
    }
}
