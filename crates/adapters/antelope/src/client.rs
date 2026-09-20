//! Async control-port client.
//!
//! One TCP connection, one reader task. Writes are fire-and-forget;
//! reads are matched to `single` replies by `(ext2, ext3)` (header-less
//! `COMMAND_STATUS: FAIL` replies fail the oldest pending request).
//! Cyclic state and notifications fan out on a broadcast channel.
//!
//! TODO(reconnect): a dropped connection is reported
//! ([`ClientEvent::Closed`]) but not re-established; callers reconnect.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

use crate::error::{AntelopeError, Result};
use crate::protocol::INITIALIZE_FORMAT;
use crate::protocol::call::Call;
use crate::protocol::envelope::ServerFrame;
use crate::protocol::framing::{FrameDecoder, encode_frame};

/// Cyclic report class carrying the Galaxy32 master state.
pub const CYCLIC_STATE_CMD: u32 = 115;

/// Default `request` timeout.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(2);

/// What the reader task publishes.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// A `cyclic` or `notification` frame (replies go to their requester).
    Frame(Arc<ServerFrame>),
    /// The connection ended; every pending request failed with
    /// [`AntelopeError::Closed`].
    Closed,
}

struct Pending {
    id: u64,
    key: (u64, u64),
    method: String,
    tx: oneshot::Sender<Result<Value>>,
}

struct Shared {
    pending: Mutex<VecDeque<Pending>>,
    events: broadcast::Sender<ClientEvent>,
    latest_state: Mutex<Option<Arc<Value>>>,
    closed: AtomicBool,
}

impl Shared {
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let drained: Vec<Pending> = self.pending.lock().drain(..).collect();
        for p in drained {
            // The requester may have given up already; nothing to do then.
            let _ = p.tx.send(Err(AntelopeError::Closed));
        }
        let _ = self.events.send(ClientEvent::Closed);
    }

    fn dispatch(&self, frame: ServerFrame) {
        match &frame {
            ServerFrame::Single {
                header, contents, ..
            } => {
                let failed = frame.is_failure();
                let mut pending = self.pending.lock();
                // Header-less replies are failures; they can only be
                // attributed to the oldest outstanding request.
                let idx = header.as_ref().map_or_else(
                    || (!pending.is_empty()).then_some(0),
                    |h| pending.iter().position(|p| p.key == h.key()),
                );
                let Some(p) = idx.and_then(|i| pending.remove(i)) else {
                    tracing::debug!(?header, "antelope: unsolicited single reply");
                    return;
                };
                drop(pending);
                let result = if failed {
                    Err(AntelopeError::CommandFailed { method: p.method })
                } else {
                    Ok(contents.clone())
                };
                let _ = p.tx.send(result);
            }
            ServerFrame::Cyclic {
                header, contents, ..
            } => {
                if header.cmd == CYCLIC_STATE_CMD {
                    *self.latest_state.lock() = Some(Arc::new(contents.clone()));
                }
                let _ = self.events.send(ClientEvent::Frame(Arc::new(frame)));
            }
            ServerFrame::Notification { .. } => {
                let _ = self.events.send(ClientEvent::Frame(Arc::new(frame)));
            }
        }
    }
}

struct Inner {
    addr: SocketAddr,
    writer: tokio::sync::Mutex<OwnedWriteHalf>,
    shared: Arc<Shared>,
    reader: JoinHandle<()>,
    next_id: AtomicU64,
    request_timeout: Duration,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// A Manager Server control-port connection. Cheap to clone; the
/// connection closes when the last clone drops.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("addr", &self.inner.addr)
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Connect, replay the captured `initialize_format` handshake and
    /// start the reader task. Waits (briefly) for the first server frame
    /// so the server has digested the handshake before any request.
    ///
    /// # Errors
    /// Connect/handshake failure or timeout.
    pub async fn connect(addr: SocketAddr) -> Result<Self> {
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| AntelopeError::Timeout {
                method: format!("connect {addr}"),
            })??;
        stream.set_nodelay(true)?;
        let (read, mut write) = stream.into_split();
        write.write_all(&encode_frame(INITIALIZE_FORMAT)?).await?;

