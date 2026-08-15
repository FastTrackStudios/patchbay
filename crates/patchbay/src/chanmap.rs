//! REAPER ChanMap bridge.
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
pub fn default_path() -> PathBuf {
    let host = std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "default".to_string());
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!(
            ".fasttrackstudio/Reaper/ChanMaps/{host}.ReaperChanMap"
        ))
}

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
        if !name.is_empty() {
            names.insert(idx + 1, name.to_string());
        }
    }
    Ok(names)
}

/// Merge `channel (1-based) → name` into the chanmap's `nameN=` lines,
/// preserving everything else. Creates a minimal 128-channel identity
/// map when the file doesn't exist.
pub fn write_names(path: &str, names: &BTreeMap<u32, String>) -> Result<(), String> {
    let path = resolve_path(path);
    let existing = fs::read_to_string(&path).ok();
    let mut lines: Vec<String> = match &existing {
        Some(text) => text.lines().map(str::to_string).collect(),
        None => {
            let mut l = vec!["[reaper_chanmap]".to_string()];
            l.extend((0..128).map(|i| format!("ch{i}={i}")));
            l
        }
    };

    // Drop name lines we're about to rewrite, keep foreign ones.
    lines.retain(|line| {
        let Some(rest) = line.trim().strip_prefix("name") else {
            return true;
        };
        let Some((idx, _)) = rest.split_once('=') else {
            return true;
        };
        idx.parse::<u32>()
            .map(|i| !names.contains_key(&(i + 1)))
            .unwrap_or(true)
    });
    for (channel, name) in names {
        lines.push(format!("name{}={}", channel - 1, name));
    }

    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    fs::write(&path, lines.join("\n") + "\n").map_err(|e| format!("{}: {e}", path.display()))
}

/// `playback_97` → 97. Port names whose suffix isn't numeric don't
/// correspond to a chanmap channel.
pub fn channel_of_port(port_name: &str) -> Option<u32> {
    let digits = port_name
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .count();
    if digits == 0 || digits == port_name.len() {
        return None;
    }
    port_name[port_name.len() - digits..].parse().ok()
}
