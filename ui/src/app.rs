//! Root component: the rail (current device, Route, Mix — a tab bar on a
//! phone) + view outlet + status bar.
//!
//! Takes no props — the shell provides a [`crate::Shell`] via context, so
//! the same component mounts in the desktop app and the browser remote.

use dioxus::prelude::*;

use crate::devices::{self, KIND_DANTE, KIND_SYSTEM};
use crate::hosts;
use crate::icons::{ContextIcon, Mark, ViewIcon};
use crate::pages::{MixPage, RoutePage};
use crate::panels::StatusBar;
use crate::scenes::ScenesView;
use crate::settings::SettingsView;
use crate::state::{ARMED_OUTPUTS, VIEW, View};
use crate::ui::{Status, StatusDot};
use patchbay_proto::DeviceSummary;

#[component]
pub fn PatchbayApp() -> Element {
    // Above the live engine: connections to the others outlive a switch.
    crate::hosts::use_peers();
    crate::session::use_session();
    rsx! {
        crate::hosts::LiveEngine { Workspace {} }
    }
}

/// The whole window, mounted against whichever engine is live.
#[component]
fn Workspace() -> Element {
    let view = *VIEW.read();
    let handle = crate::state::use_patchbay();
    let look = crate::appearance::use_appearance();
    // The rail shows and switches the current device from every view.
    devices::use_device_poll();

    // Pre-scan the Dante network in the background right after launch,
    // so the Network view opens populated instead of blank-then-scanning.
    let prescan = handle.clone();
    use_future(move || {
        let handle = prescan.clone();
        async move {
            crate::state::sleep_secs(1).await;
            if crate::state::DANTE_DEVICES.peek().is_empty() && !*crate::state::DANTE_LOADING.peek()
            {
                crate::state::refresh_dante(&handle).await;
            }
        }
    });
    rsx! {
        document::Style { {crate::theme::CSS} }
        div {
            class: "patchbay-root",
            "data-theme": look.theme,
            "data-accent": look.accent,
            "data-density": look.density,
            tabindex: "0",
            onkeydown: move |e: Event<KeyboardData>| {
                if e.key() == Key::Escape {
                    ARMED_OUTPUTS.write().clear();
                    *crate::state::DRAG.write() = None;
                }
                // Ctrl+Z: undo the last link gesture.
                if e.modifiers().ctrl()
                    && matches!(e.key(), Key::Character(ref c) if c == "z" || c == "Z")
                {
                    crate::state::undo_last(handle.clone());
                }
            },
            div { class: "shell",
                Rail { current: view }
                div { class: "main-col",
                    div { class: "view-outlet",
                        match view {
                            View::Route => rsx! { RoutePage {} },
                            View::Mix => rsx! { MixPage {} },
                            View::Scenes => rsx! { ScenesView {} },
                            View::Settings => rsx! { SettingsView {} },
                        }
                    }
                    StatusBar {}
                }
            }
        }
    }
}

/// The rail: which device, then what to do to it; below, the two places
/// that aren't about any one device.
#[component]
fn Rail(current: View) -> Element {
    rsx! {
        nav { class: "rail",
            div { class: "rail-brand",
                Mark {}
                "Patchbay"
            }
            ContextSwitcher {}
            for view in View::ALL.iter().copied().filter(|v| !v.is_utility()) {
                RailItem { key: "{view.label()}", view, current }
            }
            div { class: "rail-spacer" }
            for view in View::ALL.iter().copied().filter(|v| v.is_utility()) {
                RailItem { key: "{view.label()}", view, current }
            }
        }
    }
}

