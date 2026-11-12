//! Starting this machine's engine, or joining the one already running.
//!
//! One engine per machine: whoever starts the engine first (`fts-patchbay`,
//! `patchbay serve`, or any Architect app that links the Patchbay
//! extension) serves it on the listen address, and everyone after that
//! attaches to it as a client. Two engines would mean two `PipeWire`
//! connections editing one graph, or two sets of Core Audio taps rendering
//! one mix.
//!
//! [`Engine::start_or_attach`] asks the address whether a Patchbay engine
//! answers there and attaches if one does; otherwise it binds the port
//! **before** creating the backend, so a loser of the race never opens a
//! second `PipeWire` connection, and the loser retries the join while the
//! winner's backend comes up. An attached engine notices when the engine
//! it reaches goes away ([`Engine::is_lost`]); its holder (`crate::home`)
//! then starts or joins again, so one of the survivors takes the port.
//!
//! Every engine lives on one runtime per process ([`runtime`]), so a
//! caller on any executor can await a start, and a failed start leaves
//! nothing behind.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use architect::host::{EngineHost, WebBundle};

use crate::proto::services::patchbay_service::PatchbayServiceStreamClient;
use crate::proto::{GraphEvent, PatchbayServiceClient};
use crate::server::PatchbayBackend;
use crate::server::permissions::PermissionProvider;

/// The listen address when the engine's config names none. The same for
/// every starter on a platform, so who starts the engine never decides
/// whether it is open to the LAN: loopback on macOS (the RPC is
/// unauthenticated; `patchbay listen lan` opens it), every interface on
/// Linux (the studio machine's phones), as each platform's app always did.
#[cfg(target_os = "linux")]
pub const DEFAULT_ADDR: &str = "0.0.0.0:4046";
/// The listen address when the engine's config names none (see the Linux
/// value).
#[cfg(not(target_os = "linux"))]
pub const DEFAULT_ADDR: &str = "127.0.0.1:4046";

/// How long one join attempt waits for the engine to answer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long to keep trying to join an engine that holds the port but has
/// not answered yet (it bound the port and is still bringing its backend
/// up, or is busy) before giving up, never starting a second one.
const JOIN_DEADLINE: Duration = Duration::from_secs(20);
const JOIN_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Makes the hosting app's [`PermissionProvider`], given the engine's
/// runtime.
pub type MakePermissions =
    Box<dyn FnOnce(tokio::runtime::Handle) -> Arc<dyn PermissionProvider> + Send>;

/// What to do when the port is held by something that answers, and is not
/// a Patchbay engine.
///
/// A holder that does not answer at all may be a Patchbay that is starting
/// or busy: that is never a reason to run a second engine, whatever this
/// says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IfTaken {
    /// Fail with [`EngineError::AddressTaken`].
    #[default]
    Fail,
    /// Run the engine here anyway, reachable only in-process (what
    /// `fts-patchbay` always did with a taken port).
    RunUnserved,
}

/// How to start the engine.
pub struct EngineOptions {
    /// `host:port` to serve `/vox` (and the browser remote) on.
    pub addr: String,
    /// The browser remote to serve, if any.
    pub web: Option<WebBundle>,
    /// The hosting app's permission flow (Patchbay.app on macOS), made
    /// with the engine's runtime, which its prompts run on. Only an
    /// engine that runs here uses it.
    pub permissions: Option<MakePermissions>,
    /// Advertise on, and browse, the LAN for other engines.
    pub discover_peers: bool,
    /// When the port is held by something that is not Patchbay.
    pub if_taken: IfTaken,
    /// How long to keep trying to join a holder that does not answer yet.
    pub join_deadline: Duration,
}

impl EngineOptions {
    /// Serve on `addr`, with no browser remote, no permission provider,
    /// peer discovery on, and failing on a port held by something else.
    #[must_use]
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            web: None,
            permissions: None,
            discover_peers: true,
            if_taken: IfTaken::Fail,
            join_deadline: JOIN_DEADLINE,
        }
    }

    /// [`Self::new`] on this machine's listen address ([`listen_addr`]).
    #[must_use]
    pub fn machine() -> Self {
        Self::new(listen_addr())
    }
}

/// This machine's Patchbay listen address, the same whoever starts the
/// engine.
///
/// An app, `patchbay serve` and a takeover all read: `PATCHBAY_ADDR` (a
/// per-process override for scratch and development engines), else the
/// engine config's `network.bind` (`patchbay listen lan|local`), else
/// [`DEFAULT_ADDR`]. The engine's config decides, not the starter.
#[must_use]
pub fn listen_addr() -> String {
    std::env::var("PATCHBAY_ADDR")
        .ok()
        .or_else(crate::server::configured_bind)
        .unwrap_or_else(|| DEFAULT_ADDR.to_owned())
}

