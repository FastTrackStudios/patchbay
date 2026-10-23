//! Studio routing: the patchbay server (host audio graph, Core Audio, Dante, Yamaha and Antelope adapters), its wire types, and its UI

#[cfg(feature = "server")]
pub mod antelope;
#[cfg(feature = "server")]
pub mod coreaudio;
#[cfg(feature = "server")]
pub mod dante;
#[cfg(feature = "server")]
pub mod device;
#[cfg(feature = "server")]
pub mod host;
pub mod proto;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "ui")]
pub mod ui;
#[cfg(feature = "server")]
pub mod yamaha;
