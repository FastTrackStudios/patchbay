//! The two form controls every renderer can draw: a dropdown and a
//! slider, built from `div`s and buttons.
//!
//! Blitz (Architect's window) has no `<select>`, no `<input type=range>`
//! and no `change` event, so the native controls cannot be used there;
//! these draw the same in Blitz, the desktop webview and the browser
//! remote.
//!
//! The slider follows `features/editor/src/ui/drag.rs`: it measures in
//! async event handlers (a `spawn` that measures while blitz-dom is still
//! dispatching panics with "already borrowed"), tracks the unclamped
//! value through a gesture so the thumb can't oscillate between steps,
//! re-measures per gesture, and ends a gesture on release wherever that
//! happens.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use dioxus::prelude::*;

/// The one open dropdown, by instance. Opening another closes it; a press
/// anywhere else in the app closes it ([`close_choices`]).
static OPEN_CHOICE: GlobalSignal<Option<u64>> = Signal::global(|| None);

/// Close the open dropdown. The app root calls this on every press that a
/// dropdown did not stop.
pub fn close_choices() {
    if OPEN_CHOICE.peek().is_some() {
        *OPEN_CHOICE.write() = None;
    }
}

/// One entry of a [`Choice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    /// What [`Choice::onpick`] is called with.
    pub value: String,
    /// What it shows.
    pub label: String,
    /// Shown, not pickable.
    pub disabled: bool,
}

impl ChoiceOption {
    /// A pickable option.
    #[must_use]
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            disabled: false,
        }
    }

    /// One whose label is its value.
    #[must_use]
    pub fn plain(value: impl Into<String>) -> Self {
        let value = value.into();
        Self::new(value.clone(), value)
    }
}

/// A dropdown: the current option on a button, the options in a list
/// under it (over it with `up`, for one near the bottom of the window).
/// Picking calls `onpick` with the option's value; the caller owns the
/// value.
#[component]
pub fn Choice(
    value: String,
    options: Vec<ChoiceOption>,
    onpick: EventHandler<String>,
    #[props(default)] class: String,
    #[props(default)] disabled: bool,
    /// Shown when `value` is none of the options.
    #[props(default)]
    placeholder: String,
    /// The accessible name, when no visible label says it.
    #[props(default)]
    label: String,
    /// Open the list above the button.
    #[props(default)]
    up: bool,
) -> Element {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = use_hook(|| NEXT.fetch_add(1, Ordering::Relaxed));
    let open = *OPEN_CHOICE.read() == Some(id);
    let shown = options
        .iter()
        .find(|o| o.value == value)
        .map_or_else(|| placeholder.clone(), |o| o.label.clone());
    let toggle = move |_| {
        *OPEN_CHOICE.write() = if open { None } else { Some(id) };
    };
    let menu_at = if up {
        "bottom: calc(100% + 2px);"
    } else {
        "top: calc(100% + 2px);"
    };
    rsx! {
        div {
            class: if class.is_empty() { "choice".to_owned() } else { format!("choice {class}") },
            style: "position: relative; display: inline-flex; min-width: 0; max-width: 100%;",
            // A press inside keeps it open; the root closes it otherwise.
            onpointerdown: move |e| e.stop_propagation(),
            onkeydown: move |e: Event<KeyboardData>| {
                if e.key() == Key::Named(NamedKey::Escape) && open {
                    e.stop_propagation();
                    *OPEN_CHOICE.write() = None;
                }
            },
            button {
                class: if open { "choice-button open" } else { "choice-button" },
                r#type: "button",
                disabled,
                "aria-haspopup": "listbox",
                "aria-expanded": "{open}",
                "aria-label": "{label}",
                title: "{shown}",
                onclick: toggle,
                span { class: "choice-label", "{shown}" }
                span { class: "choice-caret", if up { "▴" } else { "▾" } }
            }
            if open {
                div {
                    class: "choice-menu",
                    role: "listbox",
                    style: "position: absolute; {menu_at} left: 0; z-index: 60; min-width: 100%; max-height: 300px; overflow-y: scroll; display: flex; flex-direction: column;",
                    for o in options.iter().cloned() {
                        button {
                            key: "{o.value}",
                            r#type: "button",
                            role: "option",
                            class: if o.value == value { "choice-item on" } else { "choice-item" },
                            disabled: o.disabled,
                            onclick: move |_| {
                                *OPEN_CHOICE.write() = None;
                                onpick.call(o.value.clone());
                            },
                            "{o.label}"
                        }
                    }
                }
            }
        }
    }
}

/// Fraction of the way along a track, clamped; `from_end` counts from the
/// far edge (a vertical slider counts up from its bottom).
fn fraction(origin: f64, length: f64, at: f64, from_end: bool) -> f64 {
    if length <= 0.0 {
        return 0.0;
    }
    let f = ((at - origin) / length).clamp(0.0, 1.0);
    if from_end { 1.0 - f } else { f }
}

/// Snap `v` to `step` within `min..=max`.
fn quantize(v: f64, min: f64, max: f64, step: f64) -> f64 {
    if step <= 0.0 {
        return v.clamp(min, max);
    }
    ((v - min) / step)
        .round()
        .mul_add(step, min)
        .clamp(min, max)
}

