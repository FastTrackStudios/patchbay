//! Appearance — theme, accent and density.
//!
//! A per-device choice, not an engine setting: the phone on the music
//! stand wants Midnight while the desktop beside it stays on Studio, so
//! it lives in the webview's `localStorage` and never crosses the RPC.
//! That also keeps this crate shell-agnostic — the desktop webview and
//! a browser remote persist it the same way.
//!
//! The choice reaches the stylesheet as three `data-*` attributes on the
//! app root (see `theme/tokens.css`); nothing here knows a colour.

use dioxus::prelude::*;

/// `localStorage` key. `.v2`: the first build wrote its defaults here
/// on every load, so a stored value there says nothing about what anyone
/// chose.
const STORE_KEY: &str = "patchbay.appearance.v2";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Theme {
    /// Studio or Daylight, whichever the system is asking for.
    Auto,
    Studio,
    /// The default: this is mostly used in dark rooms, often on a phone.
    #[default]
    Midnight,
    Console,
    Daylight,
    Contrast,
}

impl Theme {
    pub const ALL: [Self; 6] = [
        Self::Auto,
        Self::Studio,
        Self::Midnight,
        Self::Console,
        Self::Daylight,
        Self::Contrast,
    ];

    /// The `data-theme` value, and the stored name.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Studio => "studio",
            Self::Midnight => "midnight",
            Self::Console => "console",
            Self::Daylight => "daylight",
            Self::Contrast => "contrast",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Studio => "Studio",
            Self::Midnight => "Midnight",
            Self::Console => "Console",
            Self::Daylight => "Daylight",
            Self::Contrast => "Contrast",
        }
    }

    #[must_use]
    pub const fn note(self) -> &'static str {
        match self {
            Self::Auto => "Follows this device",
            Self::Studio => "Neutral dark",
            Self::Midnight => "True black, for OLED and dark stages",
            Self::Console => "Warm charcoal",
            Self::Daylight => "Light, for bright rooms",
            Self::Contrast => "Maximum legibility",
        }
    }

    /// The concrete theme to paint: `Auto` becomes whatever the system
    /// prefers.
    #[must_use]
    pub const fn resolve(self, system_light: bool) -> Self {
        match self {
            Self::Auto if system_light => Self::Daylight,
            Self::Auto => Self::Studio,
            other => other,
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.id() == id)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Accent {
    #[default]
    Blue,
    Teal,
    Green,
    Amber,
    Orange,
    Red,
    Pink,
    Purple,
}

impl Accent {
    pub const ALL: [Self; 8] = [
        Self::Blue,
        Self::Teal,
        Self::Green,
        Self::Amber,
        Self::Orange,
        Self::Red,
        Self::Pink,
        Self::Purple,
    ];

    /// The `data-accent` value, and the stored name.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Blue => "blue",
            Self::Teal => "teal",
            Self::Green => "green",
            Self::Amber => "amber",
            Self::Orange => "orange",
            Self::Red => "red",
            Self::Pink => "pink",
            Self::Purple => "purple",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.id() == id)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
}

impl Density {
    pub const ALL: [Self; 2] = [Self::Comfortable, Self::Compact];

    /// The `data-density` value, and the stored name.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Comfortable => "comfortable",
            Self::Compact => "compact",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Comfortable => "Comfortable",
            Self::Compact => "Compact",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.id() == id)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Appearance {
    pub theme: Theme,
    pub accent: Accent,
    pub density: Density,
}

impl Appearance {
    /// `theme=studio;accent=blue;density=compact`.
    fn encode(self) -> String {
        format!(
            "theme={};accent={};density={}",
            self.theme.id(),
            self.accent.id(),
            self.density.id()
        )
    }

    /// Tolerant on purpose: a field this build doesn't know (a theme
    /// that was removed, a key a newer build wrote) falls back to the
    /// default for that field and leaves the rest alone.
    fn decode(stored: &str) -> Self {
        let mut out = Self::default();
        for (key, value) in stored.split(';').filter_map(|kv| kv.split_once('=')) {
            match key.trim() {
                "theme" => out.theme = Theme::from_id(value.trim()).unwrap_or_default(),
                "accent" => out.accent = Accent::from_id(value.trim()).unwrap_or_default(),
                "density" => out.density = Density::from_id(value.trim()).unwrap_or_default(),
                _ => {}
            }
        }
        out
    }
}

