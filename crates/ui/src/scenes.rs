//! Scenes — everything Patchbay can save and put back, in one place.
//!
//! The rig's state lives in four different places, each with its own
//! store and its own restore rules, and they were each buried in the
//! view that happened to own them:
//!
//! - **routing presets** — `PipeWire` links, by name
//! - **device snapshots** — a console's writable params and crosspoints
//! - **the Dante snapshot** — every reachable device's subscriptions
//! - **mixes** — saved and supervised continuously, so they need no
//!   restore button; they are listed here for completeness and edited
//!   in Mixes.
//!
//! Restores that write to hardware are never automatic and never
//! one-click: a device snapshot is diffed first and shows exactly what
//! it would change.

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use patchbay_proto::{DanteDeviceConfig, DeviceSnapshotInfo, MixView};

use crate::state::{self, PatchbayHandle};
use crate::ui::{ConfirmButton, ErrorBar};

static SNAPSHOTS: GlobalSignal<Vec<DeviceSnapshotInfo>> = Signal::global(Vec::new);
static DANTE_SAVED: GlobalSignal<Vec<DanteDeviceConfig>> = Signal::global(Vec::new);
static MIXES: GlobalSignal<Vec<MixView>> = Signal::global(Vec::new);
static ERROR: GlobalSignal<String> = Signal::global(String::new);
/// Last thing that happened, for the line under the buttons.
static NOTE: GlobalSignal<String> = Signal::global(String::new);
/// A hardware write is running — the Dante buttons take seconds.
static BUSY: GlobalSignal<bool> = Signal::global(|| false);

async fn refresh(handle: &PatchbayHandle) {
    let (snaps, dante, mixes) = futures_util::join!(
        handle.0.list_device_snapshots(),
        handle.0.dante_config(),
        handle.0.list_mixes(),
    );
    if let Ok(s) = snaps
        && *SNAPSHOTS.peek() != s
    {
        *SNAPSHOTS.write() = s;
    }
    if let Ok(d) = dante
        && *DANTE_SAVED.peek() != d
    {
        *DANTE_SAVED.write() = d;
    }
    if let Ok(m) = mixes
        && *MIXES.peek() != m
    {
        *MIXES.write() = m;
    }
}

