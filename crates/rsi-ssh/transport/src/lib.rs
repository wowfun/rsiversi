//! Bounded, epoch-bound SSH helper stdio multiplexing.

mod budget;
mod deadlines;
mod request;
mod state;
mod stream;
mod wire;

pub use budget::Message;
pub use request::{Incoming, IncomingRequest, RequestKind};
pub use rsi_ssh_protocol::control::Control;
pub use stream::{ReceiveStream, SendStream, StreamChunk, StreamId};

use state::State;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Notify, mpsc, oneshot, watch},
};
use tokio_util::sync::CancellationToken;

/// Which side of the single client-to-helper request graph owns this endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Client,
    Helper,
}

/// Transport failures never imply a remote side effect was safely replayable.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    /// No new operation can be admitted on this connection.
    #[error("SSH helper connection is closed")]
    Closed,
    /// A dispatched request lacks its exact complete reply.
    #[error("SSH helper operation outcome is unknown")]
    OutcomeUnknown,
    /// A fixed queue, message, stream or aggregate budget is full.
    #[error("SSH helper transport capacity exceeded")]
    Capacity,
    /// A frame or operation violates the connection contract.
    #[error("invalid SSH helper transport operation")]
    Invalid,
}
/// Transport result.
pub type Result<T> = std::result::Result<T, Error>;
impl From<rsi_ssh_protocol::frame::FrameError> for Error {
    fn from(error: rsi_ssh_protocol::frame::FrameError) -> Self {
        use rsi_ssh_protocol::frame::FrameError;
        match error {
            FrameError::Capacity => Self::Capacity,
            FrameError::Epoch | FrameError::Invalid => Self::Invalid,
        }
    }
}

#[derive(Debug)]
struct Shared {
    epoch: u64,
    role: Role,
    state: Mutex<State>,
    changed: Notify,
    requests_changed: Notify,
    stop: CancellationToken,
    ordinary: mpsc::Sender<IncomingRequest>,
    controls: mpsc::Sender<IncomingRequest>,
    heartbeat: watch::Sender<u64>,
    tasks: AtomicUsize,
    settled: Notify,
}
pub(crate) struct TaskSettlement(Arc<Shared>);
impl TaskSettlement {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self(shared)
    }
}
impl Drop for TaskSettlement {
    fn drop(&mut self) {
        self.0.close();
        self.0.tasks.fetch_sub(1, Ordering::AcqRel);
        self.0.settled.notify_waiters();
    }
}
impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn close(&self) {
        {
            let mut state = self.lock();
            state.close();
        }
        self.stop.cancel();
        self.changed.notify_waiters();
    }
}
#[derive(Debug)]
struct Owner(Arc<Shared>);
impl Drop for Owner {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// A retained connection owner. Dropping its last public handle stops all owned tasks.
#[derive(Clone, Debug)]
pub struct Connection(Arc<Owner>);
impl Connection {
    /// Starts frame tasks over explicitly supplied transport halves.
    ///
    /// # Errors
    /// Rejects a zero connection epoch before spawning any owned tasks.
    pub fn start<R, W>(reader: R, writer: W, role: Role, epoch: u64) -> Result<(Self, Incoming)>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        if epoch == 0 {
            return Err(Error::Invalid);
        }
        let (ordinary, ordinary_rx) = mpsc::channel(state::ORDINARY_CALLS);
        let (controls, controls_rx) = mpsc::channel(state::CONTROL_CALLS);
        let (heartbeat, _) = watch::channel(0);
        let shared = Arc::new(Shared {
            epoch,
            role,
            state: Mutex::new(State::new()),
            changed: Notify::new(),
            requests_changed: Notify::new(),
            stop: CancellationToken::new(),
            ordinary,
            controls,
            heartbeat,
            tasks: AtomicUsize::new(3),
            settled: Notify::new(),
        });
        let connection = Self(Arc::new(Owner(shared.clone())));
        let deadlines_shared = shared.clone();
        tokio::spawn(async move {
            let _settlement = TaskSettlement::new(deadlines_shared.clone());
            deadlines::run(deadlines_shared).await;
        });
        let reader_shared = shared.clone();
        tokio::spawn(async move {
            let _settlement = TaskSettlement::new(reader_shared.clone());
            wire::read(reader_shared, reader).await;
        });
        let writer_shared = shared.clone();
        tokio::spawn(async move {
            let _settlement = TaskSettlement::new(writer_shared.clone());
            wire::write(writer_shared, writer).await;
        });
        Ok((
            connection,
            Incoming {
                shared,
                ordinary: ordinary_rx,
                controls: controls_rx,
            },
        ))
    }
    fn shared(&self) -> &Arc<Shared> {
        &self.0.0
    }
    /// Returns the epoch this connection and all of its stream handles retain.
    pub fn epoch(&self) -> u64 {
        self.shared().epoch
    }
    /// Compares opaque connection ownership, not caller-supplied epoch numbers.
    pub fn same_connection(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    /// Returns the immutable request direction of this connection.
    pub fn role(&self) -> Role {
        self.shared().role
    }
    /// Retires this connection without replaying admitted work.
    pub fn close(&self) {
        self.shared().close();
    }
    /// Checks retirement without waiting or granting permission for a new effect.
    pub fn is_closed(&self) -> bool {
        self.shared().stop.is_cancelled()
    }
    /// Waits until frame tasks have been told to stop, not until SSH is reaped.
    pub async fn closed(&self) {
        self.shared().stop.cancelled().await;
    }
    /// Waits until both frame tasks have released their transport halves. The SSH
    /// process remains the caller's responsibility to terminate and reap.
    pub async fn settled(&self) {
        let shared = self.shared();
        loop {
            let settled = shared.settled.notified();
            tokio::pin!(settled);
            settled.as_mut().enable();
            if shared.tasks.load(Ordering::Acquire) == 0 {
                return;
            }
            settled.await;
        }
    }
    /// Subscribes to fresh client serials accepted by this helper connection.
    pub fn heartbeats(&self) -> watch::Receiver<u64> {
        self.shared().heartbeat.subscribe()
    }
    /// Sends one fresh client heartbeat; abandoned waiters retain its single slot.
    ///
    /// # Errors
    /// Rejects the helper role, a retired connection, an occupied heartbeat slot,
    /// or exhausted serials; connection loss settles the waiter as `Closed`.
    pub async fn heartbeat(&self) -> Result<()> {
        let shared = self.shared();
        if shared.role != Role::Client {
            return Err(Error::Invalid);
        }
        let reply = {
            let mut state = shared.lock();
            state.ensure_open()?;
            if state.heartbeat_waiter.is_some() {
                return Err(Error::Capacity);
            }
            let serial = state.last_heartbeat.checked_add(1).ok_or(Error::Capacity)?;
            let (send, receive) = oneshot::channel();
            state.last_heartbeat = serial;
            state.heartbeat_waiter = Some(send);
            state.heartbeat_deadline =
                Some(tokio::time::Instant::now() + std::time::Duration::from_secs(30));
            state.heartbeat_to_send = Some(serial);
            receive
        };
        shared.changed.notify_waiters();
        shared.requests_changed.notify_waiters();
        reply.await.map_err(|_| Error::Closed)?
    }
}