pub static APPEARANCE: GlobalSignal<Appearance> = Signal::global(Appearance::default);
/// The system is asking for a light scheme (`prefers-color-scheme`).
static SYSTEM_LIGHT: GlobalSignal<bool> = Signal::global(|| false);
/// The stored choice has been read. Until then nothing is painted from
/// the defaults this starts with.
static LOADED: GlobalSignal<bool> = Signal::global(|| false);
/// Someone on this device has picked something — it was stored, or they
/// just did. Only then is anything written: a device that never chose
/// keeps following the default, including when the default changes.
static CHOSEN: GlobalSignal<bool> = Signal::global(|| false);

/// Change the appearance because the user asked to.
fn choose(f: impl FnOnce(&mut Appearance)) {
    *CHOSEN.write() = true;
    f(&mut APPEARANCE.write());
}

/// Reads the stored choice, then reports the system scheme now and on
/// every change. Messages are `prefs:<encoded>` and `scheme:<light|dark>`.
const WATCH_JS: &str = r#"
let stored = "";
try { stored = localStorage.getItem("STORE_KEY") || ""; } catch (_) {}
dioxus.send("prefs:" + stored);
const mq = window.matchMedia("(prefers-color-scheme: light)");
const report = () => { try { dioxus.send("scheme:" + (mq.matches ? "light" : "dark")); } catch (_) {} };
report();
if (mq.addEventListener) { mq.addEventListener("change", report); } else { mq.addListener(report); }
await new Promise(() => {});
"#;

/// After the root has the new attributes: persist (`Some` — only a
/// choice is stored), then paint what is outside the app root in the
/// theme too — the page behind it (seen in overscroll and the
/// home-indicator strip) and the browser's own chrome via `theme-color`.
fn apply_js(persist: Option<&str>) -> String {
    let store = persist.map_or_else(String::new, |encoded| {
        format!(r#"try {{ localStorage.setItem("{STORE_KEY}", "{encoded}"); }} catch (_) {{}}"#)
    });
    format!(
        r#"
{store}
requestAnimationFrame(() => {{
  const root = document.querySelector(".patchbay-root");
  if (!root) return;
  const css = getComputedStyle(root);
  const bg = css.getPropertyValue("--bg").trim();
  const surface = css.getPropertyValue("--surface").trim();
  document.documentElement.style.background = bg;
  document.body.style.background = surface;
  document.documentElement.style.colorScheme = css.colorScheme;
  let meta = document.querySelector('meta[name="theme-color"]');
  if (!meta) {{
    meta = document.createElement("meta");
    meta.name = "theme-color";
    document.head.appendChild(meta);
  }}
  meta.content = bg;
}});
"#
    )
}

/// The three `data-*` values for the app root.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RootAttrs {
    pub theme: &'static str,
    pub accent: &'static str,
    pub density: &'static str,
}

/// Loads, watches and persists the appearance; returns what the app
/// root should wear. Call once from whichever component renders
/// `.patchbay-root`.
pub fn use_appearance() -> RootAttrs {
    use_future(|| async {
        let mut watch = document::eval(&WATCH_JS.replace("STORE_KEY", STORE_KEY));
        while let Ok(msg) = watch.recv::<String>().await {
            if let Some(stored) = msg.strip_prefix("prefs:") {
                if !stored.is_empty() {
                    *CHOSEN.write() = true;
                }
                let a = Appearance::decode(stored);
                if *APPEARANCE.peek() != a {
                    *APPEARANCE.write() = a;
                }
                *LOADED.write() = true;
            } else if let Some(scheme) = msg.strip_prefix("scheme:") {
                let light = scheme == "light";
                if *SYSTEM_LIGHT.peek() != light {
                    *SYSTEM_LIGHT.write() = light;
                }
            }
        }
    });

    use_effect(|| {
        let a = *APPEARANCE.read();
        // Subscribed so an Auto theme repaints the page chrome too.
        let _ = *SYSTEM_LIGHT.read();
        if *LOADED.read() {
            let encoded = a.encode();
            document::eval(&apply_js(CHOSEN.read().then_some(encoded.as_str())));
        }
    });

    let a = *APPEARANCE.read();
    RootAttrs {
        theme: a.theme.resolve(*SYSTEM_LIGHT.read()).id(),
        accent: a.accent.id(),
        density: a.density.id(),
    }
}

// ─── Settings section ───────────────────────────────────────────────────

