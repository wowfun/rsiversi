//! Bounded ACP correlation and drain over an explicitly supplied transport.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

pub mod server;
mod transport;
use rsi_acp_protocol::{
    FrameDecoder, MAX_DIRECTION_BYTES, MAX_FRAME_BYTES, MAX_PENDING, Message, RequestId,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI64, Ordering},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
pub use transport::{ProcessTransport, StreamTransport, Transport};

/// Categorical transport failures; no wire payload appears in diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    /// Retention or pending-request admission was exhausted.
    #[error("ACP peer capacity exceeded")]
    Capacity,
    /// Invalid wire input or unexpected response correlation.
    #[error("ACP peer protocol failed")]
    Protocol,
    /// Peer terminated, or cancellation left request delivery unknown.
    #[error("ACP peer closed; request delivery may be unknown")]
    Closed,
}

/// Admitted incoming message; its byte lease lasts through consumer handling.
pub struct Incoming {
    /// Exact connection-local message ordinal, including requests and notifications.
    pub ordinal: u64,
    /// Validated request or notification.
    pub message: Message,
    _bytes: OwnedSemaphorePermit,
}
impl std::fmt::Debug for Incoming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Incoming").finish_non_exhaustive()
    }
}

/// Admitted response; retaining this value retains its connection byte budget.
pub struct Response {
    preceding_messages: u64,
    result: Result<Value, Value>,
    _bytes: OwnedSemaphorePermit,
}
impl Response {
    /// Last incoming ordinal observed before this response on the byte stream.
    pub const fn preceding_messages(&self) -> u64 {
        self.preceding_messages
    }
    /// Borrows the exact result or remote error without copying retained payload.
    pub fn result(&self) -> Result<&Value, &Value> {
        self.result.as_ref()
    }
}
impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response").finish_non_exhaustive()
    }
}

struct PendingResponse {
    sender: oneshot::Sender<Response>,
    discard_late_reply: bool,
}
type Pending = BTreeMap<RequestId, PendingResponse>;
enum Write {
    Frame {
        bytes: Vec<u8>,
        _permit: OwnedSemaphorePermit,
        written: oneshot::Sender<()>,
    },
    Drain(oneshot::Sender<()>),
}

#[derive(Clone)]
/// Cloneable request port. Only the separate Peer owner controls task lifetime.
pub struct PeerHandle {
    stop: CancellationToken,
    failure: Arc<Mutex<Option<Error>>>,
    incoming_budget: Arc<Semaphore>,
    outgoing_budget: Arc<Semaphore>,
    pending: Arc<Mutex<Pending>>,
    incoming_requests: Arc<Mutex<BTreeSet<RequestId>>>,
    sequence: Arc<AtomicI64>,
    writes: mpsc::Sender<Write>,
}
impl std::fmt::Debug for PeerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerHandle").finish_non_exhaustive()
    }
}

impl PeerHandle {
    /// Whether this exact peer has been retired.
    pub fn is_closed(&self) -> bool {
        self.stop.is_cancelled()
    }

