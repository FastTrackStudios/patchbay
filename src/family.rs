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
//! The hub runs over a [`Families`] registry: the built-in families, then
//! every family [`register`]ed in this process (a family extension
//! registers itself from its `build`), which replaces a built-in of the
//! same kind. Register before the engine starts: the engine reads the
//! registrations once, when it starts.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use crate::device::DynDeviceAdapter;
use crate::proto::{DeviceConfig, FamilyInfo};
pub use crate::server::DiscoveryCache;

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

/// Add `family` to every engine this process starts from now on.
///
/// A family extension calls this from its `build`. It replaces a built-in
/// family, or an earlier registration, of the same kind; the one it
/// replaced is returned. An engine already running keeps the families it
/// started with.
pub fn register(family: Arc<dyn DeviceFamily>) -> Option<Arc<dyn DeviceFamily>> {
    let mut list = registrations()
        .write()
        .unwrap_or_else(PoisonError::into_inner);
    replace_or_push(&mut list, family)
}

/// The families [`register`]ed in this process, in order.
#[must_use]
pub fn registered() -> Vec<Arc<dyn DeviceFamily>> {
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
pub struct Families {
    list: Vec<Arc<dyn DeviceFamily>>,
}

impl Families {
    /// No families.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `family`; one already registered for its kind is replaced, in
    /// place (with a warning).
    #[must_use]
    pub fn with(mut self, family: impl DeviceFamily) -> Self {
        let _ = self.register(Arc::new(family));
        self
    }

    /// Add `family`. One already registered for its kind is replaced, in
    /// place, with a warning, and returned.
    pub fn register(&mut self, family: Arc<dyn DeviceFamily>) -> Option<Arc<dyn DeviceFamily>> {
        replace_or_push(&mut self.list, family)
    }

    /// Every family, in order.
    #[must_use]
    pub fn all(&self) -> &[Arc<dyn DeviceFamily>] {
        &self.list
    }

    /// The family for config kind `kind`.
    #[must_use]
    pub fn find(&self, kind: &str) -> Option<&Arc<dyn DeviceFamily>> {
        self.list.iter().find(|f| f.kind() == kind)
    }

    /// Every registered kind, in order.
    #[must_use]
    pub fn kinds(&self) -> Vec<&'static str> {
        self.list.iter().map(|f| f.kind()).collect()
    }

    /// The default device list: each family's defaults, in order.
    #[must_use]
    pub fn defaults(&self) -> Vec<DeviceConfig> {
        self.list.iter().flat_map(|f| f.defaults()).collect()
    }

    /// What each family is, in order (`patchbay families`).
    #[must_use]
    pub fn infos(&self) -> Vec<FamilyInfo> {
        self.list.iter().map(|f| f.info()).collect()
    }
}
