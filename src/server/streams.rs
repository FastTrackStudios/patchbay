//! Application audio streams — the "send Firefox to the Stems bus" path.
//!
//! Port links wire *devices*; they cannot move a running application's
//! audio from one sink to another. That is a `pulse` concept
//! (sink-inputs), and `pipewire-pulse` implements it, so the operation
//! is `pactl move-sink-input`.
//!
//! `pactl -f json` is used rather than the human format: the text output
//! is indentation-and-locale-sensitive, and a mis-parse here moves the
//! WRONG application's audio.

use std::process::Command;

use crate::proto::{AppStream, PatchbayError};
use serde_json::Value;

/// Parse `pactl -f json list sink-inputs`.
///
/// Streams `pipewire-pulse` created for our own monitoring taps are
/// excluded — a meter tap is not something a user should be able to
/// re-route.
#[must_use]
pub(crate) fn parse_sink_inputs(json: &str) -> Vec<AppStream> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let mut out: Vec<AppStream> = items
        .iter()
        .filter_map(parse_one)
        .filter(|s| !is_own_tap(s))
        .collect();
    out.sort_by(|a, b| (&a.app_name, a.index).cmp(&(&b.app_name, b.index)));
    out
}

/// A stream we created ourselves to measure levels.
fn is_own_tap(stream: &AppStream) -> bool {
    stream.app_name == "parec" || stream.binary == "parec"
}

fn prop<'a>(props: &'a Value, key: &str) -> Option<&'a str> {
    props.get(key).and_then(Value::as_str)
}

fn parse_one(item: &Value) -> Option<AppStream> {
    let index = u32::try_from(item.get("index").and_then(Value::as_u64)?).ok()?;
    let props = item.get("properties")?;
    // `sink` is the numeric index; `sink` may also appear as a string in
    // some pactl versions, so accept either.
    let sink = item.get("sink");
    let sink_index = sink
        .and_then(Value::as_u64)
        .or_else(|| sink.and_then(Value::as_str).and_then(|s| s.parse().ok()))
        .and_then(|n| u32::try_from(n).ok());

    let app_name = prop(props, "application.name")
        .or_else(|| prop(props, "media.name"))
        .or_else(|| prop(props, "node.name"))
        .unwrap_or("(unknown)")
        .to_owned();

    Some(AppStream {
        index,
        app_name,
        binary: prop(props, "application.process.binary")
            .unwrap_or_default()
            .to_owned(),
        media_name: prop(props, "media.name").unwrap_or_default().to_owned(),
        sink_index: sink_index.unwrap_or(u32::MAX),
        sink_name: item
            .get("sink_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        corked: item.get("corked").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// List the application streams `pipewire-pulse` currently knows about.
///
/// # Errors
/// If `pactl` can't be run or reports failure.
pub(crate) fn list() -> Result<Vec<AppStream>, PatchbayError> {
    let out = Command::new("pactl")
        .args(["-f", "json", "list", "sink-inputs"])
        .output()
        .map_err(|e| PatchbayError::Internal(format!("pactl spawn failed: {e}")))?;
    if !out.status.success() {
        return Err(PatchbayError::Internal(format!(
            "pactl list sink-inputs exited {}",
            out.status
        )));
    }
    Ok(parse_sink_inputs(&String::from_utf8_lossy(&out.stdout)))
}

/// Move one application stream to `sink` (a `node.name`).
///
/// # Errors
/// If `pactl` can't be run, or refuses the move (unknown stream or sink).
pub(crate) fn move_to_sink(index: u32, sink: &str) -> Result<(), PatchbayError> {
    let out = Command::new("pactl")
        .args(["move-sink-input", &index.to_string(), sink])
        .output()
        .map_err(|e| PatchbayError::Internal(format!("pactl spawn failed: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(PatchbayError::Internal(format!(
        "moving stream {index} to {sink} failed: {}",
        stderr.trim()
    )))
}