    /// First categorical retirement failure; clean EOF or explicit close has none.
    pub fn failure(&self) -> Option<Error> {
        *self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Sends a request and correlates its exact reply. Dropping this future retires the peer.
    pub async fn request(
        &self,
        method: &str,
        params: &Value,
        cancellation: CancellationToken,
    ) -> Result<Response, Error> {
        self.request_inner(method, params, cancellation, false)
            .await
    }

    /// Requests permission without retiring a flushed request when its waiter ends.
    /// A late reply consumes its retained pending slot and grants no local authority.
    pub async fn request_permission(&self, params: &Value) -> Result<Response, Error> {
        rsi_acp_protocol::validate_permission(params).map_err(|_| Error::Protocol)?;
        self.request_inner(
            "session/request_permission",
            params,
            CancellationToken::new(),
            true,
        )
        .await
    }

    async fn request_inner(
        &self,
        method: &str,
        params: &Value,
        cancellation: CancellationToken,
        discard_late_reply: bool,
    ) -> Result<Response, Error> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        let number = self
            .sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| Error::Capacity)?;
        let id = RequestId::Integer(number);
        let (send, receive) = oneshot::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if pending.len() == MAX_PENDING {
                return Err(Error::Capacity);
            }
            pending.insert(
                id.clone(),
                PendingResponse {
                    sender: send,
                    discard_late_reply,
                },
            );
        }
        let mut guard = RequestGuard {
            id,
            peer: self,
            complete: false,
            retain: false,
        };
        let flushed = match self
            .queue(&json!({"jsonrpc":"2.0", "id":number, "method":method, "params":params}))
        {
            Ok(flushed) => flushed,
            Err(error) => {
                guard.complete = true;
                return Err(error);
            }
        };
        let exchange = async {
            self.flushed(flushed).await?;
            guard.retain = discard_late_reply;
            receive.await.map_err(|_| Error::Closed)
        };
        let result = tokio::select! { biased;
            response = exchange => response,
            () = self.stop.cancelled() => Err(Error::Closed),
            () = cancellation.cancelled() => Err(Error::Closed),
        };
        guard.complete = result.is_ok();
        result
    }

    /// Sends a notification and waits for its serialized flush.
    pub async fn notify(&self, method: &str, params: &Value) -> Result<(), Error> {
        self.send(&json!({"jsonrpc":"2.0", "method":method, "params":params}))
            .await
    }

    /// Responds to an exact peer request and waits for the flush boundary.
    pub async fn respond(
        &self,
        id: &RequestId,
        result: Result<&Value, &Value>,
    ) -> Result<(), Error> {
        let receive = {
            let mut incoming = self
                .incoming_requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !incoming.contains(id) {
                return Err(self.retire(Error::Protocol));
            }
            let receive = match result {
                Ok(value) => self.queue(&json!({"jsonrpc":"2.0","id":id,"result":value})),
                Err(error) => self.queue(&json!({"jsonrpc":"2.0","id":id,"error":error})),
            }?;
            incoming.remove(id);
            receive
        };
        self.flushed(receive).await
    }

    /// Waits until every earlier admitted write has crossed the transport flush.
    pub async fn drain(&self) -> Result<(), Error> {
        let (send, receive) = oneshot::channel();
        tokio::select! { biased;
            () = self.stop.cancelled() => return Err(Error::Closed),
            result = self.writes.send(Write::Drain(send)) => result.map_err(|_| Error::Closed)?,
        }
        tokio::select! { biased; () = self.stop.cancelled() => Err(Error::Closed), result = receive => result.map_err(|_| Error::Closed) }
    }

    async fn send(&self, value: &impl Serialize) -> Result<(), Error> {
        self.flushed(self.queue(value)?).await
    }

    fn queue(&self, value: &impl Serialize) -> Result<oneshot::Receiver<()>, Error> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        let mut frame = BoundedFrame(Vec::new());
        serde_json::to_writer(&mut frame, value).map_err(|_| Error::Capacity)?;
        let permit = self
            .outgoing_budget
            .clone()
            .try_acquire_many_owned(u32::try_from(frame.0.len()).map_err(|_| Error::Capacity)?)
            .map_err(|_| Error::Capacity)?;
        frame.0.push(b'\n');
        let (send, receive) = oneshot::channel();
        self.writes
            .try_send(Write::Frame {
                bytes: frame.0,
                _permit: permit,
                written: send,
            })
            .map_err(|_| Error::Capacity)?;
        Ok(receive)
    }

    async fn flushed(&self, receive: oneshot::Receiver<()>) -> Result<(), Error> {
        tokio::select! { biased; result = receive => result.map_err(|_| self.retire(Error::Closed)), () = self.stop.cancelled() => Err(Error::Closed) }
    }

    fn retire(&self, error: Error) -> Error {
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert(error);
        self.stop.cancel();
        error
    }
}

struct RequestGuard<'a> {
    id: RequestId,
    peer: &'a PeerHandle,
    complete: bool,
    retain: bool,
}
impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        if self.retain && !self.complete {
            return;
        }
        self.peer
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
        if !self.complete {
            self.peer.retire(Error::Closed);
        }
    }
}

/// Reader/writer task owner, separate from detachable application controllers.
pub struct Peer {
    handle: PeerHandle,
    incoming: mpsc::Receiver<Incoming>,
    transport: Arc<dyn Transport>,
    tasks: Vec<JoinHandle<()>>,
}
impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer").finish_non_exhaustive()
    }
}
impl Peer {
    /// Starts one isolated connection over the caller-supplied transport.
    pub fn start(transport: Arc<dyn Transport>) -> Self {
        let (writes, outbound) = mpsc::channel(MAX_PENDING);
        let (events, incoming) = mpsc::channel(8_192);
        let handle = PeerHandle {
            stop: CancellationToken::new(),
            failure: Arc::new(Mutex::new(None)),
            incoming_budget: Arc::new(Semaphore::new(MAX_DIRECTION_BYTES)),
            outgoing_budget: Arc::new(Semaphore::new(MAX_DIRECTION_BYTES)),
            pending: Arc::new(Mutex::new(BTreeMap::new())),
            incoming_requests: Arc::new(Mutex::new(BTreeSet::new())),
            sequence: Arc::new(AtomicI64::new(1)),
            writes,
        };
        let tasks = vec![
            tokio::spawn(read_loop(transport.clone(), handle.clone(), events)),
            tokio::spawn(write_loop(transport.clone(), handle.clone(), outbound)),
        ];
        Self {
            handle,
            incoming,
            transport,
            tasks,
        }
    }
    /// Obtains an I/O port without transferring process lifetime.
    pub fn handle(&self) -> PeerHandle {
        self.handle.clone()
    }
    /// Reads the next bounded request or notification. EOF means this peer retired.
    pub async fn next(&mut self) -> Option<Incoming> {
        self.incoming.recv().await
    }
    /// Takes an already queued message without waiting; its byte lease remains owned.
    pub fn try_next(&mut self) -> Option<Incoming> {
        self.incoming.try_recv().ok()
    }

