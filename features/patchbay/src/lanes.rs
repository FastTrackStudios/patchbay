//! One connection to an engine, and a lane on it per service: the core's
//! `PatchbayService`, its event stream, and each device family's own
//! section (`DeviceFamily::routes`, studio#420).
//!
//! A client (the window, the browser remote, a phone, the CLI) holds one
//! [`Lanes`] per engine and opens a typed client for whichever section it
//! needs: `lanes.open::<DanteServiceClient>()`. Over a websocket that is
//! a vox lane on the one connection (`ConnectionHandle::open_lane`); for
//! an engine in this process it is a lane on its `LocalServer`. A section
//! the engine doesn't serve (its family isn't installed there) fails to
//! open, with the reason. A remote client makes the connection with
//! `vox_core::initiator_on(link).establish_connection()` and wraps the
//! handle: `Lanes::Remote(handle)`.
//!
//! **Who may open which lane.** Today every lane is as open as the
//! core's own service: the engine's websocket has no gate yet
//! (studio#362). When it does, a family's section is authorised like the
//! core's, per method, not per lane: opening a lane is free, and each
//! call is checked against the caller. A family declares which of its
//! methods only *observe* (read state, subscribe) and which *change*
//! something (routing, levels, a service unit); a remote peer (another
//! machine of the account's Architect peers, #466) gets observe by being
//! a peer, and change only with the grant the owner gave it. A family's
//! own actions already carry that split (`effect = "read_only"` in its
//! manifest); its RPC methods will carry the same.

#[cfg(feature = "server")]
use std::sync::Arc;

/// A typed client a lane can be opened for: any `#[architect::rpc]`
/// service's client. (`Send` where lanes are driven on the engine's
/// runtime; a browser's are not.)
#[cfg(feature = "server")]
pub trait LaneClient: vox_core::FromVoxLane + Clone + Send + 'static {}
#[cfg(feature = "server")]
impl<T: vox_core::FromVoxLane + Clone + Send + 'static> LaneClient for T {}

/// A typed client a lane can be opened for: any `#[architect::rpc]`
/// service's client.
#[cfg(not(feature = "server"))]
pub trait LaneClient: vox_core::FromVoxLane + Clone + 'static {}
#[cfg(not(feature = "server"))]
impl<T: vox_core::FromVoxLane + Clone + 'static> LaneClient for T {}

/// The lanes to one engine. Cheap to clone; every clone opens lanes on
/// the same connection.
#[derive(Clone)]
pub enum Lanes {
    /// An engine over a websocket (`ws://host:port/vox`).
    Remote(vox_core::ConnectionHandle),
    /// The engine in this process.
    #[cfg(feature = "server")]
    Local(Arc<architect::LocalServer>),
}

impl Lanes {
    /// A typed client for service `C` on this engine. From any thread: a
    /// lane's driver needs the engine runtime on native, so a caller that
    /// isn't on one (the window's thread) is moved onto it.
    ///
    /// # Errors
    /// The engine serves no such section, or the connection is gone.
    #[cfg(feature = "server")]
    pub async fn open<C: LaneClient>(&self) -> Result<C, String> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return self.open_here::<C>().await;
        }
        let lanes = self.clone();
        crate::server::spawn(async move { lanes.open_here::<C>().await }).await?
    }

    /// A typed client for service `C` on this engine (a browser: lanes
    /// are driven on the page's own executor).
    ///
    /// # Errors
    /// The engine serves no such section, or the connection is gone.
    #[cfg(not(feature = "server"))]
    pub async fn open<C: LaneClient>(&self) -> Result<C, String> {
        self.open_here::<C>().await
    }

    async fn open_here<C: vox_core::FromVoxLane>(&self) -> Result<C, String> {
        match self {
            Self::Remote(connection) => connection
                .open_lane::<C>()
                .await
                .map_err(|e| format!("open the {} lane: {e:?}", C::SERVICE_NAME)),
            #[cfg(feature = "server")]
            Self::Local(server) => server
                .establish::<C>()
                .await
                .map_err(|e| format!("open the {} lane: {e}", C::SERVICE_NAME)),
        }
    }
}
