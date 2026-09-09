//! Capturable bus sources — the OBS integration.
//!
//! A virtual sink already has a `.monitor`, but recorders treat monitors
//! as second-class: OBS either hides them or buries them as "Monitor of
//! `patchbay.stems_bus`" among every other monitor on the box. What you
//! actually want in the Audio Input Capture list is a device called
//! "Stems Bus".
//!
//! `module-virtual-source` provides exactly that — a real source node
//! fed from the sink's monitor, carrying its own description. This is a
//! `pulse` module rather than a `PipeWire` factory object, so it goes
//! through `pactl`, the same "express intent, shell out, best-effort"
//! contract as the clock and Dante helpers.
//!
//! Parsing and planning are pure and tested; only [`ensure`] and
//! [`remove`] run a process.

use std::collections::HashSet;
use std::process::Command;

use patchbay_proto::{VirtualSink, capture_source_name, sink_node_name};

/// `pactl` prefixes a virtual source's node name with `output.`, so the
/// name we ask for is not the name we find when listing.
fn listed_name(source_name: &str) -> String {
    format!("output.{source_name}")
}

/// Source names present, from `pactl list short sources`.
///
/// The format is tab-separated `index<TAB>name<TAB>driver…`; anything
/// that doesn't have a name column is skipped rather than guessed at.
#[must_use]
pub(crate) fn parse_sources(listing: &str) -> HashSet<String> {
    listing
        .lines()
        .filter_map(|line| line.split('\t').nth(1))
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

/// Which capture sources are missing for `sinks`.
///
/// Returns `(source_name, description)` pairs to create, in config
/// order. A sink that isn't `capturable` is skipped; one whose source
/// already exists is skipped, so this is idempotent.
#[must_use]
pub(crate) fn plan(existing: &HashSet<String>, sinks: &[VirtualSink]) -> Vec<(String, String)> {
    sinks
        .iter()
        .filter(|s| s.capturable)
        .filter_map(|sink| {
            let source = capture_source_name(&sink_node_name(&sink.name));
            if existing.contains(&listed_name(&source)) || existing.contains(&source) {
                return None;
            }
            Some((source, sink.name.clone()))
        })
        .collect()
}

/// Escape a value for a `pactl` property assignment.
///
/// A bus is named by the user, so it can contain a quote or a space;
/// unescaped, that either breaks the module load or silently truncates
/// the description.
#[must_use]
pub(crate) fn escape_prop(value: &str) -> String {
    let escaped = value.replace('\\', r"\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Read the current source list.
fn list_sources() -> HashSet<String> {
    Command::new("pactl")
        .args(["list", "short", "sources"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| parse_sources(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// Create any missing capture source for `sinks`. Returns how many were
/// created.
///
/// Best-effort: a host without `pactl` (or without `pipewire-pulse`)
/// simply has no capture sources, and the buses still work.
pub(crate) fn ensure(sinks: &[VirtualSink]) -> u32 {
    let wanted = plan(&list_sources(), sinks);
    let mut created = 0_u32;
    for (source, description) in wanted {
        let props = format!("device.description={}", escape_prop(&description));
        let result = Command::new("pactl")
            .args([
                "load-module",
                "module-virtual-source",
                &format!("source_name={source}"),
                // Fed from the bus's monitor — this is what makes it
                // carry the bus's audio.
                &format!("master={}.monitor", source.trim_end_matches("-src")),
                &format!("source_properties={props}"),
            ])
            .output();
        match result {
            Ok(out) if out.status.success() => created = created.saturating_add(1),
            Ok(out) => tracing::warn!(
                source,
                "capture source create failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => tracing::warn!(source, "pactl spawn failed: {e}"),
        }
    }
    created
}

/// Extract the module id owning `source_name` from
/// `pactl list short modules`.
///
/// Lines look like `42<TAB>module-virtual-source<TAB>source_name=x …`.
#[must_use]
pub(crate) fn module_id_for(listing: &str, source_name: &str) -> Option<String> {
    let needle = format!("source_name={source_name}");
    listing
        .lines()
        .find(|line| line.contains("module-virtual-source") && line.contains(&needle))
        .and_then(|line| line.split('\t').next())
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
}

/// Tear down the capture source for a bus that is being removed.
pub(crate) fn remove(sink_display_name: &str) {
    let source = capture_source_name(&sink_node_name(sink_display_name));
    let Ok(out) = Command::new("pactl")
        .args(["list", "short", "modules"])
        .output()
    else {
        return;
    };
    let Some(id) = module_id_for(&String::from_utf8_lossy(&out.stdout), &source) else {
        return;
    };
    if let Err(e) = Command::new("pactl").args(["unload-module", &id]).status() {
        tracing::warn!(source, "unloading capture source failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(name: &str, capturable: bool) -> VirtualSink {
        VirtualSink {
            name: name.to_owned(),
            channels: 2,
            capturable,
        }
    }

    #[test]
    fn parses_the_short_source_listing() {
        let listing = "0\talsa_output.pci.monitor\tPipeWire\ts16le 2ch 48000Hz\n\
                       1\toutput.patchbay.stems_bus-src\tPipeWire\ts16le 2ch 48000Hz\n";
        let sources = parse_sources(listing);
        assert!(sources.contains("output.patchbay.stems_bus-src"));
        assert!(sources.contains("alsa_output.pci.monitor"));
        assert_eq!(sources.len(), 2);
    }

    #[test]
    fn malformed_listing_lines_are_skipped() {
        assert!(parse_sources("").is_empty());
        assert!(parse_sources("no-tabs-here\n\n").is_empty());
    }

    #[test]
    fn plans_a_source_for_a_capturable_bus() {
        let plan = plan(&HashSet::new(), &[sink("Stems Bus", true)]);
        assert_eq!(
            plan,
            vec![("patchbay.stems_bus-src".to_owned(), "Stems Bus".to_owned())]
        );
    }

    #[test]
    fn a_non_capturable_bus_gets_nothing() {
        assert!(plan(&HashSet::new(), &[sink("Stems Bus", false)]).is_empty());
    }

    /// Runs on every settle, so an existing source must not be
    /// re-created — that would stack duplicate modules on every restart.
    #[test]
    fn is_idempotent_against_the_listed_name() {
        let existing: HashSet<String> = ["output.patchbay.stems_bus-src".to_owned()].into();
        assert!(plan(&existing, &[sink("Stems Bus", true)]).is_empty());
    }

    /// Some `pactl` versions list the bare name without the `output.`
    /// prefix; both must count as present.
    #[test]
    fn is_idempotent_against_the_bare_name_too() {
        let existing: HashSet<String> = ["patchbay.stems_bus-src".to_owned()].into();
        assert!(plan(&existing, &[sink("Stems Bus", true)]).is_empty());
    }

    #[test]
    fn plans_only_the_missing_half_of_a_mixed_set() {
        let existing: HashSet<String> = ["output.patchbay.stems_bus-src".to_owned()].into();
        let out = plan(&existing, &[sink("Stems Bus", true), sink("Cue Bus", true)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, "Cue Bus");
    }

    /// A bus name is user input and ends up in a shell property.
    #[test]
    fn descriptions_with_quotes_are_escaped() {
        assert_eq!(escape_prop(r#"My "Big" Bus"#), r#""My \"Big\" Bus""#);
        assert_eq!(escape_prop(r"back\slash"), r#""back\\slash""#);
        assert_eq!(escape_prop("Stems Bus"), r#""Stems Bus""#);
    }

    #[test]
    fn finds_the_module_owning_a_source() {
        let listing = "12\tmodule-null-sink\tsink_name=patchbay.stems_bus\n\
                       42\tmodule-virtual-source\tsource_name=patchbay.stems_bus-src master=…\n";
        assert_eq!(
            module_id_for(listing, "patchbay.stems_bus-src"),
            Some("42".to_owned())
        );
        assert_eq!(module_id_for(listing, "patchbay.other-src"), None);
    }

    /// Unloading the wrong module would silently kill someone else's
    /// audio, so a near-miss must not match.
    #[test]
    fn does_not_match_a_different_module_type() {
        let listing = "42\tmodule-remap-source\tsource_name=patchbay.stems_bus-src\n";
        assert_eq!(module_id_for(listing, "patchbay.stems_bus-src"), None);
    }
}
