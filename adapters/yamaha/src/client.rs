//! Async RCP client: one TCP connection to `<console>:49280`.
//!
//! - **Reader task** per connection: LF framing ([`LineCodec`]), then
//!   `OK`/`OKm`/`ERROR` lines resolve pending requests and `NOTIFY` lines
//!   fan out on a broadcast channel ([`ClientEvent::Notify`]).
//! - **Correlation.** The console answers in order. `OK` replies echo
//!   the command's leading arguments (`<address> <x> <y>` for get/set),
//!   so they match the oldest pending request with the same verb and
//!   key; `ERROR <verb> <reason>` echoes only the verb and matches the
//!   oldest pending request with that verb (FIFO).
//! - **Flow control.** Writes are serialized; at most
//!   [`ClientOptions::window`] requests are in flight; state-changing
//!   commands (`set`, scene recall…) are spaced by
//!   [`ClientOptions::write_gap`] (Yamaha warns that back-to-back writes
//!   can be dropped; Companion uses 5 ms).
//! - **Keepalive.** Optionally `scpmode keepalive <ms>` per session, and
//!   a `devstatus runmode` ping every [`ClientOptions::ping_interval`]
//!   (also detects half-dead sockets).
//! - **Reconnect** with exponential backoff; [`ClientEvent::Connected`]
//!   `{ reconnect: true }` tells the owner to re-read everything.
//!
//! Written from the protocol notes rather than on top of the `yamaha-rcp`
//! crate, which drops `NOTIFY` lines and corrupts lines split across reads.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Notify, Semaphore, broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::error::{Result, TfError};
use crate::rcp::codec::LineCodec;
use crate::rcp::command::{Command, RcpValue};
use crate::rcp::reply::{Line, ParamReply};
use crate::rcp::token::Token;

/// The RCP TCP port.
pub const RCP_PORT: u16 = 49280;

/// Connection tuning.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// How long a request waits for its reply.
    pub request_timeout: Duration,
    /// TCP connect timeout.
    pub connect_timeout: Duration,
    /// Maximum requests in flight (pipelining bound).
    pub window: usize,
    /// Minimum spacing between state-changing commands.
    pub write_gap: Duration,
    /// Send `scpmode keepalive <ms>` on every (re)connect. `None` leaves
    /// the console's session setting alone.
    pub keepalive: Option<Duration>,
    /// Send a `devstatus runmode` ping this often. `None` disables pings
    /// (then only a socket error detects a dead link).
    pub ping_interval: Option<Duration>,
    /// Reconnect after the connection drops.
    pub reconnect: bool,
    /// First reconnect delay.
    pub backoff_min: Duration,
    /// Reconnect delay cap.
    pub backoff_max: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(2),
            connect_timeout: Duration::from_secs(3),
            window: 32,
            write_gap: Duration::from_millis(5),
            keepalive: Some(Duration::from_secs(10)),
            ping_interval: Some(Duration::from_secs(4)),
            reconnect: true,
            backoff_min: Duration::from_millis(250),
            backoff_max: Duration::from_secs(10),
        }
    }
}

/// What the client publishes.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// A `NOTIFY` line.
    Notify(Arc<Notification>),
    /// A session is up (after `scpmode` setup). `reconnect` is false for
    /// the first session.
    Connected {
        /// Whether this is a re-established connection.
        reconnect: bool,
    },
    /// The connection dropped; pending requests failed with
    /// [`TfError::Closed`].
    Disconnected,
}

/// An unsolicited `NOTIFY <verb> <args…>` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// `set`, `sscurrent_ex`, `ssrecall_ex`, `devstatus`, `mtr`…
    pub verb: String,
    /// Arguments after the verb.
    pub args: Vec<Token>,
    /// The raw line.
    pub line: String,
}

impl Notification {
    /// The parameter change carried by `NOTIFY set …`.
    #[must_use]
    pub fn param(&self) -> Option<ParamReply> {
        (self.verb == "set")
            .then(|| ParamReply::from_args(&self.args).ok())
            .flatten()
    }

    /// Whether this reports a scene change (`sscurrent_ex`, `ssrecall_ex`…).
    #[must_use]
    pub fn is_scene_change(&self) -> bool {
        self.verb.starts_with("sscurrent") || self.verb.starts_with("ssrecall")
    }
}

/// A successful reply (`OK`/`OKm`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// Echoed verb.
    pub verb: String,
    /// Arguments after the verb.
    pub args: Vec<Token>,
    /// `OKm` rather than `OK`.
    pub modified: bool,
}