    /// Cancels and joins transport work, then closes/reaps its owned transport.
    pub async fn close(mut self) -> Result<(), Error> {
        self.handle.stop.cancel();
        for task in self.tasks.drain(..) {
            let _ignored = task.await;
        }
        self.transport.close().await
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.handle.stop.cancel();
    }
}

async fn read_loop(
    transport: Arc<dyn Transport>,
    peer: PeerHandle,
    events: mpsc::Sender<Incoming>,
) {
    let mut decoder = FrameDecoder::default();
    let mut ordinal = 0_u64;
    let reading = async {
        loop {
            let chunk = transport.read().await?;
            if chunk.is_empty() {
                decoder.finish().map_err(|_| Error::Protocol)?;
                return Ok::<(), Error>(());
            }
            for frame in decoder.push(&chunk).map_err(|_| Error::Protocol)? {
                let permit = peer
                    .incoming_budget
                    .clone()
                    .try_acquire_many_owned(
                        u32::try_from(frame.len()).map_err(|_| Error::Capacity)?,
                    )
                    .map_err(|_| Error::Capacity)?;
                let message = rsi_acp_protocol::decode(&frame).map_err(|_| Error::Protocol)?;
                if let Message::Response { id, result } = message {
                    let pending = peer
                        .pending
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&id)
                        .ok_or(Error::Protocol)?;
                    if pending
                        .sender
                        .send(Response {
                            preceding_messages: ordinal,
                            result,
                            _bytes: permit,
                        })
                        .is_err()
                        && !pending.discard_late_reply
                    {
                        return Err(Error::Closed);
                    }
                } else {
                    ordinal = ordinal.checked_add(1).ok_or(Error::Capacity)?;
                    if let Message::Request { id, .. } = &message {
                        let mut requests = peer
                            .incoming_requests
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if requests.len() == MAX_PENDING || !requests.insert(id.clone()) {
                            return Err(Error::Protocol);
                        }
                    }
                    events
                        .try_send(Incoming {
                            ordinal,
                            message,
                            _bytes: permit,
                        })
                        .map_err(|_| Error::Capacity)?;
                }
            }
        }
    };
    tokio::select! { biased; () = peer.stop.cancelled() => {}, result = reading => {
        if let Err(error) = result { peer.retire(error); }
    } }
    peer.stop.cancel();
    peer.pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    if let Err(error) = transport.close().await {
        peer.retire(error);
    }
}

async fn write_loop(
    transport: Arc<dyn Transport>,
    peer: PeerHandle,
    mut writes: mpsc::Receiver<Write>,
) {
    let writing = async {
        while let Some(write) = writes.recv().await {
            match write {
                Write::Frame {
                    bytes,
                    _permit,
                    written,
                } => {
                    transport.write(&bytes).await?;
                    transport.flush().await?;
                    let _ignored = written.send(());
                }
                Write::Drain(send) => {
                    transport.flush().await?;
                    let _ignored = send.send(());
                }
            }
        }
        Ok::<(), Error>(())
    };
    tokio::select! { biased; () = peer.stop.cancelled() => {}, result = writing => {
        if let Err(error) = result { peer.retire(error); }
    } }
    peer.stop.cancel();
}

struct BoundedFrame(Vec<u8>);
impl std::io::Write for BoundedFrame {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_FRAME_BYTES {
            return Err(std::io::Error::other("ACP frame bound"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod drain_tests {
    use super::*;
    use std::future::Future as _;
    #[derive(Debug, Default)]
    struct HeldWrites {
        release: CancellationToken,
        entered: tokio::sync::Notify,
    }
    #[async_trait::async_trait]
    impl Transport for HeldWrites {
        async fn read(&self) -> Result<Vec<u8>, Error> {
            std::future::pending().await
        }
        async fn write(&self, _: &[u8]) -> Result<(), Error> {
            self.entered.notify_one();
            self.release.cancelled().await;
            Ok(())
        }
        async fn flush(&self) -> Result<(), Error> {
            Ok(())
        }
        async fn close(&self) -> Result<(), Error> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn drain_waits_for_full_queue_without_retiring_or_overtaking_writes() {
        let transport = Arc::new(HeldWrites::default());
        let peer = Peer::start(transport.clone());
        let port = peer.handle();
        let mut sends = tokio::task::JoinSet::new();
        sends.spawn({
            let port = port.clone();
            async move { port.notify("test", &json!({})).await }
        });
        transport.entered.notified().await;
        for _ in 0..MAX_PENDING {
            sends.spawn({
                let port = port.clone();
                async move { port.notify("test", &json!({})).await }
            });
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while port.writes.capacity() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut drain = Box::pin(port.drain());
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(drain.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        assert!(!port.is_closed());
        transport.release.cancel();
        drain.await.unwrap();
        while let Some(result) = sends.join_next().await {
            result.unwrap().unwrap();
        }
        assert!(!port.is_closed());
        peer.close().await.unwrap();
    }
}
