//! Side panel: presets, inspector (aliases), clock + Dante controls.

use dioxus::prelude::*;
use patchbay_proto::MediaKind;

use crate::state::{
    self, ALIASES, CLOCK, DANTE, GRAPH, HIDE_UNCONNECTED, LAST_REPORT, MEDIA_TAB, PRESETS, SEARCH,
    SELECTED_NODE,
};

#[component]
pub fn Toolbar() -> Element {
    let search = SEARCH.read().clone();
    let tab = *MEDIA_TAB.read();
    let hide = *HIDE_UNCONNECTED.read();

    let media_tab = |kind: MediaKind, label: &'static str| {
        rsx! {
            button {
                class: if tab == kind { "tab on" } else { "tab" },
                onclick: move |_| *MEDIA_TAB.write() = kind,
                "{label}"
            }
        }
    };

    rsx! {
        div { class: "toolbar",
            div { class: "view-tabs",
                {media_tab(MediaKind::Audio, "Audio")}
                {media_tab(MediaKind::Midi, "MIDI")}
                {media_tab(MediaKind::Video, "Video")}
            }
            input {
                class: "search",
                placeholder: "filter nodes…",
                value: "{search}",
                oninput: move |e| *SEARCH.write() = e.value(),
            }
            button {
                class: if hide { "chip on" } else { "chip" },
                onclick: move |_| {
                    let cur = *HIDE_UNCONNECTED.peek();
                    *HIDE_UNCONNECTED.write() = !cur;
                },
                "connected only"
            }
            button {
                class: if *state::HIDE_MONITORS.read() { "chip on" } else { "chip" },
                title: "hide sinks' monitor taps (dimmed rows)",
                onclick: move |_| {
                    let cur = *state::HIDE_MONITORS.peek();
                    *state::HIDE_MONITORS.write() = !cur;
                },
                "hide monitors"
            }
        }
    }
}

#[component]
pub fn StatusBar() -> Element {
    let clock = CLOCK.read().clone();
    let dante = DANTE.read().clone();
    let handle = state::use_patchbay();
    let ms = if clock.rate > 0 {
        clock.quantum as f64 / clock.rate as f64 * 1000.0
    } else {
        0.0
    };
    let forced = clock.force_quantum != 0;

    let quantum_btn = |frames: u32| {
        let handle = handle.clone();
        let active = if frames == 0 {
            !forced
        } else {
            clock.force_quantum == frames
        };
        let label = if frames == 0 {
            "auto".to_string()
        } else {
            frames.to_string()
        };
        rsx! {
            button {
                class: if active { "chip on" } else { "chip" },
                onclick: move |_| {
                    let handle = handle.clone();
                    spawn(async move {
                        if let Err(e) = handle.0.force_quantum(frames).await {
                            tracing::warn!("force_quantum failed: {e:?}");
                        }
                        state::refresh_meta(&handle).await;
                    });
                },
                "{label}"
            }
        }
    };

    let dante_handle = handle.clone();
    let dante_on = dante.active;

    rsx! {
        div { class: "statusbar",
            span { class: "clock-info",
                "{clock.rate} Hz · {clock.quantum} frames · {ms:.2} ms"
                if forced { span { class: "forced-tag", " (forced)" } }
            }
            span { class: "spacer" }
            span { class: "label", "quantum:" }
            {quantum_btn(0)}
            {quantum_btn(32)}
            {quantum_btn(64)}
            {quantum_btn(128)}
            {quantum_btn(256)}
            {quantum_btn(512)}
            {quantum_btn(1024)}
            {quantum_btn(2048)}
            span { class: "spacer" }
            if dante.installed {
                span {
                    class: if dante_on { "dante-dot on" } else { "dante-dot" },
                }
                span { class: "label", "Dante" }
                button {
                    class: "chip",
                    onclick: move |_| {
                        let handle = dante_handle.clone();
                        spawn(async move {
                            if let Err(e) = handle.0.set_dante(!dante_on).await {
                                tracing::warn!("dante toggle failed: {e:?}");
                            }
                            state::refresh_meta(&handle).await;
                        });
                    },
                    if dante_on { "stop" } else { "start" }
                }
            }
        }
    }
}

#[component]
pub fn SidePanel() -> Element {
    rsx! {
        div { class: "side-panel",
            ViewsPanel {}
            ServicesPanel {}
            LatencyPanel {}
            PresetsPanel {}
            VirtualSinksPanel {}
            Inspector {}
        }
    }
}

