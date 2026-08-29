use async_trait::async_trait;
use futures_util::future::BoxFuture;
use futures_util::stream::{self, StreamExt as _};
use std::any::Any;
use std::fmt;
use std::sync::Arc;

use crate::EventListenerId;
use crate::runtime::{Runtime, drop_catching_unwind};

/// Maximum [`Parallel`] callback futures polled at once by one dispatch.
pub const MAXIMUM_PARALLEL_EVENT_CALLBACKS: usize = 64;

/// Nominal marker for one process-local typed event.
///
/// The associated mode makes scheduling and value flow a property of the
/// event contract. Callers cannot select a different mode at dispatch time.
pub trait LocalEvent: 'static + Sized {
    /// Stable Host/Profile catalog name used only for configuration and diagnostics.
    const KEY: &'static str;

    /// Direct safe-Rust value carried by this event.
    type Value: Clone + Send + Sync + 'static;

    /// Direct safe-Rust handler failure.
    type Error: Send + 'static;

    /// Compile-time dispatch and value-flow policy.
    type Mode: LocalEventMode<Self>;
}

/// Listener insertion and one-shot policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LocalEventOptions {
    /// Inserts this listener before existing listeners in the exact slot.
    pub prepend: bool,
    /// Claims this listener for at most one callback invocation.
    pub once: bool,
}

/// Synchronous ordered broadcast mode; every listener observes the same value.
#[derive(Clone, Copy, Debug)]
pub struct Emit;

/// Concurrent asynchronous all-settled broadcast mode.
#[derive(Clone, Copy, Debug)]
pub struct Parallel;

/// Ordered asynchronous mode that returns the first produced value.
#[derive(Clone, Copy, Debug)]
pub struct Serial;

/// Ordered synchronous mode that returns the first produced value.
#[derive(Clone, Copy, Debug)]
pub struct Bail;

/// Synchronous nested middleware mode.
#[derive(Clone, Copy, Debug)]
pub struct Waterfall;

/// Synchronous listener for an [`Emit`] event.
pub trait EmitEventHandler<E: LocalEvent<Mode = Emit>>: fmt::Debug + Send + Sync + 'static {
    /// Observes one immutable dispatch value.
    fn handle(&self, value: &E::Value);
}

/// Asynchronous listener for a [`Parallel`] event.
#[async_trait]
pub trait ParallelEventHandler<E: LocalEvent<Mode = Parallel>>:
    fmt::Debug + Send + Sync + 'static
{
    /// Handles one independently cloned dispatch value.
    async fn handle(&self, value: E::Value) -> std::result::Result<(), E::Error>;
}

/// Asynchronous listener for a [`Serial`] event.
#[async_trait]
pub trait SerialEventHandler<E: LocalEvent<Mode = Serial>>:
    fmt::Debug + Send + Sync + 'static
{
    /// Returns `Some` to complete the ordered dispatch or `None` to continue.
    async fn handle(&self, value: E::Value) -> std::result::Result<Option<E::Value>, E::Error>;
}

/// Synchronous listener for a [`Bail`] event.
pub trait BailEventHandler<E: LocalEvent<Mode = Bail>>: fmt::Debug + Send + Sync + 'static {
    /// Returns `Some` to complete the ordered dispatch or `None` to continue.
    fn handle(&self, value: &E::Value) -> std::result::Result<Option<E::Value>, E::Error>;
}

/// Synchronous nested middleware listener for a [`Waterfall`] event.
pub trait WaterfallEventHandler<E: LocalEvent<Mode = Waterfall>>:
    fmt::Debug + Send + Sync + 'static
{
    /// Calls `next` to delegate to the remaining middleware, or returns
    /// directly to short-circuit the remainder.
    fn handle(
        &self,
        value: E::Value,
        next: &mut dyn FnMut(E::Value) -> std::result::Result<E::Value, E::Error>,
    ) -> std::result::Result<E::Value, E::Error>;
}

mod sealed {
    pub trait Sealed {}
}

impl sealed::Sealed for Emit {}
impl sealed::Sealed for Parallel {}
impl sealed::Sealed for Serial {}
impl sealed::Sealed for Bail {}
impl sealed::Sealed for Waterfall {}

/// Sealed implementation contract for an event marker's fixed mode.
pub trait LocalEventMode<E: LocalEvent>: sealed::Sealed + Send + Sync + 'static {
    /// Exact listener trait accepted for this event mode.
    type Handler: ?Sized + Send + Sync + 'static;

    /// Sync result or owned async future produced by dispatch.
    type Dispatch;

    /// Dispatches one already snapshotted exact event slot.
    #[doc(hidden)]
    fn dispatch(snapshot: LocalEventSnapshot, value: E::Value) -> Self::Dispatch;
}

/// Type-erased listener retained only inside the Local event registry.
#[doc(hidden)]
pub struct LocalEventBinding {
    id: EventListenerId,
    handler: Arc<dyn Any + Send + Sync>,
    once: bool,
    claim_once: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl LocalEventBinding {
    pub(crate) fn new<H: ?Sized + Send + Sync + 'static>(
        id: EventListenerId,
        handler: Arc<H>,
        once: bool,
        claim_once: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            id,
            handler: Arc::new(handler),
            once,
            claim_once,
        }
    }

    fn handler<H: ?Sized + Send + Sync + 'static>(&self) -> Arc<H> {
        Arc::clone(
            self.handler
                .downcast_ref::<Arc<H>>()
                .expect("a typed Local event slot retains one exact handler trait"),
        )
    }

    fn claim(&self) -> bool {
        !self.once || (self.claim_once)()
    }
}