/// The current device, and the list to pick another from — every device
/// on every engine this UI knows, grouped by machine.
///
/// First in the rail because it is the subject of the two items under
/// it. Its dot is the device's link state, so a console that dropped off
/// the network shows from whatever you were looking at.
#[component]
fn ContextSwitcher() -> Element {
    let current = devices::context();
    let live_devices = devices::contexts();
    let open = *devices::CONTEXT_MENU.read();
    let live = hosts::LIVE.read().clone();
    let peers = hosts::peers_in_order();
    let multi = hosts::multi_host();
    // Nothing to switch between: an engine without a device layer, alone.
    if live_devices.is_empty() && !multi {
        return rsx! {};
    }
    let kind = current
        .as_ref()
        .map_or(KIND_SYSTEM, |d| d.kind.as_str())
        .to_owned();
    let label = current
        .as_ref()
        .map_or_else(|| "Device".to_owned(), devices::context_label);
    let selected = current.as_ref().map(devices::device_key);
    // Home's devices: the live list when home is live, else the copy the
    // peer poll keeps.
    let home_devices = if live.is_none() {
        live_devices.clone()
    } else {
        hosts::HOME_DEVICES.read().clone()
    };
    let home_name = hosts::BOOK.read().this.clone();

    rsx! {
        button {
            class: if open { "rail-item rail-context open" } else { "rail-item rail-context" },
            title: "The device Route and Mix act on — click to switch",
            "aria-haspopup": "menu",
            "aria-expanded": "{open}",
            onclick: move |_| {
                let now = *devices::CONTEXT_MENU.peek();
                *devices::CONTEXT_MENU.write() = !now;
            },
            ContextIcon { kind }
            span { class: "rail-label", "{label}" }
            if multi {
                span { class: "rail-sub", "{hosts::live_name()}" }
            }
            if let Some(d) = current.as_ref() {
                span { class: "rail-context-dot",
                    StatusDot { status: devices::context_status(d), title: devices::context_detail(d) }
                }
            }
        }
        if open {
            button {
                class: "context-scrim",
                "aria-label": "close",
                onclick: move |_| *devices::CONTEXT_MENU.write() = false,
            }
            div { class: "context-menu", role: "menu",
                if multi {
                    HostHeading { name: home_name, note: "this engine".to_owned(), live: live.is_none(), status: Status::Ok }
                } else {
                    div { class: "context-menu-title", "Working on" }
                }
                for d in home_devices {
                    ContextOption {
                        key: "home/{d.name}",
                        host: None,
                        on: live.is_none() && selected.as_deref() == Some(devices::device_key(&d).as_str()),
                        device: d,
                    }
                }
                for peer in peers {
                    {
                        let is_live = live.as_ref() == Some(&peer.host.addr);
                        let (status, note) = if peer.link.is_some() {
                            (Status::Ok, peer.host.addr.clone())
                        } else if peer.error.is_empty() {
                            (Status::Busy, "connecting…".to_owned())
                        } else {
                            (Status::Bad, peer.error.clone())
                        };
                        // The live engine's own poll is fresher than the peer poll's.
                        let list = if is_live { live_devices.clone() } else { peer.devices.clone() };
                        rsx! {
                            HostHeading { key: "h/{peer.host.addr}", name: peer.host.name.clone(), note, live: is_live, status }
                            for d in list {
                                ContextOption {
                                    key: "{peer.host.addr}/{d.name}",
                                    host: Some(peer.host.addr.clone()),
                                    on: is_live && selected.as_deref() == Some(devices::device_key(&d).as_str()),
                                    device: d,
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A machine, above its devices.
#[component]
fn HostHeading(name: String, note: String, live: bool, status: Status) -> Element {
    rsx! {
        div { class: if live { "context-host live" } else { "context-host" },
            StatusDot { status, title: note.clone() }
            span { class: "context-host-name", "{name}" }
            span { class: "context-host-note", "{note}" }
        }
    }
}

/// One device on one engine (`host`: its address; `None` = home).
#[component]
fn ContextOption(host: Option<String>, device: DeviceSummary, on: bool) -> Element {
    let handle = crate::state::use_patchbay();
    let what = match device.kind.as_str() {
        KIND_SYSTEM => "this machine's audio".to_owned(),
        KIND_DANTE => "the Dante network".to_owned(),
        _ => device.name.clone(),
    };
    let pick = device.clone();
    rsx! {
        button {
            class: if on { "context-option on" } else { "context-option" },
            role: "menuitemradio",
            "aria-checked": "{on}",
            onclick: move |_| {
                if *hosts::LIVE.peek() == host {
                    devices::select_context(handle.clone(), &pick);
                } else {
                    // Another machine: its engine goes live, and lands on
                    // this device.
                    *devices::CONTEXT_MENU.write() = false;
                    hosts::go_live(host.clone(), Some(devices::device_key(&pick)));
                }
            },
            ContextIcon { kind: device.kind.clone() }
            span { class: "context-option-text",
                span { class: "context-option-name", "{devices::context_label(&device)}" }
                span { class: "context-option-note", "{what} · {devices::context_detail(&device)}" }
            }
            StatusDot { status: devices::context_status(&device), title: devices::context_detail(&device) }
        }
    }
}

#[component]
fn RailItem(view: View, current: View) -> Element {
    rsx! {
        button {
            class: if view == current { "rail-item on" } else { "rail-item" },
            title: "{view.hint()}",
            "aria-current": if view == current { "page" } else { "false" },
            onclick: move |_| *VIEW.write() = view,
            ViewIcon { view }
            span { class: "rail-label", "{view.label()}" }
        }
    }
}

/// What a shell shows before there is an engine to mount [`PatchbayApp`]
/// on — the browser remote while it dials, or after the link drops. It
/// wears the same stored theme, so a phone on Midnight doesn't get a
/// flash of another palette every time it wakes up.
#[component]
pub fn Splash(title: String, detail: String, failed: bool) -> Element {
    let look = crate::appearance::use_appearance();
    rsx! {
        document::Style { {crate::theme::CSS} }
        div {
            class: "patchbay-root",
            "data-theme": look.theme,
            "data-accent": look.accent,
            "data-density": look.density,
            div { class: if failed { "splash failed" } else { "splash waiting" },
                Mark { class: "splash-mark".to_owned() }
                div { class: "splash-title", "{title}" }
                if !detail.is_empty() {
                    div { class: "splash-detail", "{detail}" }
                }
            }
        }
    }
}