/// Whether this process runs the engine or reaches another one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineMode {
    /// The engine runs here. `serving` is false when the address could
    /// not be bound (the engine still runs, reachable only in-process).
    Owned {
        /// The listen address.
        addr: String,
        /// Whether `/vox` is being served there.
        serving: bool,
    },
    /// Another process's engine, reached at `url`.
    Attached {
        /// Its `ws://…/vox`.
        url: String,
    },
}

/// Why the engine could not start.
#[derive(Debug, Clone, thiserror::Error)]
pub enum EngineError {
    /// The process's engine runtime could not be built.
    #[error("patchbay engine runtime: {0}")]
    Runtime(String),
    /// The address is taken by something that is not a Patchbay engine.
    #[error("{addr} is in use by something that is not a Patchbay engine ({detail})")]
    AddressTaken {
        /// The listen address.
        addr: String,
        /// What the bind and the last join attempt said.
        detail: String,
    },
    /// A Patchbay engine holds the address (it accepts connections) but
    /// did not answer in time. No second engine is started.
    #[error(
        "{addr} is held by an engine that did not answer within the join deadline ({detail}); \
         not starting a second one"
    )]
    NoAnswer {
        /// The listen address.
        addr: String,
        /// What the last join attempt said.
        detail: String,
    },
    /// The in-process clients could not be set up.
    #[error("patchbay engine clients: {0}")]
    Clients(String),
}

/// Why a join failed: what the holder of the port did.
#[derive(Debug)]
enum Join {
    /// Nothing listens there: the port is free to take.
    Refused(String),
    /// Something accepted the connection and did not answer in time: a
    /// Patchbay starting up or busy, perhaps. Keep trying.
    NoAnswer(String),
    /// Something answered, and it is not a Patchbay engine.
    NotPatchbay(String),
}

/// A running engine, here or elsewhere: its clients and where it runs.
pub struct Engine {
    client: PatchbayServiceClient,
    stream: PatchbayServiceStreamClient,
    mode: EngineMode,
    /// Set when an attached engine's event stream ends: the engine it
    /// reached is gone. Never set for an engine that runs here.
    lost: Arc<AtomicBool>,
    /// Keeps an in-process `LocalServer`'s acceptor and lanes alive.
    _scope: Option<Arc<architect::Scope>>,
}

impl Engine {
    /// Attach to the Patchbay engine serving `opts.addr` if there is one;
    /// otherwise take the port and run the engine here. Runs on the
    /// engine runtime, so it can be awaited from any executor.
    ///
    /// # Errors
    /// The port is held by something that is not a Patchbay engine (and
    /// `opts.if_taken` is [`IfTaken::Fail`]), or the runtime or clients
    /// could not be set up.
    pub async fn start_or_attach(opts: EngineOptions) -> Result<Self, EngineError> {
        runtime()?
            .spawn(start_or_attach_here(opts))
            .await
            .map_err(|e| EngineError::Clients(format!("the engine start failed: {e}")))?
    }

    /// [`Self::start_or_attach`] for a caller that is not on a runtime
    /// (an app's `main`).
    ///
    /// # Errors
    /// As [`Self::start_or_attach`].
    ///
    /// # Panics
    /// When called from inside a tokio runtime.
    pub fn start_or_attach_blocking(opts: EngineOptions) -> Result<Self, EngineError> {
        runtime()?.block_on(start_or_attach_here(opts))
    }

    /// The RPC client.
    #[must_use]
    pub const fn client(&self) -> &PatchbayServiceClient {
        &self.client
    }

    /// The streaming client (graph and device events).
    #[must_use]
    pub const fn stream_client(&self) -> &PatchbayServiceStreamClient {
        &self.stream
    }

    /// Where the engine runs.
    #[must_use]
    pub const fn mode(&self) -> &EngineMode {
        &self.mode
    }

    /// Whether the engine this one attached to has gone away.
    #[must_use]
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// Mark a joined engine gone: a client that saw its link end (the
    /// view's event stream) says so, without waiting for the engine's own
    /// watch. An engine that runs here is never lost.
    pub fn mark_lost(&self) {
        if matches!(self.mode, EngineMode::Attached { .. }) {
            self.lost.store(true, Ordering::Release);
        }
    }

    /// Run `call` with the RPC client on the engine runtime, from any
    /// executor.
    ///
    /// # Errors
    /// The runtime is missing, or the task panicked.
    pub async fn run<F, Fut, T>(&self, call: F) -> Result<T, String>
    where
        F: FnOnce(PatchbayServiceClient) -> Fut,
        Fut: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        spawn(call(self.client.clone())).await
    }
}

