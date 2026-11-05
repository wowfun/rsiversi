use crate::{
    Error, Frame, MAXIMUM_PENDING_REQUESTS, Message, Response, Result,
    wire::{Decoder, MAXIMUM_ID},
};
use async_trait::async_trait;
use rsi_process::{MAXIMUM_DUPLEX_CHUNK_BYTES, ManagedDuplexProcess};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Recipient-owned validation and authority for calls arriving from a child.
#[async_trait]
pub trait Handler: fmt::Debug + Send + Sync + 'static {
    /// This callback may await other requests without blocking the byte pumps.
    async fn call(
        &self,
        method: &str,
        payload: Value,
        cancellation: CancellationToken,
    ) -> Result<Value>;
}
/// Default host policy before a family explicitly supplies callback authority.
#[derive(Debug)]
pub struct RejectRequests;
/// Opaque immutable resources needed until native process settlement.
pub trait LaunchResources: fmt::Debug + Send + Sync + 'static {}
impl<T: fmt::Debug + Send + Sync + 'static> LaunchResources for T {}
#[async_trait]
impl Handler for RejectRequests {
    async fn call(&self, _: &str, _: Value, _: CancellationToken) -> Result<Value> {
        Err(Error::Remote("unsupported method".into()))
    }
}
#[derive(Debug, Default)]
struct State {
    next: u64,
    last_incoming: u64,
    pending: BTreeMap<u64, Option<oneshot::Sender<Result<Value>>>>,
    incoming: BTreeMap<u64, CancellationToken>,
    failure: Option<(&'static str, Error)>,
    watchdog: bool,
    handlers: BTreeMap<String, (Arc<dyn Handler>, CancellationToken)>,
}

/// Owns one exact inbound method and its in-flight callback lifetime.
#[derive(Debug)]
pub struct HandlerRegistration {
    shared: Weak<Shared>,
    method: String,
    cancellation: CancellationToken,
}
impl Drop for HandlerRegistration {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(shared) = self.shared.upgrade() {
            shared
                .state
                .lock()
                .expect("extension state")
                .handlers
                .remove(&self.method);
        }
    }
}
#[derive(Debug)]
struct Shared {
    generation: String,
    process: ManagedDuplexProcess,
    state: Mutex<State>,
    writer: mpsc::Sender<Vec<u8>>,
    stop: CancellationToken,
    tasks: TaskTracker,
}
impl Shared {
    fn fail(&self, operation: &'static str, error: Error) {
        let mut state = self.state.lock().expect("extension state");
        let pending = std::mem::take(&mut state.pending);
        for sender in pending.into_values().flatten() {
            let _ = sender.send(Err(error.clone()));
        }
        if state.failure.is_none() {
            state.failure = Some((operation, error));
        }
        for token in state.incoming.values() {
            token.cancel();
        }
        self.stop.cancel();
        self.process.terminate();
    }
    fn send(&self, message: Message) -> Result<()> {
        let bytes = Frame {
            generation: self.generation.clone(),
            message,
        }
        .encode()?;
        self.writer.try_send(bytes).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => Error::Capacity,
            mpsc::error::TrySendError::Closed(_) => Error::Disconnected,
        })
    }
    fn cancel(self: &Arc<Self>, id: u64) {
        {
            let mut state = self.state.lock().expect("extension state");
            let Some(slot) = state.pending.get_mut(&id) else {
                return;
            };
            *slot = None;
        }
        if let Err(error) = self.send(Message::Cancel { id }) {
            self.fail("send cancellation", error);
            return;
        }
        let shared = self.clone();
        self.tasks.spawn(async move {
            tokio::select! {
                () = shared.stop.cancelled() => {},
                () = tokio::time::sleep(Duration::from_secs(5)) => {
                    let unsettled = shared.state.lock().expect("extension state").pending.contains_key(&id);
                    if unsettled { shared.fail("cancellation acknowledgement deadline",Error::Disconnected); }
                }
            }
        });
    }
}
struct RequestGuard {
    shared: Arc<Shared>,
    id: u64,
    done: bool,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        if !self.done {
            self.shared.cancel(self.id);
        }
    }
}
/// Owner of one process connection. Put it in an Arc when sharing within a generation.
#[derive(Debug)]
pub struct Connection {
    shared: Arc<Shared>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.shared.fail("connection dropped", Error::Disconnected);
    }
}
impl Connection {
    /// Observes connection loss without granting authority to stop the process.
    pub fn disconnect_token(&self) -> CancellationToken {
        self.shared.stop.child_token()
    }
    /// Returns the first terminal operation and error observed by this generation.
    pub fn disconnect_cause(&self) -> Option<(&'static str, Error)> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failure
            .clone()
    }
    /// Retains launch files independently of cancellation of the activation waiter.
    pub fn retain_until_settled(&self, resources: Arc<dyn LaunchResources>) {
        let process = self.shared.process.clone();
        self.shared.tasks.spawn(async move {
            let _resources = resources;
            let _ = process.wait_settlement().await;
        });
    }
    /// Registers an exact method without replacing another adapter's authority.
    ///
    /// # Panics
    /// Panics if an internal connection-state lock has been poisoned.
    pub fn register_handler(
        &self,
        method: &str,
        handler: Arc<dyn Handler>,
    ) -> Result<HandlerRegistration> {
        Frame {
            generation: self.shared.generation.clone(),
            message: Message::Request {
                id: 1,
                method: method.into(),
                payload: Value::Null,
            },
        }
        .validate()?;
        let mut state = self.shared.state.lock().expect("extension state");
        if let Some((_, error)) = &state.failure {
            return Err(error.clone());
        }
        if state.handlers.contains_key(method) {
            return Err(Error::Protocol);
        }
        if state.handlers.len() >= MAXIMUM_PENDING_REQUESTS {
            return Err(Error::Capacity);
        }
        let cancellation = self.shared.stop.child_token();
        state
            .handlers
            .insert(method.into(), (handler, cancellation.clone()));
        Ok(HandlerRegistration {
            shared: Arc::downgrade(&self.shared),
            method: method.into(),
            cancellation,
        })
    }
    /// Starts the fixed protocol liveness check once, after a successful handshake.
    ///
    /// # Panics
    /// Panics if an internal connection-state lock has been poisoned.
    pub fn start_watchdog(self: &Arc<Self>) {
        {
            let mut state = self.shared.state.lock().expect("extension state");
            if state.watchdog {
                return;
            }
            state.watchdog = true;
        }
        let weak = Arc::downgrade(self);
        let stop = self.shared.stop.clone();
        self.shared.tasks.spawn(async move {
            loop {
                tokio::select! {
                    () = stop.cancelled() => return,
                    () = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
                let Some(connection) = weak.upgrade() else {
                    return;
                };
                let result = tokio::time::timeout(
                    Duration::from_secs(3),
                    connection.request("extension.ping", Value::Null, stop.child_token()),
                )
                .await;
                if !matches!(result, Ok(Ok(Value::Null))) {
                    connection.shared.fail(
                        if result.is_err() {
                            "heartbeat deadline"
                        } else {
                            "heartbeat reply"
                        },
                        Error::Disconnected,
                    );
                    return;
                }
            }
        });
    }
    /// Takes ownership of an already Sandbox-admitted process and starts its pumps.
    pub fn new(
        process: ManagedDuplexProcess,
        generation: String,
        handler: Arc<dyn Handler>,
    ) -> Result<Self> {
        Frame {
            generation: generation.clone(),
            message: Message::Cancel { id: 1 },
        }
        .validate()?;
        let (writer, receive) = mpsc::channel(2 * MAXIMUM_PENDING_REQUESTS);
        let shared = Arc::new(Shared {
            generation,
            process,
            writer,
            state: Mutex::new(State::default()),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
        });
        shared.tasks.spawn(write_loop(shared.clone(), receive));
        shared.tasks.spawn(read_loop(shared.clone(), handler));
        Ok(Self { shared })
    }
    /// Calls a method once; cancellation never permits automatic replay.
    ///
    /// # Panics
    /// Panics if an internal connection-state lock has been poisoned.
    pub async fn request(
        &self,
        method: &str,
        payload: Value,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let (sender, receiver) = oneshot::channel();
        let id = {
            let mut state = self.shared.state.lock().expect("extension state");
            if let Some((_, error)) = &state.failure {
                return Err(error.clone());
            }
            if state.pending.len() >= MAXIMUM_PENDING_REQUESTS || state.next == MAXIMUM_ID {
                return Err(Error::Capacity);
            }
            let id = state.next + 1;
            self.shared.send(Message::Request {
                id,
                method: method.into(),
                payload,
            })?;
            state.next = id;
            state.pending.insert(id, Some(sender));
            id
        };
        let mut guard = RequestGuard {
            shared: self.shared.clone(),
            id,
            done: false,
        };
        tokio::select! { biased;
            result = receiver => { guard.done = true; result.unwrap_or(Err(Error::Disconnected)) },
            () = cancellation.cancelled() => Err(Error::Cancelled),
        }
    }
    /// Requests graceful close before forced termination and complete Process settlement.
    pub async fn shutdown(&self) -> Result<()> {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            self.request("extension.close", Value::Null, CancellationToken::new()),
        )
        .await;
        self.shared.fail("shutdown", Error::Disconnected);
        self.shared.tasks.close();
        self.shared.tasks.wait().await;
        self.shared
            .process
            .wait_settlement()
            .await
            .map_err(|_| Error::Disconnected)
    }
}
async fn write_loop(shared: Arc<Shared>, mut receive: mpsc::Receiver<Vec<u8>>) {
    let input = shared.process.stdin();
    let pump = async {
        while let Some(bytes) = receive.recv().await {
            for chunk in bytes.chunks(MAXIMUM_DUPLEX_CHUNK_BYTES) {
                let mut offset = 0;
                while offset < chunk.len() {
                    let count = input
                        .write(&chunk[offset..])
                        .await
                        .map_err(|_| Error::Disconnected)?;
                    if count == 0 || count > chunk.len() - offset {
                        return Err(Error::Protocol);
                    }
                    offset += count;
                }
            }
        }
        Err::<(), _>(Error::Disconnected)
    };
    tokio::select! { biased;
        () = shared.stop.cancelled() => {},
        result = pump => shared.fail("write peer input",result.unwrap_err()),
    }
}
async fn read_loop(shared: Arc<Shared>, handler: Arc<dyn Handler>) {
    let output = shared.process.stdout();
    let pump = async {
        let mut decoder = Decoder::new();
        loop {
            let read = output
                .read(MAXIMUM_DUPLEX_CHUNK_BYTES)
                .await
                .map_err(|_| Error::Disconnected)?;
            for frame in decoder.push(&read.bytes)? {
                if frame.generation != shared.generation {
                    return Err(Error::Protocol);
                }
                dispatch(&shared, handler.clone(), frame.message)?;
            }
            if read.eof {
                return Err::<(), _>(Error::Disconnected);
            }
        }
    };
    tokio::select! { biased;
        () = shared.stop.cancelled() => {},
        result = pump => shared.fail("read peer output",result.unwrap_err()),
    }
}
fn dispatch(shared: &Arc<Shared>, handler: Arc<dyn Handler>, message: Message) -> Result<()> {
    let mut state = shared.state.lock().expect("extension state");
    match message {
        Message::Reply { id, result } => {
            let sender = state.pending.remove(&id).ok_or(Error::Protocol)?;
            if let Some(sender) = sender {
                let result = match result {
                    Response::Ok(value) => Ok(value),
                    Response::Error(error) => Err(Error::Remote(error)),
                };
                let _ = sender.send(result);
            }
        }
        Message::Cancel { id } => {
            // A cancellation can cross the already emitted reply.
            if id > state.last_incoming {
                return Err(Error::Protocol);
            }
            if let Some(token) = state.incoming.get(&id) {
                token.cancel();
            }
        }
        Message::Request {
            id,
            method,
            payload,
        } => {
            if id <= state.last_incoming {
                return Err(Error::Protocol);
            }
            if state.incoming.len() >= MAXIMUM_PENDING_REQUESTS {
                return Err(Error::Capacity);
            }
            let cancellation = shared.stop.child_token();
            state.last_incoming = id;
            state.incoming.insert(id, cancellation.clone());
            let active = shared.clone();
            let (handler, registered) = state
                .handlers
                .get(&method)
                .cloned()
                .unwrap_or_else(|| (handler, shared.stop.child_token()));
            shared.tasks.spawn(async move {
                let _cancel = cancellation.clone().drop_guard();
                let result = tokio::select! { biased;
                    () = cancellation.cancelled() => Err(Error::Cancelled),
                    () = registered.cancelled() => Err(Error::Cancelled),
                    result = handler.call(&method, payload, cancellation.clone()) => result,
                };
                let result = match result {
                    Ok(value) => Response::Ok(value),
                    Err(error) => {
                        let mut text = error.to_string();
                        let mut end = text.len().min(4096);
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        text.truncate(end);
                        Response::Error(text)
                    }
                };
                let mut state = active.state.lock().expect("extension state");
                state.incoming.remove(&id);
                let sent = if state.failure.is_none() {
                    active.send(Message::Reply { id, result })
                } else {
                    Ok(())
                };
                drop(state);
                if let Err(error) = sent {
                    active.fail("send callback reply", error);
                }
            });
        }
    }
    Ok(())
}