impl Reply {
    /// Parse as `<address> <x> <y> <value> [display]`.
    ///
    /// # Errors
    /// [`TfError::Protocol`] on a malformed reply.
    pub fn param(&self) -> Result<ParamReply> {
        ParamReply::from_args(&self.args)
    }

    /// Argument `i` as text.
    #[must_use]
    pub fn arg(&self, i: usize) -> Option<&str> {
        self.args.get(i).map(|t| t.text.as_str())
    }
}

struct Pending {
    id: u64,
    verb: String,
    key: Vec<String>,
    line: String,
    tx: oneshot::Sender<Result<Reply>>,
}

#[derive(Default)]
struct WriterSlot {
    half: Option<OwnedWriteHalf>,
    last_write_cmd: Option<Instant>,
}

struct Shared {
    addr: SocketAddr,
    opts: ClientOptions,
    pending: Mutex<VecDeque<Pending>>,
    events: broadcast::Sender<ClientEvent>,
    connected: AtomicBool,
    shutdown: AtomicBool,
    shutdown_notify: Notify,
    writer: tokio::sync::Mutex<WriterSlot>,
    window: Semaphore,
    next_id: AtomicU64,
    reader: Mutex<Option<JoinHandle<()>>>,
}

fn key_matches(key: &[String], args: &[Token]) -> bool {
    args.len() >= key.len() && key.iter().zip(args).all(|(k, a)| *k == a.text)
}

impl Shared {
    fn forget(&self, id: u64) {
        self.pending.lock().retain(|p| p.id != id);
    }

    fn fail_all(&self) {
        let drained: Vec<Pending> = self.pending.lock().drain(..).collect();
        for p in drained {
            // The requester may have given up already.
            let _ = p.tx.send(Err(TfError::Closed));
        }
    }

    fn dispatch(&self, raw: String) {
        match Line::parse(&raw) {
            Line::Ok {
                verb,
                args,
                modified,
            } => {
                let mut pending = self.pending.lock();
                let idx = pending
                    .iter()
                    .position(|p| p.verb == verb && key_matches(&p.key, &args))
                    .or_else(|| pending.iter().position(|p| p.verb == verb));
                let Some(p) = idx.and_then(|i| pending.remove(i)) else {
                    tracing::debug!(line = %raw, "yamaha: unsolicited OK line");
                    return;
                };
                drop(pending);
                let _ = p.tx.send(Ok(Reply {
                    verb,
                    args,
                    modified,
                }));
            }
            Line::Error { verb, reason } => {
                let mut pending = self.pending.lock();
                let idx = pending
                    .iter()
                    .position(|p| p.verb == verb)
                    .or_else(|| (!pending.is_empty()).then_some(0));
                let Some(p) = idx.and_then(|i| pending.remove(i)) else {
                    tracing::debug!(line = %raw, "yamaha: unsolicited ERROR line");
                    return;
                };
                drop(pending);
                let _ = p.tx.send(Err(TfError::Rejected {
                    command: p.line,
                    reason,
                }));
            }
            Line::Notify { verb, args } => {
                let _ = self.events.send(ClientEvent::Notify(Arc::new(Notification {
                    verb,
                    args,
                    line: raw,
                })));
            }
            Line::Empty => {}
            Line::Unknown(line) => tracing::debug!(%line, "yamaha: unrecognized line"),
        }
    }

    async fn request(&self, cmd: &Command) -> Result<Reply> {
        let wire = cmd.encode()?;
        let line = wire.trim_end().to_owned();
        if !self.connected.load(Ordering::SeqCst) {
            return Err(TfError::Closed);
        }
        let timeout = self.opts.request_timeout;
        let _permit = tokio::time::timeout(timeout, self.window.acquire())
            .await
            .map_err(|_| TfError::Timeout {
                command: line.clone(),
            })?
            .map_err(|_| TfError::Closed)?;
        let (tx, rx) = oneshot::channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut w = self.writer.lock().await;
            if cmd.is_write() {
                if let Some(due) = w
                    .last_write_cmd
                    .and_then(|t| t.checked_add(self.opts.write_gap))
                {
                    tokio::time::sleep_until(due).await;
                }
            }
            let Some(half) = w.half.as_mut() else {
                return Err(TfError::Closed);
            };
            // Registered under the writer lock so pending order == wire order.
            self.pending.lock().push_back(Pending {
                id,
                verb: cmd.verb().to_owned(),
                key: cmd.match_key(),
                line: line.clone(),
                tx,
            });
            // The reader may have died since the check above; it fails
            // pending requests only after clearing `connected`.
            if !self.connected.load(Ordering::SeqCst) {
                self.forget(id);
                return Err(TfError::Closed);
            }
            tracing::trace!(%line, "yamaha: send");
            if let Err(e) = half.write_all(wire.as_bytes()).await {
                self.forget(id);
                return Err(e.into());
            }
            if cmd.is_write() {
                w.last_write_cmd = Some(Instant::now());
            }
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(TfError::Closed),
            Err(_) => {
                self.forget(id);
                Err(TfError::Timeout { command: line })
            }
        }
    }

    /// Tear the session down (idempotent: the first caller takes the
    /// write half).
    async fn teardown(&self) {
        self.connected.store(false, Ordering::SeqCst);
        let half = self.writer.lock().await.half.take();
        let Some(mut half) = half else {
            return;
        };
        let reader = self.reader.lock().take();
        if let Some(h) = reader {
            h.abort();
        }
        let _ = half.shutdown().await;
        self.fail_all();
        let _ = self.events.send(ClientEvent::Disconnected);
    }

    fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
}