/// Run `fut` on the engine runtime (it may need tokio's reactor, as
/// dialling does), from any executor.
///
/// # Errors
/// The runtime is missing, or the task panicked.
pub async fn spawn<Fut, T>(fut: Fut) -> Result<T, String>
where
    Fut: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    runtime()
        .map_err(|e| e.to_string())?
        .spawn(fut)
        .await
        .map_err(|e| format!("patchbay engine task: {e}"))
}

/// The process's one engine runtime, made on first use. Every engine,
/// join attempt and call runs on it, so a failed start costs nothing.
///
/// # Errors
/// It could not be built (once; later calls get the same error).
pub fn runtime() -> Result<&'static tokio::runtime::Runtime, EngineError> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .thread_name("patchbay-rt")
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| EngineError::Runtime(e.clone()))
}

async fn start_or_attach_here(opts: EngineOptions) -> Result<Engine, EngineError> {
    // Port 0 is any free port (a phone's engine, `patchbay-ios`): no
    // engine can be waiting there to be joined, so take one at once.
    if crate::server::net::port_of(&opts.addr) == 0 {
        let listener = tokio::net::TcpListener::bind(&opts.addr)
            .await
            .map_err(|e| EngineError::AddressTaken {
                addr: opts.addr.clone(),
                detail: e.to_string(),
            })?;
        return run_here(opts, Some(listener)).await;
    }
    let url = probe_url(&opts.addr);
    let started = tokio::time::Instant::now();
    loop {
        let why = match attach(&url).await {
            Ok(engine) => return Ok(engine),
            // Free: take the port before creating the backend, so a loser
            // of a race never opens a second PipeWire connection.
            Err(Join::Refused(why)) => match tokio::net::TcpListener::bind(&opts.addr).await {
                Ok(listener) => return run_here(opts, Some(listener)).await,
                // Lost the race: the winner is bringing its backend up.
                Err(bind) => format!("{why}; bind: {bind}"),
            },
            Err(Join::NotPatchbay(why)) => {
                return match opts.if_taken {
                    IfTaken::Fail => Err(EngineError::AddressTaken {
                        addr: opts.addr,
                        detail: why,
                    }),
                    IfTaken::RunUnserved => {
                        tracing::warn!(
                            addr = %opts.addr,
                            reason = %why,
                            "patchbay engine address held by something that is not Patchbay: running unserved"
                        );
                        run_here(opts, None).await
                    }
                };
            }
            Err(Join::NoAnswer(why)) => why,
        };
        if started.elapsed() >= opts.join_deadline {
            tracing::warn!(addr = %opts.addr, reason = %why, "patchbay engine: the holder of the port never answered");
            return Err(EngineError::NoAnswer {
                addr: opts.addr,
                detail: why,
            });
        }
        tokio::time::sleep(JOIN_RETRY_DELAY).await;
    }
}

/// Where to look for an engine already serving `addr`: its host, or
/// loopback for a wildcard bind (which loopback reaches too).
fn probe_url(addr: &str) -> String {
    let (host, port) = addr.rsplit_once(':').unwrap_or((addr, ""));
    let host = match host.trim_start_matches('[').trim_end_matches(']') {
        "" | "0.0.0.0" => "127.0.0.1".to_owned(),
        "::" => "[::1]".to_owned(),
        h if h.contains(':') => format!("[{h}]"),
        h => h.to_owned(),
    };
    format!("ws://{host}:{port}/vox")
}

/// Dial the engine at `url` (`ws://host:port/vox`): its RPC and its
/// streaming client, the two links a remote opens.
///
/// # Errors
/// The connection or a handshake failed.
pub async fn dial(
    url: &str,
) -> Result<(PatchbayServiceClient, PatchbayServiceStreamClient), String> {
    let link = vox_websocket::WsLink::connect(url)
        .await
        .map_err(|e| format!("connect {url}: {e}"))?;
    let client: PatchbayServiceClient = vox_core::initiator_on(link)
        .establish()
        .await
        .map_err(|e| format!("handshake with {url}: {e:?}"))?;
    let stream_link = vox_websocket::WsLink::connect(url)
        .await
        .map_err(|e| format!("connect (stream) {url}: {e}"))?;
    let stream: PatchbayServiceStreamClient = vox_core::initiator_on(stream_link)
        .establish()
        .await
        .map_err(|e| format!("handshake (stream) with {url}: {e:?}"))?;
    Ok((client, stream))
}