/// Saved canvas views: pan/zoom/column-collapse under a name ("FOH",
/// "Broadcast") — click to jump, persisted server-side for all clients.
#[component]
fn ViewsPanel() -> Element {
    let handle = state::use_patchbay();
    let views = state::VIEWS.read().clone();
    let mut new_name = use_signal(String::new);

    let save = {
        let handle = handle.clone();
        move |_| {
            let name = new_name.peek().trim().to_string();
            if name.is_empty() {
                return;
            }
            let view = state::capture_view(name);
            let handle = handle.clone();
            spawn(async move {
                if let Err(e) = handle.0.save_view(view).await {
                    tracing::warn!("save_view failed: {e:?}");
                }
                state::refresh_meta(&handle).await;
            });
            new_name.set(String::new());
        }
    };

    rsx! {
        div { class: "panel-section",
            h3 { "Views" }
            div { class: "preset-save",
                input {
                    placeholder: "view name…",
                    value: "{new_name}",
                    oninput: move |e| new_name.set(e.value()),
                }
                button { class: "chip", title: "save the current pan/zoom/columns as a view",
                    onclick: save, "save view" }
            }
            for view in views {
                {
                    let apply_view = view.clone();
                    let del = {
                        let handle = handle.clone();
                        let name = view.name.clone();
                        move |_| {
                            let handle = handle.clone();
                            let name = name.clone();
                            spawn(async move {
                                if let Err(e) = handle.0.delete_view(name).await {
                                    tracing::warn!("delete_view failed: {e:?}");
                                }
                                state::refresh_meta(&handle).await;
                            });
                        }
                    };
                    rsx! {
                        div { class: "preset-row", key: "{view.name}",
                            span { class: "preset-name", title: "apply this view",
                                style: "cursor:pointer;",
                                onclick: move |_| state::apply_view(&apply_view),
                                "{view.name}"
                            }
                            button { class: "chip danger", onclick: del, "✕" }
                        }
                    }
                }
            }
        }
    }
}

/// Named buses: patchbay-owned null sinks, persisted in config and
/// re-created whenever the engine (re)connects.
#[component]
fn VirtualSinksPanel() -> Element {
    let handle = state::use_patchbay();
    let sinks = state::VIRTUAL_SINKS.read().clone();
    let graph = GRAPH.read();
    let live: std::collections::HashSet<String> = graph
        .nodes
        .iter()
        .filter(|n| n.virtual_sink)
        .map(|n| n.name.clone())
        .collect();
    drop(graph);
    let mut new_name = use_signal(String::new);
    let mut channels = use_signal(|| 2u32);

    let add = {
        let handle = handle.clone();
        move |_| {
            let name = new_name.peek().trim().to_string();
            if name.is_empty() {
                return;
            }
            let sink = patchbay_proto::VirtualSink {
                name,
                channels: *channels.peek(),
            };
            let handle = handle.clone();
            spawn(async move {
                if let Err(e) = handle.0.add_virtual_sink(sink).await {
                    tracing::warn!("add_virtual_sink failed: {e:?}");
                }
                state::refresh_meta(&handle).await;
            });
            new_name.set(String::new());
        }
    };

    rsx! {
        div { class: "panel-section",
            h3 { "Buses (virtual sinks)" }
            div { class: "preset-save",
                input {
                    placeholder: "bus name…",
                    value: "{new_name}",
                    oninput: move |e| new_name.set(e.value()),
                }
                select {
                    class: "alias-input bus-ch",
                    onchange: move |e| channels.set(e.value().parse().unwrap_or(2)),
                    option { value: "1", selected: *channels.read() == 1, "mono" }
                    option { value: "2", selected: *channels.read() == 2, "stereo" }
                    option { value: "4", selected: *channels.read() == 4, "4ch" }
                    option { value: "8", selected: *channels.read() == 8, "8ch" }
                }
                button { class: "chip", onclick: add, "add" }
            }
            for sink in sinks {
                {
                    let is_live = live.contains(&patchbay_proto::sink_node_name(&sink.name));
                    let del = {
                        let handle = handle.clone();
                        let name = sink.name.clone();
                        move |_| {
                            let handle = handle.clone();
                            let name = name.clone();
                            spawn(async move {
                                if let Err(e) = handle.0.remove_virtual_sink(name).await {
                                    tracing::warn!("remove_virtual_sink failed: {e:?}");
                                }
                                state::refresh_meta(&handle).await;
                            });
                        }
                    };
                    rsx! {
                        div { class: "service-row", key: "{sink.name}",
                            span { class: if is_live { "svc-dot on" } else { "svc-dot" } }
                            span { class: "service-name",
                                title: if is_live { "live" } else { "not in graph yet" },
                                "{sink.name}"
                            }
                            span { class: "rule-quantum",
                                if sink.channels == 1 { "mono" } else if sink.channels == 2 { "st" } else { "{sink.channels}ch" }
                            }
                            button { class: "chip danger", onclick: del, "✕" }
                        }
                    }
                }
            }
            if state::VIRTUAL_SINKS.read().is_empty() {
                div { class: "dim-note",
                    "Create a named bus (e.g. \"Stems\") to route several apps "
                    "into one place; it persists across PipeWire restarts."
                }
            }
        }
    }
}

