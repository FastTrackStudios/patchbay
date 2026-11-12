//! The page in a browser: the desktop app's webview and the browser
//! remote. `localStorage`, the system colour scheme and the page's own
//! chrome are browser things, done here with JavaScript; the Patchbay UI
//! only asks for them (`patchbay::ui::Page`). Architect's window has no
//! JavaScript and gives the UI a native page instead.

use std::rc::Rc;

use dioxus::prelude::*;
use patchbay::ui::Page;

/// Reports the system scheme now and on every change: `true` for light.
const SCHEME_JS: &str = r#"
const mq = window.matchMedia("(prefers-color-scheme: light)");
const report = () => { try { dioxus.send(mq.matches); } catch (_) {} };
report();
if (mq.addEventListener) { mq.addEventListener("change", report); } else { mq.addListener(report); }
await new Promise(() => {});
"#;

/// After the root has its theme: paint what is outside the app root too,
/// the page behind it (seen in overscroll and the home-indicator strip)
/// and the browser's own chrome via `theme-color`.
const PAINT_JS: &str = r#"
requestAnimationFrame(() => {
  const root = document.querySelector(".patchbay-root");
  if (!root) return;
  const css = getComputedStyle(root);
  const bg = css.getPropertyValue("--bg").trim();
  const surface = css.getPropertyValue("--surface").trim();
  document.documentElement.style.background = bg;
  document.body.style.background = surface;
  document.documentElement.style.colorScheme = css.colorScheme;
  let meta = document.querySelector('meta[name="theme-color"]');
  if (!meta) {
    meta = document.createElement("meta");
    meta.name = "theme-color";
    document.head.appendChild(meta);
  }
  meta.content = bg;
});
"#;

/// `s` as a JavaScript string literal: device keys and host names come
/// from config files, so a quote or backslash must not end it early.
fn js_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The page a browser gives the Patchbay UI.
pub fn browser() -> Page {
    Page {
        native: false,
        load: Some(Rc::new(|key: &'static str| {
            let js = format!(
                r#"try {{ return localStorage.getItem({}) || ""; }} catch (_) {{ return ""; }}"#,
                js_str(key)
            );
            Box::pin(async move {
                document::eval(&js)
                    .join::<String>()
                    .await
                    .ok()
                    .filter(|s| !s.is_empty())
            })
        })),
        save: Some(Rc::new(|key: &'static str, value: String| {
            document::eval(&format!(
                r"try {{ localStorage.setItem({}, {}); }} catch (_) {{}}",
                js_str(key),
                js_str(&value)
            ));
        })),
        watch_scheme: Some(Rc::new(|on: Callback<bool>| {
            spawn(async move {
                let mut watch = document::eval(SCHEME_JS);
                while let Ok(light) = watch.recv::<bool>().await {
                    on.call(light);
                }
            });
        })),
        paint_chrome: Some(Rc::new(|| {
            document::eval(PAINT_JS);
        })),
        coarse_pointer: Some(Rc::new(|| {
            Box::pin(async {
                document::eval(r#"return matchMedia("(pointer: coarse)").matches;"#)
                    .join::<bool>()
                    .await
                    .unwrap_or(false)
            })
        })),
    }
}