async fn connect_once(addr: SocketAddr, timeout: Duration) -> Result<TcpStream> {
    let stream = tokio::time::timeout(timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| TfError::Timeout {
            command: format!("connect {addr}"),
        })??;
    stream.set_nodelay(true)?;
    Ok(stream)
}

async fn read_loop(mut read: OwnedReadHalf, shared: Arc<Shared>, done: oneshot::Sender<()>) {
    let mut codec = LineCodec::new();
    let mut buf = vec![0u8; 16 * 1024];
    'conn: loop {
        let n = match read.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "yamaha: read failed");
                break;
            }
        };
        codec.push(buf.get(..n).unwrap_or_default());
        loop {
            match codec.next_line() {
                Ok(Some(line)) => shared.dispatch(line),
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "yamaha: framing lost; closing");
                    break 'conn;
                }
            }
        }
    }
    // Fail everything in flight now rather than letting each request run
    // into its timeout (a ping may be blocking the session loop).
    shared.connected.store(false, Ordering::SeqCst);
    shared.fail_all();
    let _ = done.send(());
}

/// Run one established session until it ends. Returns `true` on shutdown.
async fn run_session(shared: &Arc<Shared>, stream: TcpStream, reconnect: bool) -> bool {
    let (read, write) = stream.into_split();
    shared.writer.lock().await.half = Some(write);
    let (done_tx, mut done_rx) = oneshot::channel();
    *shared.reader.lock() = Some(tokio::spawn(read_loop(read, Arc::clone(shared), done_tx)));
    shared.connected.store(true, Ordering::SeqCst);

    if let Some(ka) = shared.opts.keepalive {
        let ms = u32::try_from(ka.as_millis()).unwrap_or(u32::MAX);
        if let Err(e) = shared.request(&Command::scpmode_keepalive(ms)).await {
            tracing::warn!(error = %e, "yamaha: scpmode keepalive not accepted");
        }
    }
    let _ = shared.events.send(ClientEvent::Connected { reconnect });

    let mut ping = shared.opts.ping_interval.map(|p| {
        let mut i = tokio::time::interval(p);
        i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        i.reset();
        i
    });
    let shutdown = loop {
        tokio::select! {
            _ = &mut done_rx => break false,
            () = shared.shutdown_notify.notified() => break true,
            () = async {
                match ping.as_mut() {
                    Some(i) => { i.tick().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {
                match shared.request(&Command::devstatus("runmode")).await {
                    Ok(_) | Err(TfError::Rejected { .. }) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "yamaha: keepalive ping failed; dropping connection");
                        break false;
                    }
                }
            }
        }
    };
    shared.teardown().await;
    shutdown || shared.is_shutdown()
}

async fn supervise(shared: Arc<Shared>, first: TcpStream) {
    let mut stream = Some(first);
    let mut reconnect = false;
    let mut backoff = shared.opts.backoff_min;
    loop {
        let s = match stream.take() {
            Some(s) => s,
            None => match connect_once(shared.addr, shared.opts.connect_timeout).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::info!(addr = %shared.addr, error = %e, ?backoff, "yamaha: reconnect failed");
                    if wait_or_shutdown(&shared, backoff).await {
                        return;
                    }
                    backoff = backoff.saturating_mul(2).min(shared.opts.backoff_max);
                    continue;
                }
            },
        };
        backoff = shared.opts.backoff_min;
        if run_session(&shared, s, reconnect).await || !shared.opts.reconnect {
            return;
        }
        reconnect = true;
        if wait_or_shutdown(&shared, backoff).await {
            return;
        }
    }
}

