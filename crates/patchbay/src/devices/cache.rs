//! Discovery cache: the last address each auto-discovered device was
//! found at, so the next start connects immediately instead of scanning.
//!
//! Lives beside the config as `<config stem>.state.json` (machine state,
//! not hand-edited settings — the styx config is never rewritten for
//! it). Best-effort: an unreadable or unwritable file only costs a
//! rescan.

use std::collections::BTreeMap;
use std::path::PathBuf;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    /// Config entry name → `host:port`.
    #[serde(default)]
    discovered: BTreeMap<String, String>,
    /// Config entry name → MAC (`aa:bb:cc:dd:ee:ff`): a stable identity
    /// that survives DHCP address changes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    macs: BTreeMap<String, String>,
    /// Config entry name → member name → `host:port`: the last known
    /// members of a network adapter (Dante), connected to directly on
    /// the next start instead of waiting out an mDNS browse.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    members: BTreeMap<String, BTreeMap<String, String>>,
}

pub(crate) struct DiscoveryCache {
    path: Option<PathBuf>,
    data: Mutex<StateFile>,
}

impl DiscoveryCache {
    /// Open the state file next to the config.
    pub(crate) fn open() -> Self {
        Self::at(Some(
            crate::presets::config_path().with_extension("state.json"),
        ))
    }

    /// In-memory only (tests / no config dir).
    #[cfg(test)]
    pub(crate) fn memory() -> Self {
        Self::at(None)
    }

    fn at(path: Option<PathBuf>) -> Self {
        let data = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            path,
            data: Mutex::new(data),
        }
    }

    pub(crate) fn get(&self, name: &str) -> Option<String> {
        self.data.lock().discovered.get(name).cloned()
    }

    pub(crate) fn get_mac(&self, name: &str) -> Option<String> {
        self.data.lock().macs.get(name).cloned()
    }

    /// Remember `addr` for `name` (no-op if unchanged).
    pub(crate) fn set(&self, name: &str, addr: &str) {
        let mut data = self.data.lock();
        if data.discovered.get(name).map(String::as_str) == Some(addr) {
            return;
        }
        data.discovered.insert(name.to_owned(), addr.to_owned());
        Self::persist(self.path.as_ref(), &data);
    }

    /// Remember the MAC of `name` (no-op if unchanged).
    pub(crate) fn set_mac(&self, name: &str, mac: &str) {
        let mut data = self.data.lock();
        if data.macs.get(name).map(String::as_str) == Some(mac) {
            return;
        }
        data.macs.insert(name.to_owned(), mac.to_owned());
        Self::persist(self.path.as_ref(), &data);
    }

    pub(crate) fn get_members(&self, name: &str) -> BTreeMap<String, String> {
        self.data
            .lock()
            .members
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    /// Remember the members of `name` (no-op if unchanged).
    /// Forget a device's cached address, so the next attempt discovers
    /// instead of retrying somewhere it isn't. The MAC is kept — it is
    /// how the console is followed across a DHCP change.
    pub(crate) fn forget_addr(&self, name: &str) {
        let mut data = self.data.lock();
        if data.discovered.remove(name).is_some() {
            Self::persist(self.path.as_ref(), &data);
        }
    }

    pub(crate) fn set_members(&self, name: &str, members: BTreeMap<String, String>) {
        let mut data = self.data.lock();
        if data.members.get(name) == Some(&members) {
            return;
        }
        data.members.insert(name.to_owned(), members);
        Self::persist(self.path.as_ref(), &data);
    }

    fn persist(path: Option<&PathBuf>, data: &StateFile) {
        let Some(path) = path else {
            return;
        };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("state.json.tmp");
            let json = serde_json::to_string_pretty(data).map_err(std::io::Error::other)?;
            std::fs::write(&tmp, json)?;
            std::fs::rename(&tmp, path)
        };
        if let Err(e) = write() {
            tracing::warn!(path = %path.display(), error = %e, "could not persist discovery cache");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_file() {
        let path = std::env::temp_dir().join(format!(
            "patchbay-cache-test-{}.state.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let c = DiscoveryCache::at(Some(path.clone()));
        assert_eq!(c.get("tf1"), None);
        c.set("tf1", "192.168.1.214:49280");
        let again = DiscoveryCache::at(Some(path.clone()));
        assert_eq!(again.get("tf1").as_deref(), Some("192.168.1.214:49280"));
        let _ = std::fs::remove_file(&path);
        let m = DiscoveryCache::memory();
        m.set("x", "1.2.3.4:1");
        assert_eq!(m.get("x").as_deref(), Some("1.2.3.4:1"));
        m.set_mac("x", "00:a0:de:5c:6d:1b");
        assert_eq!(m.get_mac("x").as_deref(), Some("00:a0:de:5c:6d:1b"));
        // Old state files (no `macs`) still load.
        let old: StateFile =
            serde_json::from_str(r#"{"discovered":{"tf1":"1.2.3.4:49280"}}"#).expect("old format");
        assert!(old.macs.is_empty());
        let members = BTreeMap::from([("Galaxy32".to_owned(), "10.10.10.118:4440".to_owned())]);
        m.set_members("dante", members.clone());
        assert_eq!(m.get_members("dante"), members);
        assert!(m.get_members("nope").is_empty());
    }
}