impl fmt::Debug for LocalEventBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalEventBinding")
            .field("id", &self.id)
            .field("once", &self.once)
            .finish_non_exhaustive()
    }
}

/// Runtime-owned listener snapshot whose destruction is panic-contained.
#[doc(hidden)]
pub struct LocalEventSnapshot {
    runtime: Runtime,
    bindings: Vec<Arc<LocalEventBinding>>,
}

impl fmt::Debug for LocalEventSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalEventSnapshot")
            .field("listeners", &self.bindings.len())
            .finish_non_exhaustive()
    }
}

impl LocalEventSnapshot {
    pub(crate) fn new(runtime: Runtime, bindings: Vec<Arc<LocalEventBinding>>) -> Self {
        Self { runtime, bindings }
    }

    fn bindings(&self) -> &[Arc<LocalEventBinding>] {
        &self.bindings
    }
}

impl Drop for LocalEventSnapshot {
    fn drop(&mut self) {
        while let Some(binding) = self.bindings.pop() {
            if drop_catching_unwind(binding) {
                self.runtime
                    .mark_terminal_owned("Local event listener destructor panicked");
            }
        }
    }
}

fn claim_parallel_callback<E: LocalEvent<Mode = Parallel>>(
    binding: &Arc<LocalEventBinding>,
    value: E::Value,
) -> Option<BoxFuture<'static, std::result::Result<(), E::Error>>> {
    if !binding.claim() {
        return None;
    }
    let handler = binding.handler::<dyn ParallelEventHandler<E>>();
    Some(Box::pin(async move { handler.handle(value).await }))
}

fn parallel_callbacks<E: LocalEvent<Mode = Parallel>>(
    bindings: Vec<Arc<LocalEventBinding>>,
    value: E::Value,
) -> impl futures_util::Stream<Item = BoxFuture<'static, std::result::Result<(), E::Error>>> + Send
{
    stream::unfold(
        (bindings.into_iter(), value),
        |(mut bindings, value)| async move {
            loop {
                let binding = bindings.next()?;
                if let Some(callback) = claim_parallel_callback::<E>(&binding, value.clone()) {
                    return Some((callback, (bindings, value)));
                }
            }
        },
    )
}

impl<E: LocalEvent<Mode = Emit>> LocalEventMode<E> for Emit {
    type Handler = dyn EmitEventHandler<E>;
    type Dispatch = ();

    fn dispatch(snapshot: LocalEventSnapshot, value: E::Value) {
        for binding in snapshot.bindings() {
            if binding.claim() {
                binding.handler::<Self::Handler>().handle(&value);
            }
        }
    }
}

impl<E: LocalEvent<Mode = Parallel>> LocalEventMode<E> for Parallel {
    type Handler = dyn ParallelEventHandler<E>;
    type Dispatch = BoxFuture<'static, std::result::Result<(), Vec<E::Error>>>;

    fn dispatch(snapshot: LocalEventSnapshot, value: E::Value) -> Self::Dispatch {
        Box::pin(async move {
            let bindings = snapshot.bindings().to_vec();
            let errors = parallel_callbacks::<E>(bindings, value)
                .buffered(MAXIMUM_PARALLEL_EVENT_CALLBACKS)
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .filter_map(std::result::Result::err)
                .collect::<Vec<_>>();
            drop(snapshot);
            if errors.is_empty() {
                Ok(())
            } else {
                Err(errors)
            }
        })
    }
}

impl<E: LocalEvent<Mode = Serial>> LocalEventMode<E> for Serial {
    type Handler = dyn SerialEventHandler<E>;
    type Dispatch = BoxFuture<'static, std::result::Result<Option<E::Value>, E::Error>>;

    fn dispatch(snapshot: LocalEventSnapshot, value: E::Value) -> Self::Dispatch {
        Box::pin(async move {
            for binding in snapshot.bindings() {
                if !binding.claim() {
                    continue;
                }
                if let Some(output) = binding
                    .handler::<Self::Handler>()
                    .handle(value.clone())
                    .await?
                {
                    return Ok(Some(output));
                }
            }
            Ok(None)
        })
    }
}

impl<E: LocalEvent<Mode = Bail>> LocalEventMode<E> for Bail {
    type Handler = dyn BailEventHandler<E>;
    type Dispatch = std::result::Result<Option<E::Value>, E::Error>;

    fn dispatch(snapshot: LocalEventSnapshot, value: E::Value) -> Self::Dispatch {
        for binding in snapshot.bindings() {
            if !binding.claim() {
                continue;
            }
            if let Some(output) = binding.handler::<Self::Handler>().handle(&value)? {
                return Ok(Some(output));
            }
        }
        Ok(None)
    }
}

impl<E: LocalEvent<Mode = Waterfall>> LocalEventMode<E> for Waterfall {
    type Handler = dyn WaterfallEventHandler<E>;
    type Dispatch = std::result::Result<E::Value, E::Error>;

    fn dispatch(snapshot: LocalEventSnapshot, value: E::Value) -> Self::Dispatch {
        fn call<E: LocalEvent<Mode = Waterfall>>(
            bindings: &[Arc<LocalEventBinding>],
            value: E::Value,
        ) -> std::result::Result<E::Value, E::Error> {
            let Some((binding, remaining)) = bindings.split_first() else {
                return Ok(value);
            };
            if !binding.claim() {
                return call::<E>(remaining, value);
            }
            let handler = binding.handler::<dyn WaterfallEventHandler<E>>();
            let mut next = |next_value| call::<E>(remaining, next_value);
            handler.handle(value, &mut next)
        }

        call::<E>(snapshot.bindings(), value)
    }
}
