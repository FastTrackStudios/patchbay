//! REAPER `ChanMap` bridge.
//!
//! The chanmap (`~/.fasttrackstudio/Reaper/ChanMaps/<host>.ReaperChanMap`)
//! is how channel names reach REAPER's I/O pickers today — and
//! `set_dante_channel_names.py` pushes the same names to Inferno over
//! ARC. Patchbay aliases sync with it both ways, so "channel 23 is
//! Guitar" is one fact everywhere:
//!
//! ```text
//! [reaper_chanmap]
//! ch0=0            # channel remap (untouched here)
//! …
//! name0=1 - Kick In   # 0-based name index → channel 1
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// The host's default chanmap path.
#[must_use]
pub fn default_path() -> PathBuf {
    let host = std::fs::read_to_string("/etc/hostname")
        .map_or_else(|_| "default".to_owned(), |s| s.trim().to_owned());
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!(
            ".fasttrackstudio/Reaper/ChanMaps/{host}.ReaperChanMap"
        ))
}

#[must_use]
pub fn resolve_path(path: &str) -> PathBuf {
    if path.trim().is_empty() {
        default_path()
    } else if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(rest)
    } else {
        PathBuf::from(path)
    }
}

/// `channel number (1-based) → name` from the chanmap's `nameN=` lines.
///
/// # Errors
/// If the chanmap file can't be read.
pub fn read_names(path: &str) -> Result<BTreeMap<u32, String>, String> {
    let path = resolve_path(path);
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut names = BTreeMap::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("name") else {
            continue;
        };
        let Some((idx, name)) = rest.split_once('=') else {
            continue;
        };
        let Ok(idx) = idx.parse::<u32>() else {
            continue;
        };
        let name = name.trim();
        // `nameN=` is 0-based; channels are 1-based. A file claiming
        // name4294967295= must not wrap around to channel 0.
        if let Some(channel) = idx.checked_add(1)
            && !name.is_empty()
        {
            names.insert(channel, name.to_owned());
        }
    }
    Ok(names)
}

/// Merge `channel (1-based) → name` into the chanmap's `nameN=` lines,
/// preserving everything else. Creates a minimal 128-channel identity
/// map when the file doesn't exist.
///
/// # Errors
/// If the chanmap file can't be written.
pub fn write_names(path: &str, names: &BTreeMap<u32, String>) -> Result<(), String> {
    let path = resolve_path(path);
    let existing = fs::read_to_string(&path).ok();
    let mut lines: Vec<String> = existing.as_ref().map_or_else(
        || {
            // No file yet: a minimal 128-channel identity map.
            let mut l = vec!["[reaper_chanmap]".to_owned()];
            l.extend((0..128).map(|i| format!("ch{i}={i}")));
            l
        },
        |text| text.lines().map(str::to_owned).collect(),
    );

    // Drop name lines we're about to rewrite, keep foreign ones.
    lines.retain(|line| {
        let Some(rest) = line.trim().strip_prefix("name") else {
            return true;
        };
        let Some((idx, _)) = rest.split_once('=') else {
            return true;
        };
        idx.parse::<u32>()
            .ok()
            .and_then(|i| i.checked_add(1))
            .is_none_or(|channel| !names.contains_key(&channel))
    });
    for (channel, name) in names {
        // Channels are 1-based, `nameN=` is 0-based. Channel 0 isn't a
        // real channel; skip rather than underflow.
        if let Some(idx) = channel.checked_sub(1) {
            lines.push(format!("name{idx}={name}"));
        }
    }

    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    fs::write(&path, lines.join("\n") + "\n").map_err(|e| format!("{}: {e}", path.display()))
}

/// `playback_97` → 97. Port names whose suffix isn't numeric don't
/// correspond to a chanmap channel.
///
/// Re-exported from the wire crate so the engine, the CLI and every UI
/// agree on channel identity — this used to be five near-identical
/// copies across three crates.
pub use patchbay_proto::channel_of_port;