#[component]
pub fn AppearanceSection() -> Element {
    let current = *APPEARANCE.read();
    let painted = current.theme.resolve(*SYSTEM_LIGHT.read()).id();
    rsx! {
        section { class: "settings-section",
            h3 { class: "section-label", "Appearance" }
            p { class: "dim-note",
                "Saved on this device only — a phone and the desktop can each look their own way."
            }
            div { class: "theme-grid",
                for theme in Theme::ALL {
                    ThemeCard { key: "{theme.id()}", theme, on: theme == current.theme }
                }
            }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { class: "setting-title", "Accent" }
                    span { class: "dim-note", "Selection, focus and what is switched on." }
                }
                div { class: "accent-row",
                    for accent in Accent::ALL {
                        button {
                            key: "{accent.id()}",
                            // A scope of its own, so `--accent` inside it is this
                            // swatch's colour and not the current one; in the
                            // painted theme, so its ring still matches the page.
                            class: if accent == current.accent { "accent-swatch theme-scope on" } else { "accent-swatch theme-scope" },
                            "data-theme": "{painted}",
                            "data-accent": "{accent.id()}",
                            style: "background-color: var(--accent);",
                            title: "{accent.id()}",
                            "aria-label": "{accent.id()} accent",
                            onclick: move |_| choose(|a| a.accent = accent),
                        }
                    }
                }
            }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { class: "setting-title", "Density" }
                    span { class: "dim-note",
                        "Compact fits more in a desktop window. Touch screens always get full-size controls."
                    }
                }
                div { class: "segmented",
                    for density in Density::ALL {
                        button {
                            key: "{density.id()}",
                            class: if density == current.density { "on" } else { "" },
                            onclick: move |_| choose(|a| a.density = density),
                            "{density.label()}"
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ThemeCard(theme: Theme, on: bool) -> Element {
    let accent = APPEARANCE.read().accent.id();
    // Auto is both of the themes it can become, side by side.
    let panes: &[Theme] = match theme {
        Theme::Auto => &[Theme::Studio, Theme::Daylight],
        Theme::Studio => &[Theme::Studio],
        Theme::Midnight => &[Theme::Midnight],
        Theme::Console => &[Theme::Console],
        Theme::Daylight => &[Theme::Daylight],
        Theme::Contrast => &[Theme::Contrast],
    };
    rsx! {
        button {
            class: if on { "theme-card on" } else { "theme-card" },
            "aria-pressed": "{on}",
            onclick: move |_| choose(|a| a.theme = theme),
            div { class: "theme-preview",
                for pane in panes.iter() {
                    div {
                        key: "{pane.id()}",
                        class: "theme-preview-pane theme-scope",
                        "data-theme": "{pane.id()}",
                        "data-accent": "{accent}",
                        div { class: "theme-preview-rail" }
                        div { class: "theme-preview-body",
                            div { class: "theme-preview-text" }
                            div { class: "theme-preview-row" }
                            div { class: "theme-preview-row short" }
                        }
                        div { class: "theme-preview-dot" }
                    }
                }
            }
            span { class: "theme-card-name", "{theme.label()}" }
            span { class: "theme-card-note", "{theme.note()}" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_choice_survives_being_stored() {
        let a = Appearance {
            theme: Theme::Midnight,
            accent: Accent::Amber,
            density: Density::Compact,
        };
        assert_eq!(Appearance::decode(&a.encode()), a);
    }

    #[test]
    fn what_this_build_does_not_know_falls_back_without_taking_the_rest() {
        assert_eq!(Appearance::decode(""), Appearance::default());
        assert_eq!(Appearance::decode("garbage"), Appearance::default());
        let a = Appearance::decode("theme=neon;accent=teal;future=1;density=compact");
        assert_eq!(a.theme, Theme::default());
        assert_eq!(a.accent, Accent::Teal);
        assert_eq!(a.density, Density::Compact);
    }

    #[test]
    fn a_device_that_never_chose_gets_midnight() {
        assert_eq!(Appearance::default().theme, Theme::Midnight);
        assert_eq!(Appearance::decode("").theme, Theme::Midnight);
    }

    #[test]
    fn auto_follows_the_system_and_nothing_else_does() {
        assert_eq!(Theme::Auto.resolve(true), Theme::Daylight);
        assert_eq!(Theme::Auto.resolve(false), Theme::Studio);
        assert_eq!(Theme::Midnight.resolve(true), Theme::Midnight);
    }
}
