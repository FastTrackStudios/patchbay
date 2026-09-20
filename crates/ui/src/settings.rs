//! Settings — the machinery that makes the rest of the app work, in one
//! place instead of stacked beside the graph.
//!
//! Most of it is platform-specific and only appears where it means
//! something: privacy grants and the virtual-device driver on macOS, the
//! `PipeWire` clock, the managed services and the per-app latency rules
//! on Linux. What is left — who can reach this Patchbay over the network
//! — matters everywhere.

use dioxus::prelude::*;
use patchbay_proto::{ListenAddress, PermissionsStatus};

use crate::state::{self, PatchbayHandle};
use crate::ui::{ErrorBar, Status, StatusDot};

/// Settings refresh, in seconds. Nothing here changes on its own except
/// a permission the user is granting in System Settings right now.
const POLL_SECS: u64 = 2;

static PERMISSIONS: GlobalSignal<PermissionsStatus> = Signal::global(PermissionsStatus::default);
static LISTEN: GlobalSignal<ListenAddress> = Signal::global(ListenAddress::default);
static DRIVER: GlobalSignal<patchbay_proto::VirtualDevicesStatus> =
    Signal::global(patchbay_proto::VirtualDevicesStatus::default);
static ERROR: GlobalSignal<String> = Signal::global(String::new);
/// Second click on "open to the network" — it exposes an unauthenticated
/// RPC, so it is not a one-click toggle.
static CONFIRM_LAN: GlobalSignal<bool> = Signal::global(|| false);

async fn refresh(handle: &PatchbayHandle) {
    let (perms, listen, driver) = futures_util::join!(
        handle.0.permissions(),
        handle.0.listen_address(),
        handle.0.virtual_devices(),
    );
    if let Ok(p) = perms
        && *PERMISSIONS.peek() != p
    {
        *PERMISSIONS.write() = p;
    }
    if let Ok(l) = listen
        && *LISTEN.peek() != l
    {
        *LISTEN.write() = l;
    }
    if let Ok(d) = driver
        && *DRIVER.peek() != d
    {
        *DRIVER.write() = d;
    }
}

/// A grant's state as a dot: granted, refused, or not asked yet.
fn grant_status(state: &str) -> Status {
    match state {
        "granted" => Status::Ok,
        "denied" | "restricted" => Status::Bad,
        "not_determined" | "undetermined" => Status::Idle,
        _ => Status::Missing,
    }
}

#[component]
pub fn SettingsView() -> Element {
    let handle = state::use_patchbay();
    let poll = handle;
    use_future(move || {
        let handle = poll.clone();
        async move {
            loop {
                refresh(&handle).await;
                state::sleep_secs(POLL_SECS).await;
            }
        }
    });

    let perms = PERMISSIONS.read().clone();
    let listen = LISTEN.read().clone();
    let driver = DRIVER.read().clone();
    let error = ERROR.read().clone();
    let macos = perms.platform == "macos";
    // The graph clock only exists where there is a graph.
    let has_graph = state::has_graph();

    rsx! {
        div { class: "view-head",
            span { class: "view-title", "Settings" }
            span { class: "view-sub", "how this Patchbay is allowed to work" }
        }
        ErrorBar { message: error, on_dismiss: move |()| ERROR.write().clear() }
        div { class: "view-body settings-body",
            Network { listen }
            if macos {
                Permissions { perms }
                Driver { driver }
            }
            if has_graph {
                section { class: "settings-section",
                    h3 { class: "section-label", "Graph clock" }
                    crate::panels::ClockDefaultsEditor {}
                }
                section { class: "settings-section",
                    crate::panels::ServicesPanel {}
                }
                section { class: "settings-section",
                    crate::panels::LatencyPanel {}
                }
            }
        }
    }
}

/// Save a listen address; it takes effect on the next start.
fn set_bind(handle: PatchbayHandle, bind: &'static str) {
    *CONFIRM_LAN.write() = false;
    dioxus::core::spawn_forever(async move {
        match handle.0.set_listen_address(bind.to_owned()).await {
            Ok(l) => *LISTEN.write() = l,
            Err(e) => *ERROR.write() = format!("listen address: {e}"),
        }
    });
}