/// Per-app latency rules: which apps request/pin which quantum. The
/// graph runs at the lowest request among RUNNING apps, so REAPER at 64
/// only costs you 64 while REAPER is open — everything idles back to
/// the 1024 default after. Rules apply when a node is created: restart
/// the app, or hit apply (WirePlumber restart, brief blip).
#[component]
fn LatencyPanel() -> Element {
    let handle = state::use_patchbay();
    let rules = state::LATENCY_RULES.read().clone();

    let remove = |pattern: String| {
        let handle = handle.clone();
        move |_| {
            let handle = handle.clone();
            let pattern = pattern.clone();
            spawn(async move {
                if let Err(e) = handle.0.remove_latency_rule(pattern).await {
                    tracing::warn!("remove_latency_rule failed: {e}");
                }
                state::refresh_meta(&handle).await;
            });
        }
    };
    let apply = {
        let handle = handle.clone();
        move |_| {
            let handle = handle.clone();
            spawn(async move {
                if let Err(e) = handle
                    .0
                    .service_action(
                        "wireplumber.service".into(),
                        patchbay_proto::ServiceAction::Restart,
                    )
                    .await
                {
                    tracing::warn!("wireplumber restart failed: {e}");
                }
            });
        }
    };

    rsx! {
        div { class: "panel-section",
            h3 { "App latency" }
            if rules.is_empty() {
                div { class: "dim-note",
                    "No per-app rules. Select a node and set its quantum in the inspector — "
                    "the graph follows the lowest request among running apps."
                }
            }
            for rule in rules {
                div { class: "service-row", key: "{rule.pattern}",
                    span { class: "service-name", title: "{rule.pattern}",
                        "{rule.pattern}"
                    }
                    span { class: "rule-quantum",
                        "{rule.quantum}"
                        if rule.force { span { class: "forced-tag", " pin" } }
                    }
                    button { class: "chip danger", onclick: remove(rule.pattern.clone()), "✕" }
                }
            }
            button {
                class: "chip",
                style: "margin-top:6px;",
                title: "restart WirePlumber so rules hit already-running apps (brief audio blip)",
                onclick: apply,
                "apply now (restart WirePlumber)"
            }
            ClockDefaultsEditor {}
        }
    }
}

/// Runtime clock defaults — the patchbay-owned override of the flake's
/// 50-quantum.conf (idle/default quantum + the min/max clamp). Applies
/// on PipeWire restart.
#[component]
fn ClockDefaultsEditor() -> Element {
    let handle = state::use_patchbay();
    let stored = *state::CLOCK_DEFAULTS.read();
    let mut quantum = use_signal(|| stored.quantum);
    let mut min_q = use_signal(|| stored.min_quantum);
    let mut max_q = use_signal(|| stored.max_quantum);
    use_effect(use_reactive!(|stored| {
        quantum.set(stored.quantum);
        min_q.set(stored.min_quantum);
        max_q.set(stored.max_quantum);
    }));

    let save = {
        let handle = handle.clone();
        move |_| {
            let handle = handle.clone();
            let defaults = patchbay_proto::ClockDefaults {
                quantum: *quantum.peek(),
                min_quantum: *min_q.peek(),
                max_quantum: *max_q.peek(),
            };
            spawn(async move {
                if let Err(e) = handle.0.set_clock_defaults(defaults).await {
                    tracing::warn!("set_clock_defaults failed: {e}");
                }
                state::refresh_meta(&handle).await;
            });
        }
    };

    let num_input = |label: &'static str, mut sig: Signal<u32>| {
        rsx! {
            label { class: "clock-field",
                span { class: "label", "{label}" }
                input {
                    r#type: "number",
                    class: "alias-input",
                    value: "{sig}",
                    oninput: move |e| sig.set(e.value().parse().unwrap_or(0)),
                }
            }
        }
    };

    rsx! {
        div { class: "clock-defaults",
            h3 { style: "margin-top:12px;", "Clock defaults" }
            div { class: "dim-note",
                "Idle/default quantum + clamp — overrides the flake's 50-quantum.conf "
                "(0 = don't set). Restart PipeWire (Services) to apply."
            }
            div { class: "clock-fields",
                {num_input("default", quantum)}
                {num_input("min", min_q)}
                {num_input("max", max_q)}
            }
            button { class: "chip", onclick: save, "save defaults" }
        }
    }
}