        let (events, _) = broadcast::channel(256);
        let shared = Arc::new(Shared {
            pending: Mutex::new(VecDeque::new()),
            events,
            latest_state: Mutex::new(None),
            closed: AtomicBool::new(false),
        });
        let mut first = shared.events.subscribe();
        let reader = tokio::spawn(read_loop(read, Arc::clone(&shared)));
        // A live endpoint always says something back — the server sends
        // cyclic state as soon as the handshake lands. Silence means the
        // Manager Server is still announcing an endpoint whose session
        // has ended: it accepts the connection and never speaks. Treat
        // that as a failure so discovery moves on to the live endpoint
        // instead of holding a dead one and then failing on the first
        // request.
        if tokio::time::timeout(FIRST_FRAME_TIMEOUT, first.recv())
            .await
            .is_err()
        {
            reader.abort();
            return Err(AntelopeError::Timeout {
                method: format!("handshake {addr} (endpoint announced but silent)"),
            });
        }
        Ok(Self {
            inner: Arc::new(Inner {
                addr,
                writer: tokio::sync::Mutex::new(write),
                shared,
                reader,
                next_id: AtomicU64::new(0),
                request_timeout: DEFAULT_REQUEST_TIMEOUT,
            }),
        })
    }

    /// Peer address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.inner.addr
    }

    /// Whether the connection has ended.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.shared.closed.load(Ordering::SeqCst)
    }

    /// Subscribe to cyclic/notification frames and close events.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ClientEvent> {
        self.inner.shared.events.subscribe()
    }

    /// Latest cyclic-115 device state (`None` until the first arrives).
    #[must_use]
    pub fn latest_state(&self) -> Option<Arc<Value>> {
        self.inner.shared.latest_state.lock().clone()
    }

    /// Send a call without waiting for anything (all `set_*` methods).
    ///
    /// # Errors
    /// [`AntelopeError::Closed`] or a socket error.
    pub async fn send(&self, call: &Call) -> Result<()> {
        if self.is_closed() {
            return Err(AntelopeError::Closed);
        }
        let frame = call.to_frame()?;
        tracing::trace!(method = %call.method, "antelope: send");
        let mut w = self.inner.writer.lock().await;
        w.write_all(&frame).await?;
        Ok(())
    }

    /// Fire-and-forget `["method", args, kwargs]`.
    ///
    /// # Errors
    /// See [`Client::send`].
    pub async fn call(
        &self,
        method: &str,
        args: Vec<Value>,
        kwargs: Vec<(String, Value)>,
    ) -> Result<()> {
        self.send(&Call {
            method: method.to_owned(),
            args,
            kwargs,
        })
        .await
    }

    /// Send a read and wait for the `single` reply with header
    /// `(ext2, ext3) == key`. Returns the reply `contents`.
    ///
    /// # Errors
    /// Timeout, `COMMAND_STATUS: FAIL`, closed connection, socket error.
    pub async fn request(
        &self,
        method: &str,
        args: Vec<Value>,
        kwargs: Vec<(String, Value)>,
        key: (u64, u64),
    ) -> Result<Value> {
        let call = Call {
            method: method.to_owned(),
            args,
            kwargs,
        };
        self.request_call(&call, key).await
    }

    /// [`Client::request`] for a prepared [`Call`].
    ///
    /// # Errors
    /// See [`Client::request`].
    pub async fn request_call(&self, call: &Call, key: (u64, u64)) -> Result<Value> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.shared.pending.lock().push_back(Pending {
            id,
            key,
            method: call.method.clone(),
            tx,
        });
        if let Err(e) = self.send(call).await {
            self.forget(id);
            return Err(e);
        }
        match tokio::time::timeout(self.inner.request_timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(AntelopeError::Closed),
            Err(_) => {
                self.forget(id);
                Err(AntelopeError::Timeout {
                    method: call.method.clone(),
                })
            }
        }
    }

    fn forget(&self, id: u64) {
        self.inner.shared.pending.lock().retain(|p| p.id != id);
    }
}

async fn read_loop(mut read: OwnedReadHalf, shared: Arc<Shared>) {
    let mut decoder = FrameDecoder::new();
    let mut buf = vec![0u8; 64 * 1024];
    'conn: loop {
        let n = match read.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "antelope: read failed");
                break;
            }
        };
        decoder.push(buf.get(..n).unwrap_or_default());
        loop {
            match decoder.next_frame() {
                Ok(Some(body)) => match ServerFrame::parse(&body) {
                    Ok(frame) => shared.dispatch(frame),
                    Err(e) => tracing::debug!(error = %e, "antelope: skipping unparseable frame"),
                },
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "antelope: framing lost; closing");
                    break 'conn;
                }
            }
        }
    }
    shared.close();
}