/// Who can reach this Patchbay.
#[component]
fn Network(listen: ListenAddress) -> Element {
    let handle = state::use_patchbay();
    let armed = *CONFIRM_LAN.read();
    let pending = !listen.configured.is_empty() && listen.configured != listen.current;
    // One per button: a closure that writes to a signal borrows it
    // mutably, so it can't be handed to two handlers.
    let open_handle = handle.clone();
    let close_handle = handle;

    rsx! {
        section { class: "settings-section",
            h3 { class: "section-label", "Network" }
            div { class: "setting-row",
                StatusDot {
                    status: if listen.lan { Status::Busy } else { Status::Ok },
                    title: if listen.lan { "reachable from the network" } else { "this machine only" },
                }
                div { class: "setting-text",
                    span { class: "setting-title",
                        if listen.lan { "Open to the network" } else { "This machine only" }
                    }
                    span { class: "dim-note", "listening on {listen.current}" }
                }
                if listen.lan {
                    button {
                        class: "chip",
                        onclick: move |_| set_bind(close_handle.clone(), "127.0.0.1:4046"),
                        "close it"
                    }
                } else if armed {
                    button {
                        class: "chip danger armed",
                        onclick: move |_| set_bind(open_handle.clone(), "0.0.0.0:4046"),
                        "yes, open it"
                    }
                    button {
                        class: "chip",
                        onclick: move |_| *CONFIRM_LAN.write() = false,
                        "cancel"
                    }
                } else {
                    button {
                        class: "chip",
                        onclick: move |_| *CONFIRM_LAN.write() = true,
                        "open to the network"
                    }
                }
            }
            if pending {
                p { class: "dim-note",
                    "Will listen on {listen.configured} next time Patchbay starts — rebinding now "
                    "would drop every connected remote."
                }
            }
            if armed {
                p { class: "setting-warning",
                    "The RPC has no authentication. Anything that can reach this port can "
                    "re-route this machine's audio and write to the consoles Patchbay is "
                    "connected to. Only do this on a network you control."
                }
            }
            if listen.lan && !listen.urls.is_empty() {
                div { class: "setting-urls",
                    span { class: "dim-note", "open from another device:" }
                    for u in listen.urls.iter() {
                        code { key: "{u}", class: "setting-url", "{u}" }
                    }
                }
            }
            if !listen.web_note.is_empty() {
                p { class: "dim-note", "{listen.web_note}" }
            }
        }
    }
}

/// The macOS privacy grants Patchbay depends on.
#[component]
fn Permissions(perms: PermissionsStatus) -> Element {
    let handle = state::use_patchbay();
    let rows = [
        (
            "System Audio Recording",
            perms.system_audio_recording.clone(),
            "Without it process taps are created but deliver silence — app meters read zero \
             and mixes carry nothing.",
        ),
        (
            "Microphone",
            perms.microphone.clone(),
            "Needed to take audio from an input device, including an interface's own inputs.",
        ),
        (
            "Local Network",
            perms.local_network.clone(),
            "Needed to find the Yamaha TF, Dante devices and the Antelope Manager Server.",
        ),
    ];
    rsx! {
        section { class: "settings-section",
            h3 { class: "section-label", "Privacy" }
            for (name, state, why) in rows {
                div { class: "setting-row", key: "{name}",
                    StatusDot { status: grant_status(&state), title: "{state}" }
                    div { class: "setting-text",
                        span { class: "setting-title", "{name}" }
                        span { class: "dim-note", "{why}" }
                    }
                    span { class: "mix-badge", "{state}" }
                }
            }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { class: "dim-note", "{perms.note}" }
                }
                button {
                    class: "chip",
                    disabled: perms.requesting,
                    onclick: move |_| {
                        let handle = handle.clone();
                        dioxus::core::spawn_forever(async move {
                            match handle.0.request_permissions().await {
                                Ok(p) => *PERMISSIONS.write() = p,
                                Err(e) => *ERROR.write() = format!("permissions: {e}"),
                            }
                        });
                    },
                    if perms.requesting { "asking…" } else { "ask again" }
                }
            }
        }
    }
}

/// `Patchbay.driver` — the loopback devices apps can play into.
#[component]
fn Driver(driver: patchbay_proto::VirtualDevicesStatus) -> Element {
    rsx! {
        section { class: "settings-section",
            h3 { class: "section-label", "Virtual device driver" }
            div { class: "setting-row",
                StatusDot {
                    status: if driver.driver_loaded { Status::Ok } else { Status::Bad },
                    title: if driver.driver_loaded { "loaded" } else { "not installed" },
                }
                div { class: "setting-text",
                    span { class: "setting-title",
                        if driver.driver_loaded { "Loaded" } else { "Not installed" }
                    }
                    span { class: "dim-note",
                        if driver.driver_loaded {
                            "Publishing {driver.devices.len()} device(s). Create and remove them in Now."
                        } else {
                            "Run packaging/macos/install-driver.sh. Without it there are no Patchbay \
                             devices for apps to play into, and mixes have nothing to be heard through."
                        }
                    }
                }
            }
            for d in driver.devices.iter() {
                div { class: "setting-row", key: "{d.uid}",
                    span { class: "svc-dot on" }
                    div { class: "setting-text",
                        span { class: "setting-title", "{d.name}" }
                        span { class: "dim-note", "{d.channels} channels · {d.uid}" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_reads_as_its_state_not_as_a_guess() {
        assert!(matches!(grant_status("granted"), Status::Ok));
        assert!(matches!(grant_status("denied"), Status::Bad));
        assert!(matches!(grant_status("restricted"), Status::Bad));
        // Not asked yet is not the same as refused.
        assert!(matches!(grant_status("not_determined"), Status::Idle));
        // Anything we don't recognise is absent, not granted.
        assert!(matches!(grant_status("n/a"), Status::Missing));
    }
}
