//! The views each device family contributes, looked up by config `kind`
//! (studio#416).
//!
//! The Route and Mix pages and the device console ask here for the view
//! of the selected device's family; a kind that registered nothing gets
//! the generic device view (parameter sections, the crosspoint grid,
//! snapshots), which is drawn from the device's data alone. So any family
//! works without a view of its own, and a family adds one by registering
//! it: the built-ins below do, and so will each family extension
//! (studio#410). A process family's own view waits for
//! FastTrackStudios/architect#579; until then it gets the generic one.

use std::sync::{OnceLock, PoisonError, RwLock};

use dioxus::prelude::*;

use crate::proto::DeviceView;

/// What one family draws, by page. `None` leaves that page to the
/// generic device view.
#[derive(Clone, Copy)]
pub struct FamilyViews {
    /// The config `kind` these are for (`dante`).
    pub kind: &'static str,
    /// The Route page for this kind's devices.
    pub route: Option<fn() -> Element>,
    /// The Mix page for this kind's devices.
    pub mix: Option<fn() -> Element>,
    /// A console drawn above the generic device view (a TF's strips, a
    /// Galaxy32's mixer), given the device's full read.
    pub console: Option<fn(DeviceView) -> Element>,
}

impl FamilyViews {
    /// Nothing of its own, for `kind`: fill in what it draws.
    #[must_use]
    pub const fn new(kind: &'static str) -> Self {
        Self {
            kind,
            route: None,
            mix: None,
            console: None,
        }
    }
}

fn registry() -> &'static RwLock<Vec<FamilyViews>> {
    static VIEWS: OnceLock<RwLock<Vec<FamilyViews>>> = OnceLock::new();
    VIEWS.get_or_init(|| RwLock::new(builtin()))
}

/// Register `views` for its kind, replacing what was there.
pub fn register(views: FamilyViews) {
    let mut list = registry().write().unwrap_or_else(PoisonError::into_inner);
    match list.iter_mut().find(|v| v.kind == views.kind) {
        Some(slot) => *slot = views,
        None => list.push(views),
    }
}

/// The views registered for `kind`, if any.
#[must_use]
pub fn views(kind: &str) -> Option<FamilyViews> {
    registry()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|v| v.kind == kind)
        .copied()
}

/// The built-in families' views, as each family extension will register
/// its own.
fn builtin() -> Vec<FamilyViews> {
    use crate::ui::devices::{galaxy32, yamaha_tf};
    use crate::ui::pages;
    vec![
        FamilyViews {
            route: Some(pages::SystemRoute),
            mix: Some(crate::ui::mixes::MixesView),
            ..FamilyViews::new("system-audio")
        },
        FamilyViews {
            route: Some(crate::ui::dante_grid::DanteGrid),
            mix: Some(pages::OnlyRoutes),
            ..FamilyViews::new("dante")
        },
        FamilyViews {
            console: Some(|view| rsx! { yamaha_tf::YamahaTfConsole { view } }),
            ..FamilyViews::new("yamaha-tf")
        },
        FamilyViews {
            console: Some(|view| rsx! { galaxy32::Galaxy32Console { view } }),
            ..FamilyViews::new("antelope-galaxy32")
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[component]
    fn FakeRoute() -> Element {
        rsx! { div { "data-fake-route": "yes", "the fake family's router" } }
    }

    #[component]
    fn Page() -> Element {
        rsx! { crate::ui::pages::FamilyRoute { kind: "fake-desk".to_owned() } }
    }

    #[test]
    fn a_registered_familys_route_is_what_the_route_page_draws() {
        register(FamilyViews {
            route: Some(FakeRoute),
            ..FamilyViews::new("fake-desk")
        });
        let doc = dioxus_test::render(Page).build();
        assert!(
            doc.query("[data-fake-route=\"yes\"]").immediately().is_ok(),
            "the Route page did not draw the family's own view"
        );
    }

    #[test]
    fn an_unregistered_kind_gets_the_generic_device_view() {
        assert!(views("no-such-kind").is_none());
        // The built-in consoles answer only for their own kinds.
        assert!(views("yamaha-tf").and_then(|v| v.console).is_some());
        assert!(views("yamaha-tf").and_then(|v| v.route).is_none());
        assert!(views("dante").and_then(|v| v.route).is_some());
        assert!(views("system-audio").and_then(|v| v.mix).is_some());
    }
}
