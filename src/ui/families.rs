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
//! (Its views will need a form that takes the device id rather than a
//! `fn` pointer into this process.)
//!
//! Besides its pages, a family says how the shell presents its devices:
//! the rail label, the note in the context switcher, the icon, whether
//! there is a device read to load, a note above the router. The shell
//! matches on no kind; a kind with nothing registered gets the hardware
//! defaults.
//!
//! **Register before launch.** The registry is read when a page draws,
//! and registering changes no signal, so a family registered after the
//! window is up shows on the next redraw at best. A family extension
//! registers from its `build`, its views here and its device family with
//! the engine (studio#415); one call that does both, for one kind, comes
//! with the first family extension (studio#417).

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
    /// The rail's name for the device, where its model doesn't fit
    /// (`Dante`). `None`: the model, else the config name.
    pub label: Option<&'static str>,
    /// What the device is, in the context switcher ("the Dante
    /// network"). `None`: the config name.
    pub what: Option<&'static str>,
    /// The icon's shapes, drawn in a 24×24 stroked `svg` (`ContextIcon`).
    /// `None`: hardware (two rack units).
    pub icon: Option<fn() -> Element>,
    /// Whether the pages need the device's read (`device(id)`). `false`
    /// for a family whose views draw from elsewhere: Dante's grid comes
    /// from the subscription scan, and reading the network as a device
    /// costs a second sweep for nothing.
    pub reads_device: bool,
    /// A note above the generic router (the Galaxy32's lag on changes
    /// made at the hardware).
    pub route_note: Option<&'static str>,
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
            label: None,
            what: None,
            icon: None,
            reads_device: true,
            route_note: None,
        }
    }
}

fn registry() -> &'static RwLock<Vec<FamilyViews>> {
    static VIEWS: OnceLock<RwLock<Vec<FamilyViews>>> = OnceLock::new();
    VIEWS.get_or_init(|| RwLock::new(builtin()))
}

/// Register `views` for its kind, replacing what was there. Before
/// launch: see the module docs.
pub fn register(views: FamilyViews) {
    let mut list = registry().write().unwrap_or_else(PoisonError::into_inner);
    match list.iter_mut().find(|v| v.kind == views.kind) {
        Some(slot) => *slot = views,
        None => list.push(views),
    }
}

/// Draw `view` as a component of its own (`name` is what devtools and
/// panics call it), not by calling it: its hooks then live in its own
/// scope, and a different view (another family, or a registration that
/// replaced this one) is a different component, so its hooks start fresh
/// rather than inheriting another view's.
pub fn mount(view: fn() -> Element, name: &'static str) -> Element {
    use dioxus::dioxus_vdom::{DynamicNode, VComponent};
    rsx! { {DynamicNode::Component(VComponent::new(view, (), name))} }
}

/// Draw `console` for `view` as a component of its own, the way [`mount`]
/// draws a page: the component's identity is the console function
/// itself, so a different console (another family's, or a registration
/// that replaced this kind's) gets a fresh scope instead of the hooks of
/// the one drawn before.
pub fn mount_console(console: fn(DeviceView) -> Element, view: DeviceView) -> Element {
    use dioxus::dioxus_vdom::{DynamicNode, VComponent};
    rsx! {
        {DynamicNode::Component(VComponent::new(ConsoleFn(console), ConsoleProps { view }, "FamilyConsole"))}
    }
}

/// A console's props: the device's full read.
#[derive(Props, Clone, PartialEq)]
struct ConsoleProps {
    view: DeviceView,
}

/// A console function as a component: rendered with its view, identified
/// by its own address.
#[derive(Clone, Copy)]
struct ConsoleFn(fn(DeviceView) -> Element);

/// Tells dioxus which `ComponentFunction` impl this is.
struct ConsoleMarker;

impl dioxus::dioxus_vdom::ComponentFunction<ConsoleProps, ConsoleMarker> for ConsoleFn {
    // The console function's address is the component's identity: what
    // dioxus compares to tell one console from another.
    #[allow(clippy::as_conversions, clippy::fn_to_numeric_cast_any)]
    fn fn_ptr(&self) -> usize {
        self.0 as usize
    }

    fn rebuild(&self, props: ConsoleProps) -> Element {
        (self.0)(props.view)
    }
}

