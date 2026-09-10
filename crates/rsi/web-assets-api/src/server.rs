use crate::{Commit, Observe, Offer, malformed, operations};
use async_trait::async_trait;
use futures_util::Stream;
use rsi_api_protocol::{
    ApiContext, ApiError, ApiHandler, ApiMessage, ApiOutput, ApiRegistrar, ApiRegistration,
    ApiResponseCapacity, CallOrigin, Result, RetainedBytes,
};
use rsi_meta::Execution;
use rsi_web_assets::{BundleLease, WebAssetControl};
use std::{
    collections::BTreeMap,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    task::{Context, Poll},
};
use tokio::sync::{Notify, Semaphore, mpsc};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

type Key = (String, String);
#[derive(Debug, Default)]
struct Leases {
    displayed: Option<BundleLease>,
    pending: Option<BundleLease>,
}
#[derive(Debug)]
struct Observer {
    leases: Mutex<Leases>,
    settled: Notify,
    stop: CancellationToken,
}
#[derive(Debug)]
struct State {
    assets: Arc<WebAssetControl>,
    execution: Execution,
    observers: Mutex<BTreeMap<Key, Weak<Observer>>>,
    slots: Arc<Semaphore>,
    tasks: TaskTracker,
    stop: CancellationToken,
}
/// Registration and producer owner; retirement joins every complete lease holder.
#[derive(Debug)]
pub struct WebAssetsApi {
    registrations: Vec<ApiRegistration>,
    state: Arc<State>,
}
impl WebAssetsApi {
    /// Registers the two exact operations against an explicit asset owner.
    pub fn register(
        registrar: &dyn ApiRegistrar,
        execution: Execution,
        assets: Arc<WebAssetControl>,
    ) -> Result<Self> {
        let state = Arc::new(State {
            assets,
            execution,
            observers: Mutex::default(),
            slots: Arc::new(Semaphore::new(16)),
            tasks: TaskTracker::new(),
            stop: CancellationToken::new(),
        });
        let mut registrations = Vec::new();
        for spec in operations() {
            let commit = spec.id.name() == "commit";
            registrations.push(registrar.register(
                spec,
                Arc::new(Handler {
                    state: state.clone(),
                    commit,
                }),
            )?);
        }
        Ok(Self {
            registrations,
            state,
        })
    }
    /// Fences admission, cancels observations and joins producers before returning.
    pub async fn close(mut self) {
        self.state.stop.cancel();
        for registration in std::mem::take(&mut self.registrations) {
            registration.close().await;
        }
        self.state.tasks.close();
        self.state.tasks.wait().await;
    }
}
impl Drop for WebAssetsApi {
    fn drop(&mut self) {
        self.state.stop.cancel();
    }
}
#[derive(Debug)]
struct Handler {
    state: Arc<State>,
    commit: bool,
}
#[async_trait]
impl ApiHandler for Handler {
    async fn invoke(
        &self,
        context: ApiContext,
        input: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        if self.commit {
            let request: Commit =
                serde_json::from_slice(input.as_bytes()).map_err(|_| malformed())?;
            request.validate()?;
            self.state.commit(&context.origin, &request)?;
            let ApiResponseCapacity::Finite(capacity) = output else {
                return Err(ApiError::Unavailable);
            };
            Ok(ApiOutput::Reply(ApiMessage {
                json: capacity.encode(&true)?,
                binary: None,
            }))
        } else {
            let request: Observe =
                serde_json::from_slice(input.as_bytes()).map_err(|_| malformed())?;
            request.validate()?;
            self.state.observe(context, &request, output)
        }
    }
}
fn key(origin: &CallOrigin, application: &str) -> Key {
    (
        match origin {
            CallOrigin::Local => "local".into(),
            CallOrigin::Device(device) => device.id.as_str().into(),
        },
        application.into(),
    )
}
impl State {
    fn commit(&self, origin: &CallOrigin, request: &Commit) -> Result<()> {
        let observer = self
            .observers
            .lock()
            .expect("Web observers poisoned")
            .get(&key(origin, &request.application))
            .and_then(Weak::upgrade)
            .ok_or(ApiError::Unavailable)?;
        let mut leases = observer.leases.lock().expect("Web leases poisoned");
        if observer.stop.is_cancelled() || self.stop.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        if leases
            .pending
            .as_ref()
            .is_none_or(|lease| lease.revision() != request.revision)
        {
            return Err(ApiError::Unavailable);
        }
        let pending = leases.pending.take();
        if request.accept {
            leases.displayed = pending;
        }
        observer.settled.notify_one();
        Ok(())
    }
    fn observe(
        self: &Arc<Self>,
        context: ApiContext,
        request: &Observe,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        if !matches!(output, ApiResponseCapacity::Subscription { .. }) {
            return Err(ApiError::Unavailable);
        }
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let observer = Arc::new(Observer {
            leases: Mutex::default(),
            settled: Notify::new(),
            stop: self.stop.child_token(),
        });
        let key = key(&context.origin, &request.application);
        {
            let mut observers = self.observers.lock().expect("Web observers poisoned");
            if self.stop.is_cancelled() {
                return Err(ApiError::ShuttingDown);
            }
            if observers.get(&key).and_then(Weak::upgrade).is_some() {
                return Err(ApiError::Capacity);
            }
            observers.insert(key.clone(), Arc::downgrade(&observer));
        }
        let (sender, receiver) = mpsc::channel(1);
        let stream = Forwarded {
            receiver,
            stop: observer.stop.clone(),
        };
        let state = self.clone();
        let token = self.tasks.token();
        self.execution.spawn(async move {
            let (_slot, _token) = (slot, token);
            let revoked = match context.origin {
                CallOrigin::Local => CancellationToken::new(),
                CallOrigin::Device(device) => device.revoked,
            };
            let result = tokio::select! { biased;
                () = observer.stop.cancelled() => Err(ApiError::ShuttingDown),
                () = context.retiring.cancelled() => Err(ApiError::ShuttingDown),
                () = revoked.cancelled() => Err(ApiError::Unauthorized),
                result = state.produce(&observer, &sender, output) => result,
            };
            observer.stop.cancel();
            *observer.leases.lock().expect("Web leases poisoned") = Leases::default();
            state
                .observers
                .lock()
                .expect("Web observers poisoned")
                .remove(&key);
            if let Err(error) = result {
                let _ = sender.try_send(Err(error));
            }
        });
        Ok(ApiOutput::Stream(Box::pin(stream)))
    }
    async fn produce(
        &self,
        observer: &Observer,
        sender: &mpsc::Sender<Result<ApiMessage>>,
        output: ApiResponseCapacity,
    ) -> Result<()> {
        let ApiResponseCapacity::Subscription { budget, maximum } = output else {
            return Err(ApiError::Unavailable);
        };
        let mut changed = self.assets.changes();
        let mut previous = String::new();
        loop {
            if observer
                .leases
                .lock()
                .expect("Web leases poisoned")
                .pending
                .is_some()
            {
                observer.settled.notified().await;
                continue;
            }
            let revision = changed.borrow_and_update().clone();
            if revision == previous {
                changed.changed().await.map_err(|_| ApiError::Unavailable)?;
                continue;
            }
            let permit = sender.reserve().await.map_err(|_| ApiError::Unavailable)?;
            let capacity = budget.reserve(maximum)?;
            let lease = match self.assets.acquire(&revision) {
                Ok(lease) => lease,
                Err(rsi_web_assets::AssetError::Conflict) => continue,
                Err(_) => return Err(ApiError::Unavailable),
            };
            let offer = Offer {
                revision: revision.clone(),
                catalog: lease.catalog().cloned(),
            };
            let json = capacity.encode(&offer)?;
            observer.leases.lock().expect("Web leases poisoned").pending = Some(lease);
            previous = revision;
            permit.send(Ok(ApiMessage { json, binary: None }));
        }
    }
}
struct Forwarded {
    receiver: mpsc::Receiver<Result<ApiMessage>>,
    stop: CancellationToken,
}
impl Stream for Forwarded {
    type Item = Result<ApiMessage>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}
impl Drop for Forwarded {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
