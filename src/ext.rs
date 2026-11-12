//! The Patchbay core as an Architect extension: one `linked::Extension`
//! (FastTrackStudios/studio#410).
//!
//! Linked into a distribution (Patchbay, Studio Architect), it adds the
//! `patchbay` view and the `PATCHBAY_*` actions. Both run over this
//! machine's one engine (`crate::home::engine`): the first action or view
//! that needs it starts it, or joins the engine another app already
//! serves (`fts-patchbay`, `patchbay serve`, another Architect app), and
//! takes over if that app quits. Nothing starts while the host only reads
//! what the extension declares (`Builder::of` runs [`Extension::build`]
//! for that too).
//!
//! The device families (`dante`, `coreaudio`, `antelope`, `yamaha`, the
//! `PipeWire` graph) are still inside the engine; each becomes its own
//! extension in the stack #410 lays out.

use architect_extensions::linked::{Builder, Extension};
use serde_json::Value;

use crate::proto::PatchbayServiceClient;

/// The extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Patchbay;

impl Extension for Patchbay {
    fn manifest(&self) -> &'static str {
        include_str!("../extension.toml")
    }

    fn build(&self, ext: &mut Builder) {
        ext.view("patchbay", crate::home::Home)
            .action("PATCHBAY_STATUS", |_| {
                call(|c| async move { crate::report::status(&c).await })
            })
            .action("PATCHBAY_HEALTH", |_| {
                call(|c| async move { json(crate::report::collect_health(&c).await) })
            })
            .action("PATCHBAY_NOW", |_| {
                call(|c| async move { json(c.host_overview().await) })
            })
            .action("PATCHBAY_DEVICE_LIST", |_| {
                call(|c| async move { json(c.list_devices().await) })
            })
            .action("PATCHBAY_DEVICE_SHOW", |args| async move {
                let id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .ok_or("give `id`")?
                    .to_owned();
                call(|c| async move { json(c.device(id).await) }).await
            });
    }
}

/// Run `f` with the engine's RPC client, on its runtime.
async fn call<F, Fut>(f: F) -> Result<Value, String>
where
    F: FnOnce(PatchbayServiceClient) -> Fut,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    crate::home::engine().await?.run(f).await?
}

/// A call's answer as JSON, its error as text: what `--json` prints.
fn json<T: serde::Serialize, E: std::fmt::Display>(r: Result<T, E>) -> Result<Value, String> {
    serde_json::to_value(r.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