/// Sleep `d`; `true` if shutdown was requested meanwhile.
async fn wait_or_shutdown(shared: &Shared, d: Duration) -> bool {
    if shared.is_shutdown() {
        return true;
    }
    tokio::select! {
        () = tokio::time::sleep(d) => shared.is_shutdown(),
        () = shared.shutdown_notify.notified() => true,
    }
}

struct Inner {
    shared: Arc<Shared>,
    supervisor: JoinHandle<()>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.shutdown_notify.notify_one();
        self.supervisor.abort();
        let reader = self.shared.reader.lock().take();
        if let Some(h) = reader {
            h.abort();
        }
    }
}

/// An RCP connection. Cheap to clone; closes when the last clone drops.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("addr", &self.inner.shared.addr)
            .field("connected", &self.is_connected())
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Connect with default options.
    ///
    /// # Errors
    /// Connect failure or timeout.
    pub async fn connect(addr: SocketAddr) -> Result<Self> {
        Self::connect_with(addr, ClientOptions::default()).await
    }

    /// Connect, start the reader/supervisor and wait for the session to
    /// be set up (`scpmode keepalive` if configured).
    ///
    /// # Errors
    /// Connect failure or timeout.
    pub async fn connect_with(addr: SocketAddr, opts: ClientOptions) -> Result<Self> {
        let stream = connect_once(addr, opts.connect_timeout).await?;
        let (events, _) = broadcast::channel(4096);
        let setup_timeout = opts.request_timeout.saturating_add(opts.connect_timeout);
        let shared = Arc::new(Shared {
            addr,
            window: Semaphore::new(opts.window.max(1)),
            opts,
            pending: Mutex::new(VecDeque::new()),
            events,
            connected: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            shutdown_notify: Notify::new(),
            writer: tokio::sync::Mutex::new(WriterSlot::default()),
            next_id: AtomicU64::new(0),
            reader: Mutex::new(None),
        });
        let mut rx = shared.events.subscribe();
        let supervisor = tokio::spawn(supervise(Arc::clone(&shared), stream));
        let client = Self {
            inner: Arc::new(Inner { shared, supervisor }),
        };
        let wait = async {
            loop {
                match rx.recv().await {
                    Ok(ClientEvent::Connected { .. }) => return Ok(()),
                    Ok(ClientEvent::Disconnected) | Err(broadcast::error::RecvError::Closed) => {
                        return Err(TfError::Closed);
                    }
                    Ok(ClientEvent::Notify(_)) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
        };
        tokio::time::timeout(setup_timeout, wait)
            .await
            .map_err(|_| TfError::Timeout {
                command: format!("session setup with {addr}"),
            })??;
        Ok(client)
    }

    /// Peer address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.inner.shared.addr
    }

    /// Whether a session is currently up.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.inner.shared.connected.load(Ordering::SeqCst)
    }

    /// Subscribe to NOTIFY lines and connection events.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ClientEvent> {
        self.inner.shared.events.subscribe()
    }

    /// Stop reconnecting and close the connection.
    pub async fn close(&self) {
        let shared = &self.inner.shared;
        shared.shutdown.store(true, Ordering::SeqCst);
        shared.shutdown_notify.notify_one();
        shared.teardown().await;
    }

    /// Send any command and wait for its `OK`.
    ///
    /// # Errors
    /// [`TfError::Rejected`] for an `ERROR` reply, [`TfError::Timeout`],
    /// [`TfError::Closed`], socket errors.
    pub async fn request(&self, cmd: &Command) -> Result<Reply> {
        self.inner.shared.request(cmd).await
    }

    /// `get <address> <x> <y>`.
    ///
    /// # Errors
    /// See [`Client::request`]; malformed replies.
    pub async fn get(&self, address: &str, x: u16, y: u16) -> Result<ParamReply> {
        self.request(&Command::get(address, x, y)).await?.param()
    }

    /// `set <address> <x> <y> <value>`; returns the applied value the
    /// console echoes (it may clamp). **Changes console state.**
    ///
    /// # Errors
    /// See [`Client::request`]; malformed replies.
    pub async fn set(&self, address: &str, x: u16, y: u16, value: RcpValue) -> Result<ParamReply> {
        self.request(&Command::set(address, x, y, value))
            .await?
            .param()
    }

    /// `devinfo <what>` → the quoted value.
    ///
    /// # Errors
    /// See [`Client::request`].
    pub async fn devinfo(&self, what: &str) -> Result<String> {
        let r = self.request(&Command::devinfo(what)).await?;
        Ok(r.arg(1).unwrap_or_default().to_owned())
    }
}
