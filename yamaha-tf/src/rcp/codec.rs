//! LF line framing.
//!
//! RCP is line-based ASCII: every command and reply ends with LF. TCP
//! chunks can split a line anywhere or carry several lines, so bytes are
//! buffered until a LF arrives. A trailing CR is stripped defensively.

use crate::error::{Result, TfError};

/// A line longer than this without a LF means the stream is garbage.
pub const MAX_LINE: usize = 64 * 1024;

/// Incremental LF line decoder.
#[derive(Debug, Default)]
pub struct LineCodec {
    buf: Vec<u8>,
}

impl LineCodec {
    /// An empty decoder.
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Append freshly read bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes buffered after the last complete line.
    #[must_use]
    pub const fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Next complete line (LF removed, trailing CRs stripped, invalid
    /// UTF-8 replaced), or `None` until more bytes arrive.
    ///
    /// # Errors
    /// [`TfError::Protocol`] when more than [`MAX_LINE`] bytes arrive
    /// without a LF; the buffer is discarded so decoding can resync.
    pub fn next_line(&mut self) -> Result<Option<String>> {
        if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop();
            while line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
        if self.buf.len() > MAX_LINE {
            let n = self.buf.len();
            self.buf.clear();
            return Err(TfError::Protocol(format!(
                "{n} bytes without a line feed; discarding"
            )));
        }
        Ok(None)
    }
}
