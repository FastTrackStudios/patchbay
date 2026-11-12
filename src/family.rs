//! The device-family contract: what a family of devices gives the hub
//! (FastTrackStudios/studio#410, question 1).
//!
//! A family is one kind of device Patchbay routes: a Yamaha TF console,
//! an Antelope Galaxy32, the Dante network, this machine's own audio.
//! It knows how to find and connect one device of its kind
//! ([`DeviceFamily::connect`]); everything after that (supervision,
//! reconnects, events, the RPC, the CLI, the generic device view,
//! snapshots) is the hub's, generic over [`DynDeviceAdapter`].
//!
//! The hub runs over a `Families` registry: the built-in families, then
//! every family [`register`]ed in this process (a family extension
//! registers itself from its `build`), which replaces a built-in of the
//! same kind. Register before the engine starts: the engine reads the
//! registrations once, when it starts.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use crate::device::DynDeviceAdapter;
use crate::proto::{DeviceConfig, FamilyInfo};
pub use crate::server::devices::cache::DiscoveryCache;

/// A connected device, type-erased.
pub type Adapter = Arc<dyn DynDeviceAdapter>;

/// What [`DeviceFamily::connect`] returns.
pub type ConnectFuture = Pin<Box<dyn Future<Output = Result<Adapter, ConnectError>> + Send>>;

/// Why a connect attempt failed. Connects fail soft: they return this
/// and never block the other devices. (A connect that panics is caught by
/// the hub and counts as [`ConnectError::Failed`].)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectError {
    /// Discovery ran and found nothing (`device list` says *not found*).
    #[error("not found: {0}")]
    NotFound(String),
    /// Found, or pinned, but the connection failed (*offline*).
    #[error("failed: {0}")]
    Failed(String),
}

impl ConnectError {
    /// The reason, as `device list` shows it.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::NotFound(m) | Self::Failed(m) => m,
        }
    }
}

/// What the hub hands a family's connect: shared state every family may
/// use.
#[derive(Clone)]
pub struct ConnectCtx {
    /// Last-found addresses of auto-discovered devices.
    pub(crate) cache: Arc<DiscoveryCache>,
}

impl ConnectCtx {
    /// A context over `cache`.
    #[must_use]
    pub(crate) const fn new(cache: Arc<DiscoveryCache>) -> Self {
        Self { cache }
    }

    /// A context over an empty, in-memory discovery cache: what a family's
    /// own tests hand its `connect`, without starting an engine (feature
    /// `test-util`). Seed it with [`DiscoveryCache::remember`] to test the
    /// cached-address path.
    #[cfg(feature = "test-util")]
    #[must_use]
    pub fn in_memory() -> Self {
        Self::new(Arc::new(DiscoveryCache::memory()))
    }

    /// Where auto-discovered devices were last found, by config name: read
    /// it to connect straight away, [`DiscoveryCache::remember`] what
    /// discovery found, [`DiscoveryCache::forget`] an address that stopped
    /// answering.
    #[must_use]
    pub fn cache(&self) -> &DiscoveryCache {
        &self.cache
    }
}

fn registrations() -> &'static RwLock<Vec<Arc<dyn DeviceFamily>>> {
    static REGISTERED: OnceLock<RwLock<Vec<Arc<dyn DeviceFamily>>>> = OnceLock::new();
    REGISTERED.get_or_init(|| RwLock::new(Vec::new()))
}

/// Whether this host runs device families at all. A phone or a tablet
/// does not: Patchbay there controls the engines on other machines, has
/// no audio graph or driver of its own and scans the LAN for nothing
/// (`patchbay-ios`, studio#466). There the families register nothing and
/// the engine assembles none ([`assemble`]); their views still register,
/// so a phone looking at another machine's engine draws its consoles.
pub(crate) const FAMILIES_HERE: bool = !cfg!(target_os = "ios");