/// Rig health: the managed systemd units (PipeWire, WirePlumber, PTP
/// clock, Inferno nodes, routing links…) with restart controls. When
/// PipeWire itself is down the engine reconnects on its own once it's
/// restarted from here.
#[component]
fn ServicesPanel() -> Element {
    let handle = state::use_patchbay();

    // Poll every 5 s so a crashed unit shows up without user action.
    use_future({
        let handle = handle.clone();
        move || {
            let handle = handle.clone();
            async move {
                loop {
                    if let Ok(services) = handle.0.services().await {
                        *state::SERVICES.write() = services;
                    }
                    state::sleep_secs(5).await;
                }
            }
        }
    });

    let services = state::SERVICES.read().clone();
    let act = |unit: String, action: patchbay_proto::ServiceAction| {
        let handle = handle.clone();
        move |_| {
            let handle = handle.clone();
            let unit = unit.clone();
            spawn(async move {
                if let Err(e) = handle.0.service_action(unit.clone(), action).await {
                    tracing::warn!("service {action:?} {unit} failed: {e}");
                }
                if let Ok(services) = handle.0.services().await {
                    *state::SERVICES.write() = services;
                }
            });
        }
    };

    rsx! {
        div { class: "panel-section",
            h3 { "Services" }
            for svc in services {
                {
                    let dot = match (svc.present, svc.state.as_str()) {
                        (false, _) => "svc-dot missing",
                        (_, "active") => "svc-dot on",
                        (_, "failed") => "svc-dot failed",
                        (_, "activating" | "deactivating" | "reloading") => "svc-dot busy",
                        _ => "svc-dot",
                    };
                    let running = svc.state == "active";
                    let unit = svc.unit.clone();
                    rsx! {
                        div { class: "service-row", key: "{svc.unit}",
                            span { class: "{dot}" }
                            span { class: "service-name", title: "{svc.unit} — {svc.state}/{svc.sub_state}",
                                "{svc.label}"
                            }
                            if svc.present {
                                if running {
                                    button { class: "chip", title: "restart {svc.unit}",
                                        onclick: act(unit.clone(), patchbay_proto::ServiceAction::Restart),
                                        "↻"
                                    }
                                    button { class: "chip danger", title: "stop {svc.unit}",
                                        onclick: act(unit, patchbay_proto::ServiceAction::Stop),
                                        "■"
                                    }
                                } else {
                                    button { class: "chip", title: "start {svc.unit}",
                                        onclick: act(unit, patchbay_proto::ServiceAction::Start),
                                        "▶"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// What applying `preset` WOULD do against the current graph — the
/// same resolution the server does, computed client-side for preview.
fn preset_diff(preset: &patchbay_proto::RoutingPreset) -> String {
    let graph = GRAPH.read();
    let port_id = |node: &str, port: &str| -> Option<(u32, u32)> {
        let n = graph.nodes.iter().find(|n| n.name == node)?;
        let p = graph
            .ports
            .iter()
            .find(|p| p.node_id == n.id && p.name == port)?;
        Some((n.id, p.id))
    };
    let (mut create, mut existing, mut missing) = (0u32, 0u32, 0u32);
    for l in &preset.links {
        match (
            port_id(&l.output_node, &l.output_port),
            port_id(&l.input_node, &l.input_port),
        ) {
            (Some((_, op)), Some((_, ip))) => {
                if graph
                    .links
                    .iter()
                    .any(|x| x.output_port == op && x.input_port == ip)
                {
                    existing += 1;
                } else {
                    create += 1;
                }
            }
            _ => missing += 1,
        }
    }
    // Exclusive mode would ALSO remove live links absent from the preset.
    let in_preset = |on: &str, op: &str, inn: &str, ip: &str| {
        preset.links.iter().any(|l| {
            l.output_node == on && l.output_port == op && l.input_node == inn && l.input_port == ip
        })
    };
    let extras = graph
        .links
        .iter()
        .filter(|l| {
            let names = || {
                let on = graph.nodes.iter().find(|n| n.id == l.output_node)?;
                let op = graph.ports.iter().find(|p| p.id == l.output_port)?;
                let inn = graph.nodes.iter().find(|n| n.id == l.input_node)?;
                let ip = graph.ports.iter().find(|p| p.id == l.input_port)?;
                Some(!in_preset(&on.name, &op.name, &inn.name, &ip.name))
            };
            names().unwrap_or(false)
        })
        .count();
    format!(
        "would create {create}, keep {existing}, {missing} missing; restore would also remove {extras}"
    )
}

#[component]
fn PresetsPanel() -> Element {
    let presets = PRESETS.read().clone();
    let handle = state::use_patchbay();
    let mut new_name = use_signal(String::new);
    let report = LAST_REPORT.read().clone();

    rsx! {
        div { class: "panel-section",
            h3 { "Presets" }
            div { class: "preset-save",
                input {
                    placeholder: "preset name…",
                    value: "{new_name}",
                    oninput: move |e| new_name.set(e.value()),
                }
                button {
                    class: "chip",
                    onclick: {
                        let handle = handle.clone();
                        move |_| {
                            let name = new_name.peek().trim().to_string();
                            if name.is_empty() {
                                return;
                            }
                            let handle = handle.clone();
                            spawn(async move {
                                match handle.0.save_preset(name, String::new()).await {
                                    Ok(_) => state::refresh_meta(&handle).await,
                                    Err(e) => tracing::warn!("save_preset failed: {e:?}"),
                                }
                            });
                            new_name.set(String::new());
                        }
                    },
                    "save current"
                }
            }
            for preset in presets {
                {
                    let name = preset.name.clone();
                    let links = preset.links.len();
                    let apply = |exclusive: bool| {
                        let handle = handle.clone();
                        let name = name.clone();
                        move |_| {
                            let handle = handle.clone();
                            let name = name.clone();
                            spawn(async move {
                                match handle.0.apply_preset(name.clone(), exclusive).await {
                                    Ok(r) => *LAST_REPORT.write() = Some((name, r)),
                                    Err(e) => tracing::warn!("apply_preset failed: {e:?}"),
                                }
                            });
                        }
                    };
                    let del = {
                        let handle = handle.clone();
                        let name = name.clone();
                        move |_| {
                            let handle = handle.clone();
                            let name = name.clone();
                            spawn(async move {
                                if let Err(e) = handle.0.delete_preset(name).await {
                                    tracing::warn!("delete_preset failed: {e:?}");
                                }
                                state::refresh_meta(&handle).await;
                            });
                        }
                    };
                    let diff = {
                        let preset = preset.clone();
                        move |_| {
                            *state::PRESET_DIFF.write() =
                                Some((preset.name.clone(), preset_diff(&preset)));
                        }
                    };
                    rsx! {
                        div { class: "preset-row", key: "{preset.name}",
                            span { class: "preset-name", title: "{links} links", "{preset.name}" }
                            button { class: "chip", title: "preview what apply/restore would do",
                                onclick: diff, "?" }
                            button { class: "chip", onclick: apply(false), "apply" }
                            button { class: "chip", title: "also remove links not in the preset",
                                onclick: apply(true), "restore" }
                            button { class: "chip danger", onclick: del, "✕" }
                        }
                    }
                }
            }
            if let Some((name, text)) = state::PRESET_DIFF.read().clone() {
                div { class: "apply-report", "{name}: {text}" }
            }
            if let Some((name, r)) = report {
                div { class: "apply-report",
                    "{name}: {r.created} created, {r.existing} kept, "
                    "{r.destroyed} removed, {r.missing.len()} missing"
                }
            }
        }
    }
}

#[component]
fn Inspector() -> Element {
    let Some(node_id) = *SELECTED_NODE.read() else {
        return rsx! {
            div { class: "panel-section dim", "Select a node to inspect / rename its channels." }
        };
    };
    let graph = GRAPH.read();
    let Some(node) = graph.nodes.iter().find(|n| n.id == node_id).cloned() else {
        return rsx! {
            div { class: "panel-section dim", "Node vanished." }
        };
    };
    // Numeric-aware sort so playback_10 follows playback_9.
    let mut ports: Vec<_> = graph
        .ports
        .iter()
        .filter(|p| p.node_id == node.id)
        .cloned()
        .collect();
    drop(graph);
    ports.sort_by_key(|p| {
        let digits = p
            .name
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .count();
        if digits == 0 || digits == p.name.len() {
            (p.name.clone(), 0u64)
        } else {
            let (prefix, num) = p.name.split_at(p.name.len() - digits);
            (prefix.to_string(), num.parse().unwrap_or(0))
        }
    });

    let aliases = ALIASES.read();
    let colors = state::COLORS.read();
    let node_alias = aliases.get(&node.name).cloned().unwrap_or_default();
    let node_color = colors.get(&node.name).cloned().unwrap_or_default();
    let port_aliases: Vec<(String, String, String)> = ports
        .iter()
        .map(|p| {
            let key = format!("{}:{}", node.name, p.name);
            let alias = aliases.get(&key).cloned().unwrap_or_default();
            let color = colors.get(&key).cloned().unwrap_or_default();
            (p.name.clone(), alias, color)
        })
        .collect();
    drop(aliases);
    drop(colors);

    rsx! {
        div { class: "panel-section",
            h3 { "Inspector" }
            div { class: "inspect-line", span { class: "label", "name " } "{node.name}" }
            div { class: "inspect-line", span { class: "label", "class " } "{node.media_class}" }
            if !node.latency.is_empty() {
                div { class: "inspect-line", span { class: "label", "latency " } "{node.latency}" }
            }
            AliasEditor {
                key: "{node.name}",
                target: node.name.clone(),
                placeholder: "node display name…".to_string(),
                current: node_alias,
            }
            ColorSwatches {
                key: "col-{node.name}",
                target: node.name.clone(),
                current: node_color,
            }
            LatencyRuleEditor { key: "lat-{node.name}", node_name: node.name.clone() }
            BulkRouting { key: "bulk-{node.name}", node_name: node.name.clone() }
            ChanmapSync { node_name: node.name.clone() }
            BulkNames { key: "names-{node.name}", node_name: node.name.clone() }
            h3 { style: "margin-top:12px;", "Channels" }
            div { class: "channel-list",
                for (port_name, alias, color) in port_aliases {
                    div { class: "channel-row", key: "{node.name}:{port_name}",
                        span { class: "channel-port", title: "{port_name}", "{port_name}" }
                        ColorCycle {
                            target: format!("{}:{}", node.name, port_name),
                            current: color,
                        }
                        AliasEditor {
                            target: format!("{}:{}", node.name, port_name),
                            placeholder: String::new(),
                            current: alias,
                        }
                    }
                }
            }
        }
    }
}

/// Send a color change and refresh the color map.
fn set_color(handle: state::PatchbayHandle, target: String, color: String) {
    spawn(async move {
        if let Err(e) = handle.0.set_color(target, color).await {
            tracing::warn!("set_color failed: {e:?}");
        }
        state::refresh_meta(&handle).await;
    });
}

/// Full palette row for a node: cables from this node inherit the
/// color unless a port overrides it.
#[component]
fn ColorSwatches(target: String, current: String) -> Element {
    let handle = state::use_patchbay();
    rsx! {
        div { class: "color-swatches",
            span { class: "label", "color " }
            for c in state::PALETTE {
                {
                    let handle = handle.clone();
                    let target = target.clone();
                    let on = current == c;
                    rsx! {
                        button {
                            key: "{c}",
                            class: if on { "swatch on" } else { "swatch" },
                            style: "background:{c};",
                            onclick: move |_| set_color(
                                handle.clone(),
                                target.clone(),
                                if on { String::new() } else { c.to_string() },
                            ),
                        }
                    }
                }
            }
            button {
                class: if current.is_empty() { "swatch none on" } else { "swatch none" },
                title: "no color (media-kind default)",
                onclick: {
                    let handle = handle.clone();
                    let target = target.clone();
                    move |_| set_color(handle.clone(), target.clone(), String::new())
                },
                "×"
            }
        }
    }
}

/// Tiny per-channel color dot: click cycles the palette, last step
/// clears back to inherited.
#[component]
fn ColorCycle(target: String, current: String) -> Element {
    let handle = state::use_patchbay();
    let style = if current.is_empty() {
        "background:transparent;".to_string()
    } else {
        format!("background:{current};")
    };
    rsx! {
        button {
            class: "swatch cycle",
            style: "{style}",
            title: "channel color (click to cycle)",
            onclick: move |_| {
                let next = match state::PALETTE.iter().position(|c| *c == current) {
                    None => state::PALETTE[0].to_string(),
                    Some(i) if i + 1 < state::PALETTE.len() => state::PALETTE[i + 1].to_string(),
                    Some(_) => String::new(),
                };
                set_color(handle.clone(), target.clone(), next);
            },
        }
    }
}

/// Paste one name per line → alias this node's channels sequentially
/// (by numeric channel, on the chosen direction). 128 names in one
/// paste instead of 128 inputs.
#[component]
fn BulkNames(node_name: String) -> Element {
    let handle = state::use_patchbay();
    let mut text = use_signal(String::new);
    let mut result = use_signal(String::new);

    let apply = |direction: patchbay_proto::PortDirection| {
        let handle = handle.clone();
        let node_name = node_name.clone();
        move |_| {
            let names: Vec<String> = text.peek().lines().map(|l| l.trim().to_string()).collect();
            if names.iter().all(|l| l.is_empty()) {
                result.set("paste channel names first (one per line)".into());
                return;
            }
            // Ports of the chosen direction, in numeric-channel order.
            let graph = GRAPH.peek();
            let Some(node) = graph.nodes.iter().find(|n| n.name == node_name) else {
                return;
            };
            let mut ports: Vec<(u64, String)> = graph
                .ports
                .iter()
                .filter(|p| p.node_id == node.id && p.direction == direction)
                .filter(|p| !crate::layout::is_monitor(&p.name))
                .filter_map(|p| {
                    let digits = p
                        .name
                        .chars()
                        .rev()
                        .take_while(|c| c.is_ascii_digit())
                        .count();
                    if digits == 0 || digits == p.name.len() {
                        return None;
                    }
                    let n: u64 = p.name[p.name.len() - digits..].parse().ok()?;
                    Some((n, p.name.clone()))
                })
                .collect();
            drop(graph);
            ports.sort();
            let pairs: Vec<(String, String)> = ports
                .into_iter()
                .zip(names.iter().cloned())
                .filter(|(_, name)| !name.is_empty()) // blank line = skip channel
                .map(|((_, port), name)| (format!("{node_name}:{port}"), name))
                .collect();
            if pairs.is_empty() {
                result.set("no numeric channels to name on that side".into());
                return;
            }
            let handle = handle.clone();
            spawn(async move {
                let n = pairs.len();
                for (target, alias) in pairs {
                    if let Err(e) = handle.0.set_alias(target, alias).await {
                        tracing::warn!("bulk set_alias failed: {e:?}");
                    }
                }
                state::refresh_meta(&handle).await;
                result.set(format!("named {n} channels"));
            });
        }
    };

    rsx! {
        div { class: "bulk-names",
            span { class: "label", "bulk name channels (one per line, blank = skip)" }
            textarea {
                class: "alias-input names-paste",
                rows: "4",
                placeholder: "Main Output ST L\nMain Output ST R\nClick\n…",
                value: "{text}",
                oninput: move |e| text.set(e.value()),
            }
            div { class: "chanmap-buttons",
                button { class: "chip", onclick: apply(patchbay_proto::PortDirection::Input),
                    "→ inputs" }
                button { class: "chip", onclick: apply(patchbay_proto::PortDirection::Output),
                    "→ outputs" }
            }
            if !result.read().is_empty() {
                div { class: "apply-report", "{result}" }
            }
        }
    }
}

/// One alias input: commits on Enter or blur, empty clears.
#[component]
fn AliasEditor(target: String, placeholder: String, current: String) -> Element {
    let handle = state::use_patchbay();
    let mut draft = use_signal(|| current.clone());
    // Follow external changes (chanmap import, another editor).
    use_effect(use_reactive!(|current| draft.set(current)));

    let commit = {
        let target = target.clone();
        move || {
            let value = draft.peek().trim().to_string();
            if value == current {
                return;
            }
            let handle = handle.clone();
            let target = target.clone();
            spawn(async move {
                if let Err(e) = handle.0.set_alias(target, value).await {
                    tracing::warn!("set_alias failed: {e:?}");
                }
                state::refresh_meta(&handle).await;
            });
        }
    };
    let commit_blur = commit.clone();

    rsx! {
        input {
            class: "alias-input",
            placeholder: "{placeholder}",
            value: "{draft}",
            oninput: move |e| draft.set(e.value()),
            onkeydown: move |e| {
                if e.key() == Key::Enter {
                    commit();
                }
            },
            onblur: move |_| commit_blur(),
        }
    }
}

/// Bulk 1:1 wiring from the inspected node into a target node — the
/// direct-path tool (plain links add zero latency; loopback sinks like
/// `daw` buffer a full quantum). Pair REAPER straight to Inferno here,
/// then save it as a preset.
#[component]
fn BulkRouting(node_name: String) -> Element {
    let handle = state::use_patchbay();
    let mut target = use_signal(String::new);
    let mut result = use_signal(String::new);

    // Candidate targets: any other node that has input ports.
    let graph = GRAPH.read();
    let mut targets: Vec<(String, String)> = graph
        .nodes
        .iter()
        .filter(|n| n.name != node_name)
        .filter(|n| {
            graph
                .ports
                .iter()
                .any(|p| p.node_id == n.id && p.direction == patchbay_proto::PortDirection::Input)
        })
        .map(|n| (n.name.clone(), state::node_label(&n.name, &n.label)))
        .collect();
    drop(graph);
    targets.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));

    let run = |link: bool| {
        let handle = handle.clone();
        let node_name = node_name.clone();
        move |_| {
            let to = target.peek().clone();
            if to.is_empty() {
                result.set("pick a target node first".into());
                return;
            }
            let handle = handle.clone();
            let from = node_name.clone();
            spawn(async move {
                let res = if link {
                    handle.0.connect_one_to_one(from, to).await
                } else {
                    handle.0.disconnect_nodes(from, to).await
                };
                match res {
                    Ok(n) => result.set(format!(
                        "{n} link(s) {}",
                        if link { "created" } else { "removed" }
                    )),
                    Err(e) => result.set(format!("failed: {e}")),
                }
            });
        }
    };

    rsx! {
        div { class: "bulk-routing",
            span { class: "label", "bulk route (1:1 by channel) → " }
            select {
                class: "alias-input",
                onchange: move |e| target.set(e.value()),
                option { value: "", selected: target.read().is_empty(), "target node…" }
                for (name, label) in targets {
                    option { value: "{name}", selected: *target.read() == name, "{label}" }
                }
            }
            div { class: "chanmap-buttons",
                button { class: "chip", onclick: run(true), "link 1:1" }
                button { class: "chip danger", onclick: run(false), "unlink all →" }
            }
            if !result.read().is_empty() {
                div { class: "apply-report", "{result}" }
            }
        }
    }
}

/// Quantum rule for this node: pick a buffer size and whether it's a
/// hard pin (`node.force-quantum`) or a request (`node.latency`).
#[component]
fn LatencyRuleEditor(node_name: String) -> Element {
    let handle = state::use_patchbay();
    let rule = state::LATENCY_RULES
        .read()
        .iter()
        .find(|r| r.pattern == node_name)
        .cloned();
    let current = rule.as_ref().map(|r| r.quantum).unwrap_or(0);
    let force = rule.as_ref().map(|r| r.force).unwrap_or(true);

    let set = |quantum: u32, force: bool| {
        let handle = handle.clone();
        let node_name = node_name.clone();
        move |_| {
            let handle = handle.clone();
            let node_name = node_name.clone();
            spawn(async move {
                let res = if quantum == 0 {
                    handle.0.remove_latency_rule(node_name).await
                } else {
                    handle
                        .0
                        .set_latency_rule(patchbay_proto::LatencyRule {
                            pattern: node_name,
                            quantum,
                            force,
                        })
                        .await
                };
                if let Err(e) = res {
                    tracing::warn!("latency rule change failed: {e}");
                }
                state::refresh_meta(&handle).await;
            });
        }
    };

    rsx! {
        div { class: "latency-editor",
            span { class: "label", "quantum rule " }
            for q in [0u32, 32, 64, 128, 256, 512, 1024, 2048] {
                button {
                    key: "{q}",
                    class: if q == current { "chip on" } else { "chip" },
                    onclick: set(q, force),
                    if q == 0 { "none" } else { "{q}" }
                }
            }
            if current != 0 {
                button {
                    class: if force { "chip on" } else { "chip" },
                    title: "pin: node.force-quantum (hard) vs request: node.latency (driver takes the min)",
                    onclick: set(current, !force),
                    if force { "pinned" } else { "request" }
                }
            }
            div { class: "dim-note",
                "applies when the app (re)starts, or apply now from the App latency panel"
            }
        }
    }
}

/// Import/export this node's channel names from/to the REAPER ChanMap
/// (empty path = the host's default chanmap).
#[component]
fn ChanmapSync(node_name: String) -> Element {
    let handle = state::use_patchbay();
    let mut path = use_signal(String::new);
    let mut result = use_signal(String::new);

    let run = |import: bool| {
        let handle = handle.clone();
        let node_name = node_name.clone();
        move |_| {
            let handle = handle.clone();
            let node_name = node_name.clone();
            let path = path.peek().clone();
            spawn(async move {
                let res = if import {
                    handle.0.import_chanmap(node_name, path).await
                } else {
                    handle.0.export_chanmap(node_name, path).await
                };
                match res {
                    Ok(n) => {
                        result.set(format!(
                            "{} {n} channel names",
                            if import { "imported" } else { "exported" }
                        ));
                        state::refresh_meta(&handle).await;
                    }
                    Err(e) => result.set(format!("chanmap failed: {e}")),
                }
            });
        }
    };

    let mut dante_dev = use_signal(String::new);
    let dante_names: Vec<String> = state::DANTE_DEVICES
        .read()
        .iter()
        .map(|d| d.name.clone())
        .collect();

    let run_dante = |direction: &'static str| {
        let handle = handle.clone();
        let node_name = node_name.clone();
        move |_| {
            let handle = handle.clone();
            let node_name = node_name.clone();
            let device = dante_dev.peek().clone();
            result.set("scanning Dante network…".into());
            spawn(async move {
                match handle
                    .0
                    .import_inferno_names(node_name, device, direction.into())
                    .await
                {
                    Ok(n) => {
                        result.set(format!("imported {n} Dante {direction} channel names"));
                        state::refresh_meta(&handle).await;
                    }
                    Err(e) => result.set(format!("dante import failed: {e}")),
                }
            });
        }
    };

    rsx! {
        div { class: "chanmap-sync",
            input {
                class: "alias-input",
                placeholder: "chanmap path (empty = host default)",
                value: "{path}",
                oninput: move |e| path.set(e.value()),
            }
            div { class: "chanmap-buttons",
                button { class: "chip", title: "ChanMap names → channel aliases",
                    onclick: run(true), "import chanmap" }
                button { class: "chip", title: "channel aliases → ChanMap nameN lines",
                    onclick: run(false), "export chanmap" }
            }
            div { class: "chanmap-buttons",
                select {
                    class: "alias-input",
                    title: "which Dante device's channel list to import names from",
                    onchange: move |e| dante_dev.set(e.value()),
                    option { value: "", selected: dante_dev.read().is_empty(),
                        "Dante device (auto)" }
                    for name in dante_names {
                        option { value: "{name}", selected: *dante_dev.read() == name, "{name}" }
                    }
                }
            }
            div { class: "chanmap-buttons",
                button { class: "chip",
                    title: "name channels from the device's RX list over ARC (for capture/source proxies)",
                    onclick: run_dante("rx"), "Dante names (rx)" }
                button { class: "chip",
                    title: "name channels from the device's TX list over ARC (for playback/sink proxies)",
                    onclick: run_dante("tx"), "Dante names (tx)" }
            }
            if !result.read().is_empty() {
                div { class: "apply-report", "{result}" }
            }
        }
    }
}