/// Join the engine at `url`: both links, one call only a Patchbay engine
/// answers, and a watch on its event stream that marks the engine lost
/// when the stream ends. The error says what the holder did ([`Join`]).
async fn attach(url: &str) -> Result<Engine, Join> {
    // Is anything there? Refused means the port is free.
    let host_port = url
        .trim_start_matches("ws://")
        .trim_end_matches("/vox")
        .to_owned();
    match tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(&host_port)).await {
        Err(_) => return Err(Join::NoAnswer(format!("{host_port}: connect timed out"))),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            return Err(Join::Refused(format!("{host_port}: {e}")));
        }
        Ok(Err(e)) => return Err(Join::NoAnswer(format!("{host_port}: {e}"))),
        Ok(Ok(_)) => {}
    }
    let probe = async {
        let (client, stream) = dial(url).await?;
        client
            .listen_address()
            .await
            .map_err(|e| format!("{url} is not a Patchbay engine: {e}"))?;
        Ok::<_, String>((client, stream))
    };
    let (client, stream) = match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
        // Accepted but silent: perhaps a Patchbay between binding its
        // port and serving it.
        Err(_) => return Err(Join::NoAnswer(format!("no answer from {url}"))),
        Ok(Err(why)) => return Err(Join::NotPatchbay(why)),
        Ok(Ok(clients)) => clients,
    };

    let lost = Arc::new(AtomicBool::new(false));
    let (tx, mut rx) = vox::channel::<GraphEvent>();
    let subscription = stream.clone();
    tokio::spawn(async move {
        if let Err(e) = subscription.graph_events(tx).await {
            tracing::debug!("patchbay: the joined engine's event stream ended: {e:?}");
        }
    });
    let flag = Arc::clone(&lost);
    let gone = url.to_owned();
    tokio::spawn(async move {
        while let Ok(Some(_)) = rx.recv().await {}
        flag.store(true, Ordering::Release);
        tracing::warn!(url = %gone, "patchbay: the engine this app joined went away");
    });

    Ok(Engine {
        client,
        stream,
        mode: EngineMode::Attached {
            url: url.to_owned(),
        },
        lost,
        _scope: None,
    })
}

/// Create the backend and its in-process clients, and serve it on
/// `listener` when there is one.
async fn run_here(
    opts: EngineOptions,
    listener: Option<tokio::net::TcpListener>,
) -> Result<Engine, EngineError> {
    let backend = PatchbayBackend::new();
    if let Some(make) = opts.permissions {
        backend.set_permission_provider(make(tokio::runtime::Handle::current()));
    }

    let scope = architect::Scope::new();
    let server = architect::LocalServer::serve(backend.router(), Arc::clone(&scope));
    let caller = server
        .caller()
        .await
        .map_err(|e| EngineError::Clients(format!("caller: {e:?}")))?;
    let client = PatchbayServiceClient::new(caller);
    let stream = server
        .establish::<PatchbayServiceStreamClient>()
        .await
        .map_err(|e| EngineError::Clients(format!("stream client: {e:?}")))?;

    let serving = listener.is_some();
    // What we serve, so `patchbay listen` can report it.
    crate::server::record_bound(
        &opts.addr,
        if opts.web.is_some() {
            ""
        } else {
            "no browser remote in this build (install `dx` and rebuild, \
             or set PATCHBAY_WEB_DIST) — /health and /vox still work"
        },
    );
    // Advertise to (when LAN-reachable) and look for other engines, so a
    // UI attached here can offer to switch to them.
    if opts.discover_peers {
        crate::server::start_peer_discovery(&opts.addr);
    }
    if let Some(listener) = listener {
        // Ungated as before: no PermissionsGate on this engine yet (FastTrackStudios/studio#362).
        let app = EngineHost::ungated(backend.router(), opts.addr.clone())
            .web(opts.web)
            .into_app();
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!(error = %e, "patchbay engine server ended");
            }
        });
    }

    Ok(Engine {
        client,
        stream,
        mode: EngineMode::Owned {
            addr: opts.addr,
            serving,
        },
        lost: Arc::new(AtomicBool::new(false)),
        _scope: Some(scope),
    })
}

#[cfg(test)]
mod tests {
    use super::probe_url;

    #[test]
    fn a_wildcard_bind_is_probed_on_loopback_and_a_host_on_itself() {
        assert_eq!(probe_url("0.0.0.0:4046"), "ws://127.0.0.1:4046/vox");
        assert_eq!(probe_url("127.0.0.1:4046"), "ws://127.0.0.1:4046/vox");
        assert_eq!(probe_url("192.168.1.20:4046"), "ws://192.168.1.20:4046/vox");
        assert_eq!(probe_url("[::]:4046"), "ws://[::1]:4046/vox");
        assert_eq!(probe_url("[fe80::1]:4046"), "ws://[fe80::1]:4046/vox");
        assert_eq!(probe_url("studio.local:4046"), "ws://studio.local:4046/vox");
    }
}