/// Add `family` to every engine this process starts from now on.
///
/// A family extension calls this from its `build`. It replaces a built-in
/// family, or an earlier registration, of the same kind; the one it
/// replaced is returned. An engine already running keeps the families it
/// started with. On a host that runs no device families (iOS,
/// `FAMILIES_HERE`) it registers nothing.
pub fn register(family: Arc<dyn DeviceFamily>) -> Option<Arc<dyn DeviceFamily>> {
    register_on(registrations(), family, FAMILIES_HERE)
}

/// [`register`] into `list`, with whether this host runs families given:
/// a test passes its own list and can be the phone.
fn register_on(
    list: &RwLock<Vec<Arc<dyn DeviceFamily>>>,
    family: Arc<dyn DeviceFamily>,
    families_here: bool,
) -> Option<Arc<dyn DeviceFamily>> {
    if !families_here {
        tracing::debug!(
            kind = family.kind(),
            "patchbay: no device families on this host; the family is not registered"
        );
        return None;
    }
    let mut list = list.write().unwrap_or_else(PoisonError::into_inner);
    replace_or_push(&mut list, family)
}

/// The families an engine runs: the built-ins, then every registration
/// (one of the same kind replaces the built-in); none at all on a host
/// that runs no families. Where the engine's families are assembled, so
/// the gate holds for every family, built in or an extension.
pub(crate) fn assemble(
    builtin: Families,
    registered: Vec<Arc<dyn DeviceFamily>>,
    families_here: bool,
) -> Families {
    if !families_here {
        return Families::new();
    }
    let mut families = builtin;
    for family in registered {
        let _ = families.register(family);
    }
    families
}

/// [`register`] `family` and its views, for one kind, in one call.
///
/// What a family extension with views of its own calls, before the
/// engine and the window start: the views go to the window's registry
/// ([`crate::ui::register_family_views`]), the family to the engine's.
/// On a host that runs no families (iOS) only the views register.
///
/// # Panics
/// In debug builds, when `views` is for another kind than `family`.
#[cfg(feature = "ui")]
pub fn register_with_views(
    family: Arc<dyn DeviceFamily>,
    views: crate::ui::FamilyViews,
) -> Option<Arc<dyn DeviceFamily>> {
    debug_assert_eq!(
        family.kind(),
        views.kind,
        "a family's views registered under another kind"
    );
    crate::ui::register_family_views(views);
    register(family)
}

/// What doesn't line up between the engine's families and the window's
/// views, as one line each: a family with no views of its own (its
/// devices draw with the generic view, fine if meant: register its views
/// with [`register_with_views`] otherwise), and views for a kind no family
/// connects (never drawn).
#[cfg(feature = "ui")]
pub(crate) fn views_mismatch(families: &[&str], views: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = families
        .iter()
        .filter(|k| !views.contains(k))
        .map(|k| {
            format!("family '{k}' has no views: its devices draw with the generic device view")
        })
        .collect();
    out.extend(
        views
            .iter()
            .filter(|k| !families.contains(k))
            .map(|k| format!("views for '{k}', which no family connects: they are never drawn")),
    );
    out
}

/// What the engine warns of at start: [`views_mismatch`], only where the
/// host runs families. On a phone none runs while every family's views
/// are registered, to draw other machines' devices: that is the design,
/// not a mismatch.
#[cfg(feature = "ui")]
pub(crate) fn start_warnings(
    families: &[&str],
    views: &[&str],
    families_here: bool,
) -> Vec<String> {
    if families_here {
        views_mismatch(families, views)
    } else {
        Vec::new()
    }
}