/// Every kind with views registered, in order.
#[cfg(feature = "server")]
pub(crate) fn kinds() -> Vec<&'static str> {
    registry()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|v| v.kind)
        .collect()
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
    use crate::ui::devices::galaxy32;
    use crate::ui::{icons, pages};
    vec![
        FamilyViews {
            route: Some(pages::SystemRoute),
            mix: Some(crate::ui::mixes::MixesView),
            what: Some("this machine's audio"),
            icon: Some(icons::MachineIcon),
            ..FamilyViews::new("system-audio")
        },
        FamilyViews {
            route: Some(crate::ui::dante_grid::DanteGrid),
            mix: Some(pages::OnlyRoutes),
            // "Dante network" doesn't fit the rail; the icon says network.
            label: Some("Dante"),
            what: Some("the Dante network"),
            icon: Some(icons::NetworkIcon),
            reads_device: false,
            ..FamilyViews::new("dante")
        },
        FamilyViews {
            console: Some(|view| rsx! { galaxy32::Galaxy32Console { view } }),
            route_note: Some(galaxy32::ROUTER_LAG),
            ..FamilyViews::new("antelope-galaxy32")
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{DeviceSummary, ParamView};

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

    // Two families' Route views with different hooks: drawn by calling
    // them in one scope, switching from one to the other would hand the
    // second the first's hook (a panic). Mounted, each has its own.
    #[component]
    fn SignalRoute() -> Element {
        let word = use_signal(|| "first".to_owned());
        rsx! { div { "data-view": "{word}" } }
    }

    #[component]
    fn HookRoute() -> Element {
        let n = use_hook(|| 7_u32);
        rsx! { div { "data-view": "second-{n}" } }
    }

    #[component]
    fn SwitchingPage() -> Element {
        let mut second = use_signal(|| false);
        let kind = if second() { "hook-desk" } else { "signal-desk" };
        rsx! {
            button { onclick: move |_| second.set(true), "switch" }
            // Unkeyed on purpose: the same FamilyRoute scope draws both.
            crate::ui::pages::FamilyRoute { kind: kind.to_owned() }
        }
    }

    #[tokio::test]
    async fn each_familys_view_is_a_component_of_its_own() {
        register(FamilyViews {
            route: Some(SignalRoute),
            ..FamilyViews::new("signal-desk")
        });
        register(FamilyViews {
            route: Some(HookRoute),
            ..FamilyViews::new("hook-desk")
        });
        let doc = dioxus_test::render(SwitchingPage).build();
        assert!(doc.query("[data-view=\"first\"]").immediately().is_ok());
        doc.query("button").click().await.unwrap();
        doc.pump().await.unwrap();
        assert!(
            doc.query("[data-view=\"second-7\"]").immediately().is_ok(),
            "the second family's view did not draw with hooks of its own"
        );
    }

    // The same kind registered again with another view while one is on
    // screen, then a redraw: drawn by calling, the new view would get the
    // old one's hooks (a panic). Mounted, it gets a scope of its own.
    // Console functions that use hooks themselves, as a family outside
    // the crate may write one (not a closure that mounts a component).
    #[allow(clippy::needless_pass_by_value)]
    fn console_with_signal(_view: crate::proto::DeviceView) -> Element {
        let word = use_signal(|| "old".to_owned());
        rsx! { div { "data-console": "{word}" } }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn console_with_hook(view: crate::proto::DeviceView) -> Element {
        let n = use_hook(|| 9_u32);
        rsx! { div { "data-console": "new-{n}-{view.params.len()}" } }
    }

    fn device(kind: &str, params: usize) -> crate::proto::DeviceView {
        crate::proto::DeviceView {
            summary: summary(kind, "desk", "Desk"),
            inputs: Vec::new(),
            outputs: Vec::new(),
            routes: Vec::new(),
            params: (0..params)
                .map(|i| ParamView {
                    path: format!("p/{i}"),
                    label: String::new(),
                    kind: crate::proto::DeviceParamKind::Toggle,
                    value: crate::proto::DeviceParamValue::Toggle(false),
                    writable: false,
                    disruptive: false,
                })
                .collect(),
        }
    }

    #[component]
    fn ReRegisteringConsolePage() -> Element {
        let mut view = use_signal(|| device("swap-console", 0));
        rsx! {
            button {
                onclick: move |_| {
                    register(FamilyViews {
                        console: Some(console_with_hook),
                        ..FamilyViews::new("swap-console")
                    });
                    // A device event: the same device, one more param.
                    view.set(device("swap-console", 1));
                },
                "re-register"
            }
            crate::ui::devices::Console { view: view() }
        }
    }

    #[tokio::test]
    async fn a_console_re_registered_for_its_kind_draws_with_hooks_of_its_own() {
        register(FamilyViews {
            console: Some(console_with_signal),
            ..FamilyViews::new("swap-console")
        });
        let doc = dioxus_test::render(ReRegisteringConsolePage).build();
        assert!(doc.query("[data-console=\"old\"]").immediately().is_ok());
        doc.query("button").click().await.unwrap();
        doc.pump().await.unwrap();
        assert!(
            doc.query("[data-console=\"new-9-1\"]")
                .immediately()
                .is_ok(),
            "the re-registered console did not draw with hooks of its own"
        );
    }

    // The same for a page view: one kind, its route replaced, a redraw.
    #[component]
    fn ReRegisteringRoutePage() -> Element {
        let mut redraws = use_signal(|| 0_u32);
        let route = views("swap-route").and_then(|v| v.route).unwrap();
        rsx! {
            button {
                onclick: move |_| {
                    register(FamilyViews {
                        route: Some(HookRoute),
                        ..FamilyViews::new("swap-route")
                    });
                    redraws += 1;
                },
                "re-register"
            }
            span { "{redraws}" }
            {mount(route, "FamilyRoute")}
        }
    }

    #[tokio::test]
    async fn a_route_re_registered_for_its_kind_draws_with_hooks_of_its_own() {
        register(FamilyViews {
            route: Some(SignalRoute),
            ..FamilyViews::new("swap-route")
        });
        let doc = dioxus_test::render(ReRegisteringRoutePage).build();
        assert!(doc.query("[data-view=\"first\"]").immediately().is_ok());
        doc.query("button").click().await.unwrap();
        doc.pump().await.unwrap();
        assert!(
            doc.query("[data-view=\"second-7\"]").immediately().is_ok(),
            "the re-registered route did not draw with hooks of its own"
        );
    }

    fn summary(kind: &str, name: &str, model: &str) -> DeviceSummary {
        DeviceSummary {
            id: String::new(),
            name: name.to_owned(),
            kind: kind.to_owned(),
            vendor: String::new(),
            model: model.to_owned(),
            serial: String::new(),
            firmware: String::new(),
            transport: String::new(),
            state: crate::proto::DeviceLinkState::Online,
            error: String::new(),
        }
    }

    #[test]
    fn the_shell_presents_a_device_as_its_family_says() {
        use crate::ui::devices::{context_label, context_what};
        // Nothing registered: the model (else the name), the config name.
        let plain = summary("plain-desk", "foh", "Desk 16");
        assert_eq!(context_label(&plain), "Desk 16");
        assert_eq!(context_what(&plain), "foh");
        assert_eq!(context_label(&summary("plain-desk", "foh", "")), "foh");
        // A family that says otherwise.
        register(FamilyViews {
            label: Some("Ring"),
            what: Some("the ring network"),
            reads_device: false,
            route_note: Some("slow to report"),
            ..FamilyViews::new("ring-net")
        });
        let ring = summary("ring-net", "ring", "Ring Controller");
        assert_eq!(context_label(&ring), "Ring");
        assert_eq!(context_what(&ring), "the ring network");
        let v = views("ring-net").unwrap();
        assert!(!v.reads_device);
        assert_eq!(v.route_note, Some("slow to report"));
        // The built-ins moved out of the shell's matches.
        assert_eq!(context_label(&summary("dante", "dante", "")), "Dante");
        assert_eq!(
            context_what(&summary("dante", "dante", "")),
            "the Dante network"
        );
        assert!(!views("dante").unwrap().reads_device);
        assert!(views("system-audio").unwrap().icon.is_some());
        assert!(views("antelope-galaxy32").unwrap().route_note.is_some());
        assert!(views("antelope-galaxy32").unwrap().reads_device);
    }

    #[component]
    fn Icons() -> Element {
        rsx! {
            div { "data-icon": "dante", crate::ui::icons::ContextIcon { kind: "dante".to_owned() } }
            div { "data-icon": "other", crate::ui::icons::ContextIcon { kind: "no-such-kind".to_owned() } }
        }
    }

    #[test]
    fn a_familys_icon_draws_inside_the_rail_icon() {
        let doc = dioxus_test::render(Icons).build();
        // Dante's network: three nodes. Anything else: two rack units.
        assert_eq!(
            doc.query_all("[data-icon=dante] svg circle")
                .immediately()
                .len(),
            3
        );
        assert_eq!(
            doc.query_all("[data-icon=other] svg rect")
                .immediately()
                .len(),
            2
        );
    }

    #[test]
    fn an_unregistered_kind_gets_the_generic_device_view() {
        assert!(views("no-such-kind").is_none());
        // The built-in consoles answer only for their own kinds.
        assert!(views("antelope-galaxy32").and_then(|v| v.console).is_some());
        assert!(views("antelope-galaxy32").and_then(|v| v.route).is_none());
        assert!(views("dante").and_then(|v| v.route).is_some());
        assert!(views("system-audio").and_then(|v| v.mix).is_some());
    }
}