/// A slider over `min..=max`, horizontal or `vertical`.
///
/// `on_input` follows the drag (for what is cheap to change live, a mix
/// gain); `on_commit` is called once when it ends, and for each key press
/// (for what goes to hardware, a console's fader). Arrow keys step it,
/// Home and End go to the ends. Its size comes from `class`.
#[component]
pub fn Slider(
    value: f64,
    min: f64,
    max: f64,
    #[props(default = 0.01)] step: f64,
    #[props(default)] vertical: bool,
    #[props(default)] disabled: bool,
    #[props(default)] class: String,
    #[props(default)] title: String,
    #[props(default)] on_input: Option<EventHandler<f64>>,
    #[props(default)] on_commit: Option<EventHandler<f64>>,
    /// A double-click (a fader back to 0 dB).
    #[props(default)]
    on_reset: Option<EventHandler<()>>,
) -> Element {
    let span = (max - min).max(f64::EPSILON);
    // The live, unclamped value while a gesture is in flight; `None`
    // between gestures, when the prop is the truth.
    let mut granular = use_signal(|| Option::<f64>::None);
    // The track's origin and length along its axis, measured per gesture.
    let mut track = use_signal(|| (0.0_f64, 0.0_f64));
    let mut mounted: Signal<Option<Rc<MountedData>>> = use_signal(|| None);

    let measure = move || async move {
        let rect = match mounted() {
            Some(el) => el.get_client_rect().await.ok(),
            None => None,
        };
        if let Some(r) = rect {
            track.set(if vertical {
                (r.origin.y, r.size.height)
            } else {
                (r.origin.x, r.size.width)
            });
        }
    };
    let at = move |e: &PointerData| {
        let p = e.client_coordinates();
        if vertical { p.y } else { p.x }
    };
    let mut emit = move |pos: f64| {
        let (origin, length) = track();
        let raw = fraction(origin, length, pos, vertical).mul_add(span, min);
        granular.set(Some(raw));
        let v = quantize(raw, min, max, step);
        if let Some(on_input) = on_input {
            on_input.call(v);
        }
    };
    let mut end = move || {
        if let (Some(raw), Some(on_commit)) = (granular.take(), on_commit) {
            on_commit.call(quantize(raw, min, max, step));
        }
    };
    let key_step = move |e: Event<KeyboardData>| {
        let next = match e.key() {
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowUp) => value + step,
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowDown) => value - step,
            Key::Named(NamedKey::Home) => min,
            Key::Named(NamedKey::End) => max,
            _ => return,
        };
        e.prevent_default();
        let v = quantize(next, min, max, step);
        if let Some(on_input) = on_input {
            on_input.call(v);
        }
        if let Some(on_commit) = on_commit {
            on_commit.call(v);
        }
    };

    // Drawn from the granular value mid-gesture, so the thumb follows the
    // pointer rather than the quantized feedback.
    let shown = granular().unwrap_or(value);
    let pos = ((shown - min) / span).clamp(0.0, 1.0) * 100.0;
    let (fill, thumb) = if vertical {
        (
            format!("left: 0; right: 0; bottom: 0; height: {pos:.2}%;"),
            format!("left: 50%; bottom: {pos:.2}%; transform: translate(-50%, 50%);"),
        )
    } else {
        (
            format!("top: 0; bottom: 0; left: 0; width: {pos:.2}%;"),
            format!("top: 50%; left: {pos:.2}%; transform: translate(-50%, -50%);"),
        )
    };
    let axis = if vertical { "vertical" } else { "horizontal" };
    rsx! {
        div {
            class: if class.is_empty() { format!("slider {axis}") } else { format!("slider {axis} {class}") },
            "data-disabled": "{disabled}",
            role: "slider",
            tabindex: if disabled { "-1" } else { "0" },
            "aria-orientation": "{axis}",
            "aria-valuemin": "{min}",
            "aria-valuemax": "{max}",
            "aria-valuenow": "{value}",
            title: "{title}",
            style: "position: relative; touch-action: none; user-select: none;",
            onmounted: move |e| async move {
                mounted.set(Some(e.data()));
            },
            onpointerdown: move |e: Event<PointerData>| async move {
                if disabled {
                    return;
                }
                // Async, not `spawn`: this runs after event dispatch has
                // let go of the document.
                measure().await;
                emit(at(&e.data()));
            },
            onpointermove: move |e: Event<PointerData>| {
                if granular.peek().is_none() {
                    return;
                }
                // Released outside: the release never reached here.
                if e.data().held_buttons().is_empty() {
                    end();
                } else {
                    emit(at(&e.data()));
                }
            },
            onpointerup: move |_| end(),
            onkeydown: move |e| {
                if !disabled {
                    key_step(e);
                }
            },
            ondoubleclick: move |_| {
                if let (false, Some(reset)) = (disabled, on_reset) {
                    reset.call(());
                }
            },
            div { class: "slider-track", style: "position: absolute; inset: 0;",
                div { class: "slider-fill", style: "position: absolute; {fill}" }
            }
            div { class: "slider-thumb", style: "position: absolute; {thumb} pointer-events: none;" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fraction, quantize};

    #[test]
    fn a_vertical_track_counts_up_from_its_bottom() {
        assert!((fraction(100.0, 200.0, 300.0, true) - 0.0).abs() < 1e-9);
        assert!((fraction(100.0, 200.0, 100.0, true) - 1.0).abs() < 1e-9);
        assert!((fraction(100.0, 200.0, 150.0, false) - 0.25).abs() < 1e-9);
        assert!((fraction(0.0, 0.0, 10.0, false) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn values_snap_to_the_step_inside_the_range() {
        assert!((quantize(0.26, 0.0, 1.0, 0.05) - 0.25).abs() < 1e-9);
        assert!((quantize(-3.0, -1.0, 1.0, 0.05) + 1.0).abs() < 1e-9);
        assert!((quantize(-71.3, -96.0, 10.0, 0.5) + 71.5).abs() < 1e-9);
    }
}