/// The families [`register`]ed in this process, in order.
#[must_use]
pub(crate) fn registered() -> Vec<Arc<dyn DeviceFamily>> {
    registrations()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Put `family` in place of the one of its kind in `list` (warning, and
/// returning the one replaced), or at the end.
fn replace_or_push(
    list: &mut Vec<Arc<dyn DeviceFamily>>,
    family: Arc<dyn DeviceFamily>,
) -> Option<Arc<dyn DeviceFamily>> {
    let kind = family.kind();
    if let Some(slot) = list.iter_mut().find(|f| f.kind() == kind) {
        tracing::warn!(
            kind,
            "patchbay: a device family replaces the one registered for its kind"
        );
        return Some(std::mem::replace(slot, family));
    }
    list.push(family);
    None
}

/// One family of devices.
pub trait DeviceFamily: Send + Sync + 'static {
    /// The config `kind` this family connects (`yamaha-tf`,
    /// `antelope-galaxy32`, `dante`, `system-audio`). One family per kind.
    fn kind(&self) -> &'static str;

    /// What it is, where it runs, and whether it can run here.
    fn info(&self) -> FamilyInfo;

    /// The entries it adds to a config that has no `devices` section.
    fn defaults(&self) -> Vec<DeviceConfig> {
        Vec::new()
    }

    /// Find (when `config.addr` is empty) and connect one device. Fails
    /// soft, telling "found nothing" ([`ConnectError::NotFound`]) from
    /// "found but unreachable" ([`ConnectError::Failed`]).
    fn connect(&self, config: DeviceConfig, ctx: ConnectCtx) -> ConnectFuture;
}

/// The families the hub runs over, in registration order (which is the
/// order of the default device list).
#[derive(Clone, Default)]
pub(crate) struct Families {
    list: Vec<Arc<dyn DeviceFamily>>,
}

impl Families {
    /// No families.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Add `family`; one already registered for its kind is replaced, in
    /// place (with a warning).
    #[must_use]
    pub(crate) fn with(mut self, family: impl DeviceFamily) -> Self {
        let _ = self.register(Arc::new(family));
        self
    }

    /// Add `family`. One already registered for its kind is replaced, in
    /// place, with a warning, and returned.
    pub(crate) fn register(
        &mut self,
        family: Arc<dyn DeviceFamily>,
    ) -> Option<Arc<dyn DeviceFamily>> {
        replace_or_push(&mut self.list, family)
    }

    /// Every family, in order.
    #[must_use]
    pub(crate) fn all(&self) -> &[Arc<dyn DeviceFamily>] {
        &self.list
    }

    /// The family for config kind `kind`.
    #[must_use]
    pub(crate) fn find(&self, kind: &str) -> Option<&Arc<dyn DeviceFamily>> {
        self.list.iter().find(|f| f.kind() == kind)
    }

    /// Every registered kind, in order.
    #[must_use]
    pub(crate) fn kinds(&self) -> Vec<&'static str> {
        self.list.iter().map(|f| f.kind()).collect()
    }

    /// The default device list: each family's defaults, in order.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn defaults(&self) -> Vec<DeviceConfig> {
        self.list.iter().flat_map(|f| f.defaults()).collect()
    }

    /// What each family is, in order, unguarded: tests only (the hub asks
    /// each family once, guarded, `DeviceHub::family_infos`).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn infos(&self) -> Vec<FamilyInfo> {
        self.list.iter().map(|f| f.info()).collect()
    }
}

#[cfg(all(test, feature = "ui"))]
mod tests {
    use super::*;

    #[test]
    fn a_phone_warns_of_no_views_without_a_family() {
        // A phone: no families, every family's views (to draw another
        // machine's devices). Nothing to warn of.
        assert_eq!(
            super::start_warnings(&[], &["yamaha-tf", "dante"], false),
            Vec::<String>::new()
        );
        // The same on a host that runs families is a mismatch.
        assert_eq!(
            super::start_warnings(&[], &["yamaha-tf", "dante"], true).len(),
            2
        );
    }

