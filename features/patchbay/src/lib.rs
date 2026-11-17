//! Studio routing: the patchbay server (host audio graph, Core Audio, Dante adapter), its wire types, and its UI

#[cfg(feature = "server")]
pub mod coreaudio;
#[cfg(feature = "server")]
pub mod dante;
#[cfg(feature = "server")]
pub mod device;
#[cfg(feature = "extension")]
pub mod ext;
#[cfg(feature = "server")]
pub mod family;
#[cfg(all(feature = "server", feature = "ui"))]
pub mod home;
#[cfg(feature = "server")]
pub mod host;
pub mod lan;
pub mod proto;
#[cfg(feature = "server")]
pub mod report;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "ui")]
pub mod ui;

/// Register a device family and its views once, from the family crate's
/// `register()`, whichever of the family crate's own features are on:
///
/// - `server` and `ui`: [`family::register_with_views`] (both, one kind);
/// - `server` only: [`family::register`];
/// - `ui` only (the browser remote, a phone): the views alone.
///
/// The `cfg`s are the calling crate's (a macro's `cfg` is evaluated where
/// it expands), so a family crate names its features `server` and `ui`.
/// `family` and `views` are expressions, evaluated only under the features
/// that have them. Later calls do nothing. On a host that runs no families
/// (iOS) the family side registers nothing (`family::register`'s gate).
///
/// ```ignore
/// pub fn register() {
///     patchbay::register_family!(family: family::YamahaTfFamily, views: ui::views());
/// }
/// ```
#[macro_export]
macro_rules! register_family {
    (family: $family:expr, views: $views:expr $(,)?) => {{
        static ONCE: ::std::sync::Once = ::std::sync::Once::new();
        ONCE.call_once(|| {
            #[cfg(all(feature = "server", feature = "ui"))]
            {
                let _ = $crate::family::register_with_views(::std::sync::Arc::new($family), $views);
            }
            #[cfg(all(feature = "server", not(feature = "ui")))]
            {
                let _ = $crate::family::register(::std::sync::Arc::new($family));
            }
            #[cfg(all(feature = "ui", not(feature = "server")))]
            {
                $crate::ui::register_family_views($views);
            }
        });
    }};
}