/// When a snapshot was taken, in words.
fn taken_ago(created: u64, now: u64) -> String {
    let secs = now.saturating_sub(created);
    if created == 0 {
        return String::new();
    }
    let mins = secs / 60;
    let hours = mins / 60;
    let days = hours / 24;
    if days > 0 {
        format!("{days}d ago")
    } else if hours > 0 {
        format!("{hours}h ago")
    } else if mins > 0 {
        format!("{mins}m ago")
    } else {
        "just now".to_owned()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[component]
pub fn ScenesView() -> Element {
    let handle = state::use_patchbay();
    let load = handle;
    use_future(move || {
        let handle = load.clone();
        async move {
            loop {
                refresh(&handle).await;
                state::sleep_secs(5).await;
            }
        }
    });

    let error = ERROR.read().clone();
    let note = NOTE.read().clone();

    rsx! {
        div { class: "view-head",
            span { class: "view-title", "Scenes" }
            span { class: "view-sub", "what Patchbay can save and put back" }
        }
        ErrorBar { message: error, on_dismiss: move |()| ERROR.write().clear() }
        div { class: "view-body scenes-body",
            if !note.is_empty() {
                p { class: "scenes-note", "{note}" }
            }
            DeviceSnapshots {}
            DanteSnapshot {}
            section { class: "settings-section",
                h3 { class: "section-label", "Routing presets" }
                p { class: "dim-note",
                    "Saved sets of graph connections, matched by name so they survive the "
                    "devices being renumbered."
                }
                crate::panels::PresetsPanel {}
            }
            MixList {}
        }
    }
}

/// Saved console states: diff first, then write only the differences.
#[component]
fn DeviceSnapshots() -> Element {
    let handle = state::use_patchbay();
    let snapshots = SNAPSHOTS.read().clone();
    let now = now_secs();
    rsx! {
        section { class: "settings-section",
            h3 { class: "section-label", "Device snapshots" }
            p { class: "dim-note",
                "A console's writable parameters and crosspoints, saved by path. Restoring "
                "reads the device first and writes only what differs — preview it on the "
                "device's own page, where the planned changes are listed before anything is "
                "sent."
            }
            if snapshots.is_empty() {
                p { class: "dim-note",
                    "None yet. Take one from a device's Inspector, or with "
                    "`patchbay device snapshot save <device> <name>`."
                }
            }
            for s in snapshots {
                {
                    let handle = handle.clone();
                    let name = s.name.clone();
                    let ago = taken_ago(s.created, now);
                    rsx! {
                        div { class: "setting-row", key: "{s.name}",
                            div { class: "setting-text",
                                span { class: "setting-title", "{s.name}" }
                                span { class: "dim-note",
                                    "{s.device} · {s.params} params · {s.routes} crosspoints"
                                    if !ago.is_empty() { " · {ago}" }
                                }
                            }
                            ConfirmButton {
                                label: "delete".to_owned(),
                                armed_label: "sure?".to_owned(),
                                on_confirm: move |()| {
                                    let handle = handle.clone();
                                    let name = name.clone();
                                    spawn_forever(async move {
                                        match handle.0.delete_device_snapshot(name.clone()).await {
                                            Ok(()) => {
                                                *NOTE.write() = format!("deleted snapshot '{name}'");
                                                refresh(&handle).await;
                                            }
                                            Err(e) => *ERROR.write() = format!("delete '{name}': {e}"),
                                        }
                                    });
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The Dante network's subscriptions, saved and re-applied over ARC.
#[component]
fn DanteSnapshot() -> Element {
    let handle = state::use_patchbay();
    let saved = DANTE_SAVED.read().clone();
    let busy = *BUSY.read();
    let subs: usize = saved.iter().map(|d| d.subscriptions.len()).sum();
    let summary = if saved.is_empty() {
        "Nothing saved".to_owned()
    } else {
        format!("{} device(s) · {subs} subscription(s)", saved.len())
    };

    let save = {
        let handle = handle.clone();
        move |_| {
            let handle = handle.clone();
            *BUSY.write() = true;
            spawn_forever(async move {
                match handle.0.save_dante_config().await {
                    Ok(n) => *NOTE.write() = format!("saved {n} Dante device(s)"),
                    Err(e) => *ERROR.write() = format!("save Dante config: {e}"),
                }
                *BUSY.write() = false;
                refresh(&handle).await;
            });
        }
    };

    rsx! {
        section { class: "settings-section",
            h3 { class: "section-label", "Dante network" }
            p { class: "dim-note",
                "Every reachable device's channel names and subscriptions. Applying writes to "
                "the hardware over ARC and is non-destructive: it sets what differs and never "
                "clears a subscription the snapshot doesn't mention."
            }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { class: "setting-title", "{summary}" }
                    span { class: "dim-note", "Scanning the network takes a few seconds." }
                }
                button { class: "chip", disabled: busy, onclick: save,
                    if busy { "working…" } else { "save from network" }
                }
                if !saved.is_empty() {
                    ConfirmButton {
                        label: "apply to network".to_owned(),
                        armed_label: "write to hardware?".to_owned(),
                        on_confirm: move |()| {
                            let handle = handle.clone();
                            *BUSY.write() = true;
                            spawn_forever(async move {
                                match handle.0.apply_dante_config().await {
                                    Ok(n) => *NOTE.write() = format!("re-applied {n} subscription(s)"),
                                    Err(e) => *ERROR.write() = format!("apply Dante config: {e}"),
                                }
                                *BUSY.write() = false;
                                refresh(&handle).await;
                            });
                        },
                    }
                }
            }
        }
    }
}

/// Mixes are saved continuously; they are here so "what is saved" is a
/// complete answer, not so they can be restored.
#[component]
fn MixList() -> Element {
    let mixes = MIXES.read().clone();
    if mixes.is_empty() {
        return rsx! {};
    }
    rsx! {
        section { class: "settings-section",
            h3 { class: "section-label", "Mixes" }
            p { class: "dim-note",
                "Saved as you edit them and kept running by the supervisor — nothing to "
                "restore. Edit them in Mixes."
            }
            for m in mixes {
                div { class: "setting-row", key: "{m.config.name}",
                    div { class: "setting-text",
                        span { class: "setting-title", "{m.config.name}" }
                        span { class: "dim-note",
                            "{m.config.sources.len()} source(s) → {m.config.outputs.len()} output(s)"
                        }
                    }
                    span {
                        class: if m.running { "mix-badge live" } else { "mix-badge off" },
                        if m.running { "running" } else { "stopped" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::taken_ago;

    #[test]
    fn snapshot_ages_read_as_time_passed() {
        let now = 1_000_000;
        assert_eq!(taken_ago(now, now), "just now");
        assert_eq!(taken_ago(now - 90, now), "1m ago");
        assert_eq!(taken_ago(now - 7_200, now), "2h ago");
        assert_eq!(taken_ago(now - 172_800, now), "2d ago");
        // No timestamp is better said with nothing than with "56y ago".
        assert_eq!(taken_ago(0, now), "");
        // A clock that went backwards doesn't produce a negative age.
        assert_eq!(taken_ago(now + 60, now), "just now");
    }
}