    /// `register_family!` with both sides on (this crate's tests have
    /// `server` and `ui`): the family and its views, once.
    #[test]
    fn register_family_registers_the_family_and_its_views_once() {
        struct Macro;
        impl DeviceFamily for Macro {
            fn kind(&self) -> &'static str {
                "macro-desk"
            }
            fn info(&self) -> FamilyInfo {
                FamilyInfo::linked("macro-desk", "Macro desk", &[], false)
            }
            fn connect(&self, _config: DeviceConfig, _ctx: ConnectCtx) -> ConnectFuture {
                Box::pin(async { Err(ConnectError::NotFound(String::new())) })
            }
        }
        fn register() {
            crate::register_family!(family: Macro, views: crate::ui::FamilyViews::new("macro-desk"));
        }
        register();
        register();
        let n = registered()
            .iter()
            .filter(|f| f.kind() == "macro-desk")
            .count();
        assert_eq!(n, 1);
        assert!(crate::ui::family_view_kinds().contains(&"macro-desk"));
    }

    #[test]
    fn families_and_views_that_dont_line_up_are_named() {
        assert_eq!(
            views_mismatch(&["dante", "yamaha-tf"], &["yamaha-tf", "dante"]),
            Vec::<String>::new()
        );
        let out = views_mismatch(&["dante", "loner"], &["dante", "orphan"]);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out[0].contains("'loner' has no views"), "{out:?}");
        assert!(out[1].contains("views for 'orphan'"), "{out:?}");
    }
}

/// The iOS gate, with the host given: CI builds no iOS target, so the
/// phone is `families_here = false` here.
#[cfg(test)]
mod gate_tests {
    use super::*;

    struct Probe(&'static str);

    impl DeviceFamily for Probe {
        fn kind(&self) -> &'static str {
            self.0
        }

        fn info(&self) -> FamilyInfo {
            FamilyInfo::linked(self.0, self.0, &[], true)
        }

        fn defaults(&self) -> Vec<DeviceConfig> {
            vec![DeviceConfig::new(self.0, self.0)]
        }

        fn connect(&self, _config: DeviceConfig, _ctx: ConnectCtx) -> ConnectFuture {
            Box::pin(async { Err(ConnectError::NotFound("a probe".to_owned())) })
        }
    }

    fn kinds(families: &Families) -> Vec<&'static str> {
        families.kinds()
    }

    #[test]
    fn a_phone_registers_no_family() {
        // A list of its own, not the process's: nothing leaks into another
        // test sharing the process (plain `cargo test`).
        let list = RwLock::new(Vec::new());
        let kinds = |list: &RwLock<Vec<Arc<dyn DeviceFamily>>>| -> Vec<&'static str> {
            list.read().unwrap().iter().map(|f| f.kind()).collect()
        };
        assert!(register_on(&list, Arc::new(Probe("gate-phone")), false).is_none());
        assert_eq!(kinds(&list), Vec::<&str>::new());
        // A host that runs families does register it.
        assert!(register_on(&list, Arc::new(Probe("gate-host")), true).is_none());
        assert_eq!(kinds(&list), ["gate-host"]);
    }

    #[test]
    fn a_phone_assembles_no_families_built_in_or_registered() {
        let builtin = || {
            Families::new()
                .with(Probe("builtin-a"))
                .with(Probe("builtin-b"))
        };
        let registered: Vec<Arc<dyn DeviceFamily>> =
            vec![Arc::new(Probe("extension")), Arc::new(Probe("builtin-b"))];
        let phone = assemble(builtin(), registered.clone(), false);
        assert_eq!(kinds(&phone), Vec::<&str>::new());
        assert_eq!(phone.defaults().len(), 0, "a phone would look for devices");
        // Elsewhere: the built-ins, a registration of a built-in's kind in
        // its place, then the new kind.
        let host = assemble(builtin(), registered, true);
        assert_eq!(kinds(&host), ["builtin-a", "builtin-b", "extension"]);
    }

    /// On iOS itself, the constant is the gate (a test that runs only
    /// where it can).
    #[cfg(target_os = "ios")]
    #[test]
    fn ios_runs_no_families() {
        assert!(!FAMILIES_HERE);
    }
}
