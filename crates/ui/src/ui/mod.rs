//! Shared widgets.
//!
//! One implementation each of the things every view needs: meters,
//! faders, collapsible sections, commit-on-blur text fields, two-click
//! destructive buttons, status dots, error bars and empty states. Views
//! compose these rather than re-growing their own.
//!
//! Everything here is presentation only — props in, events out, no RPC
//! and no global signals — so a widget can be dropped into any view and
//! unit-tested through [`level`].

pub mod level;

use dioxus::prelude::*;

use level::{METER_HOT_DB, METER_WARN_DB, fader_slot, fmt_db, fmt_peak, meter_pct};

/// Vertical peak meter, `db` in dBFS.
///
/// The colour gradient is fixed to the scale and a cover hides what is
/// above the current level, so an animating meter never repaints a
/// gradient.
#[component]
pub fn Meter(db: f64) -> Element {
    let cover = 100.0 - meter_pct(db);
    let hot = db > METER_HOT_DB;
    rsx! {
        div { class: if hot { "meter hot" } else { "meter" }, title: "{fmt_peak(db)}",
            div { class: "meter-cover", style: "height: {cover:.1}%;" }
        }
    }
}

/// Horizontal peak meter for list rows (the Now dashboard).
#[component]
pub fn LevelBar(db: f64) -> Element {
    let cover = 100.0 - meter_pct(db);
    let hot = db > METER_HOT_DB;
    rsx! {
        div { class: if hot { "levelbar hot" } else { "levelbar" }, title: "{fmt_peak(db)}",
            div { class: "levelbar-cover", style: "width: {cover:.1}%;" }
        }
    }
}

/// dB ticks beside a vertical [`Meter`].
#[component]
pub fn MeterScale() -> Element {
    let ticks: [(f64, &str); 5] = [
        (0.0, "0"),
        (METER_HOT_DB, "6"),
        (METER_WARN_DB, "20"),
        (-40.0, "40"),
        (level::METER_FLOOR_DB, "60"),
    ];
    rsx! {
        div { class: "meter-scale",
            for (db, label) in ticks {
                span { style: "bottom: {meter_pct(db):.1}%;", "{label}" }
            }
        }
    }
}

/// Vertical fader + readout. Double-click returns it to 0 dB.
///
/// Emits `(gain_db, muted)` so a caller can pass the pair straight to
/// `set_mix_source` / `set_mix_output`.
#[component]
pub fn Fader(gain_db: f64, muted: bool, on_level: EventHandler<(f64, bool)>) -> Element {
    let slot = fader_slot(gain_db);
    let text = fmt_db(gain_db);
    rsx! {
        input {
            r#type: "range",
            class: "vfader",
            min: "{level::FADER_OFF_SLOT}",
            max: "{level::FADER_MAX_DB}",
            step: "0.5",
            value: "{slot}",
            title: "{text} — double-click for 0 dB",
            oninput: move |e| {
                if let Ok(x) = e.value().parse::<f64>() {
                    on_level.call((level::slot_db(x), muted));
                }
            },
            ondoubleclick: move |_| on_level.call((0.0, muted)),
        }
    }
}

/// What a [`StatusDot`] is saying.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Running, connected, live.
    Ok,
    /// Present but not passing anything.
    Idle,
    /// Working on it.
    Busy,
    /// Failed.
    Bad,
    /// Not there at all.
    Missing,
}

impl Status {
    const fn class(self) -> &'static str {
        match self {
            Self::Ok => "svc-dot on",
            Self::Idle => "svc-dot",
            Self::Busy => "svc-dot busy",
            Self::Bad => "svc-dot failed",
            Self::Missing => "svc-dot missing",
        }
    }
}

/// The one status dot: online / idle / busy / failed / absent.
#[component]
pub fn StatusDot(status: Status, title: String) -> Element {
    rsx! { span { class: status.class(), title: "{title}" } }
}

/// A collapsible titled block. The caller owns `open` so the state can
/// live wherever it belongs (a set of open paths, a per-device map).
#[component]
pub fn Section(
    label: String,
    #[props(default = String::new())] note: String,
    open: bool,
    ontoggle: EventHandler<()>,
    children: Element,
) -> Element {
    rsx! {
        div { class: "section",
            button {
                class: "section-head",
                onclick: move |_| ontoggle.call(()),
                span { class: "section-caret", if open { "▾" } else { "▸" } }
                span { class: "section-label", "{label}" }
                if !note.is_empty() {
                    span { class: "section-note", "{note}" }
                }
            }
            if open {
                div { class: "section-body", {children} }
            }
        }
    }
}

/// Text field that commits on Enter or blur, and follows `value` when it
/// changes underneath (a rename from the device, another client, a
/// refresh) as long as the user isn't mid-edit.
#[component]
pub fn TextField(
    value: String,
    #[props(default = String::new())] placeholder: String,
    #[props(default = String::new())] class: String,
    on_commit: EventHandler<String>,
) -> Element {
    let mut draft = use_signal(|| value.clone());
    let mut editing = use_signal(|| false);
    use_effect(use_reactive!(|value| {
        if !*editing.peek() {
            draft.set(value);
        }
    }));
    // Inlined rather than shared: a closure that writes to `editing`
    // borrows it mutably, so it can't be handed to two event handlers.
    rsx! {
        input {
            class: if class.is_empty() { "textfield".to_owned() } else { format!("textfield {class}") },
            value: "{draft}",
            placeholder: "{placeholder}",
            oninput: move |e| {
                editing.set(true);
                draft.set(e.value());
            },
            onblur: move |_| {
                editing.set(false);
                on_commit.call(draft.peek().clone());
            },
            onkeydown: move |e: Event<KeyboardData>| {
                match e.key() {
                    Key::Enter => {
                        editing.set(false);
                        on_commit.call(draft.peek().clone());
                    }
                    Key::Escape => {
                        editing.set(false);
                        draft.set(value.clone());
                    }
                    _ => {}
                }
            },
        }
    }
}

/// Destructive button that arms on the first click and acts on the
/// second, so nothing irreversible is one stray click away.
#[component]
pub fn ConfirmButton(
    label: String,
    #[props(default = String::new())] armed_label: String,
    on_confirm: EventHandler<()>,
) -> Element {
    let mut armed = use_signal(|| false);
    let armed_now = armed();
    let text = if armed_now && !armed_label.is_empty() {
        armed_label
    } else {
        label
    };
    rsx! {
        button {
            class: if armed_now { "chip danger armed" } else { "chip danger" },
            title: if armed_now { "click again to confirm" } else { "" },
            onclick: move |_| {
                if armed_now {
                    armed.set(false);
                    on_confirm.call(());
                } else {
                    armed.set(true);
                }
            },
            onmouseleave: move |_| armed.set(false),
            // Touch has no "leave": tapping anywhere else disarms it.
            onblur: move |_| armed.set(false),
            "{text}"
        }
    }
}

/// A failure the user should see, with a way to dismiss it. Views put
/// this at the top of their body rather than logging and moving on.
#[component]
pub fn ErrorBar(message: String, on_dismiss: EventHandler<()>) -> Element {
    if message.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "error-bar",
            span { "{message}" }
            button { class: "chip", onclick: move |_| on_dismiss.call(()), "dismiss" }
        }
    }
}

/// What to show where there is nothing yet: what this place is for, and
/// the way to fill it.
#[component]
pub fn EmptyState(title: String, children: Element) -> Element {
    rsx! {
        div { class: "empty-state",
            h3 { "{title}" }
            {children}
        }
    }
}
