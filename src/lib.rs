//! Studio routing: the patchbay server (host audio graph, Core Audio, Dante and Antelope adapters), its wire types, and its UI

#[cfg(feature = "server")]
pub mod antelope;
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
pub mod proto;
#[cfg(feature = "server")]
pub mod report;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "ui")]
pub mod ui;
