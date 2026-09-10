use super::{Backend, BoxFuture, Duration, Future, Pin, Poll, fmt, oneshot};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::task::Context;
use wasm_bindgen::{JsCast, closure::Closure};
use web_sys::{DedicatedWorkerGlobalScope, Performance};

#[cfg(target_feature = "atomics")]
compile_error!("browser Execution requires a single-threaded Worker without WASM atomics");

type TimerKey = (Duration, u64);

thread_local! {
    static SCHEDULER: RefCell<Option<Scheduler>> = const { RefCell::new(None) };
}

/// Browser bootstrap failed before any execution authority was constructed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrowserExecutionError(&'static str);

impl fmt::Display for BrowserExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}
impl std::error::Error for BrowserExecutionError {}

/// Current Worker's timer registrations; excludes its one bootstrap callback.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BrowserExecutionSnapshot {
    /// Rust waiters whose deadline has not fired or been cancelled.
    pub pending_timers: usize,
    /// Platform alarms: always zero or one.
    pub active_alarms: usize,
}

/// Observes this Worker's execution resources without creating registrations.
pub fn browser_resource_snapshot() -> BrowserExecutionSnapshot {
    SCHEDULER.with_borrow(|slot| {
        slot.as_ref()
            .map_or_else(BrowserExecutionSnapshot::default, |scheduler| {
                BrowserExecutionSnapshot {
                    pending_timers: scheduler.timers.len(),
                    active_alarms: usize::from(scheduler.alarm.is_some()),
                }
            })
    })
}

#[derive(Debug)]
pub(super) struct Browser;

impl Browser {
    pub(super) fn new() -> Result<Self, BrowserExecutionError> {
        SCHEDULER.with_borrow_mut(|slot| {
            if slot.is_none() {
                let worker = js_sys::global()
                    .dyn_into::<DedicatedWorkerGlobalScope>()
                    .map_err(|_| {
                        BrowserExecutionError("browser execution requires a Dedicated Worker")
                    })?;
                let performance = worker.performance().ok_or(BrowserExecutionError(
                    "Worker monotonic clock is unavailable",
                ))?;
                *slot = Some(Scheduler {
                    worker,
                    performance,
                    callback: Closure::new(fire),
                    alarm: None,
                    next_id: 0,
                    timers: BTreeMap::new(),
                });
            }
            Ok(Self)
        })
    }
}

impl Backend for Browser {
    fn spawn(&self, future: BoxFuture<'static, ()>) {
        wasm_bindgen_futures::spawn_local(future);
    }

    fn prepare(&self, job: Box<dyn FnOnce() + Send>) {
        wasm_bindgen_futures::spawn_local(async move {
            job();
        });
    }

    fn now(&self) -> Duration {
        SCHEDULER.with_borrow(|slot| slot.as_ref().expect("initialized Worker").now())
    }

    fn sleep_until(&self, instant: Duration) -> BoxFuture<'static, ()> {
        let timer = SCHEDULER.with_borrow_mut(|slot| {
            let scheduler = slot.as_mut().expect("initialized Worker");
            if instant <= scheduler.now() {
                return None;
            }
            scheduler.next_id = scheduler
                .next_id
                .checked_add(1)
                .expect("Worker timer ID exhausted");
            let key = (instant, scheduler.next_id);
            let (sender, receiver) = oneshot::channel();
            scheduler.timers.insert(key, sender);
            scheduler.arm();
            Some(Timer { key, receiver })
        });
        Box::pin(async move {
            if let Some(timer) = timer {
                timer.await;
            }
        })
    }
}

struct Scheduler {
    worker: DedicatedWorkerGlobalScope,
    performance: Performance,
    callback: Closure<dyn FnMut()>,
    alarm: Option<(Duration, i32)>,
    next_id: u64,
    timers: BTreeMap<TimerKey, oneshot::Sender<()>>,
}

impl Scheduler {
    fn now(&self) -> Duration {
        Duration::from_secs_f64(self.performance.now() / 1000.0)
    }

    fn arm(&mut self) {
        let first = self.timers.first_key_value().map(|(key, _)| key.0);
        if self.alarm.as_ref().map(|(instant, _)| *instant) == first {
            return;
        }
        if let Some((_, handle)) = self.alarm.take() {
            self.worker.clear_timeout_with_handle(handle);
        }
        if let Some(instant) = first {
            let remaining = instant.saturating_sub(self.now());
            // Round up to prevent an early callback from turning into a busy loop.
            // Long waits rearm at the platform's signed-millisecond ceiling.
            let millis =
                i32::try_from(remaining.as_nanos().div_ceil(1_000_000)).unwrap_or(i32::MAX);
            let handle = self
                .worker
                .set_timeout_with_callback_and_timeout_and_arguments_0(
                    self.callback.as_ref().unchecked_ref(),
                    millis,
                )
                .expect("Worker alarm registration failed");
            self.alarm = Some((instant, handle));
        }
    }
}

fn fire() {
    let due = SCHEDULER.with_borrow_mut(|slot| {
        let scheduler = slot.as_mut().expect("initialized Worker");
        scheduler.alarm = None;
        let now = scheduler.now();
        let mut due = Vec::new();
        while scheduler
            .timers
            .first_key_value()
            .is_some_and(|(key, _)| key.0 <= now)
        {
            due.push(
                scheduler
                    .timers
                    .pop_first()
                    .expect("observed first timer")
                    .1,
            );
        }
        scheduler.arm();
        due
    });
    // Sending or dropping an owned sender can invoke caller wakers. Never do
    // either while the scheduler is borrowed.
    for sender in due {
        let _ = sender.send(());
    }
}

struct Timer {
    key: TimerKey,
    receiver: oneshot::Receiver<()>,
}

impl Future for Timer {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.receiver)
            .poll(context)
            .map(|result| result.expect("Worker timer lost its owner"))
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let removed = SCHEDULER.with_borrow_mut(|slot| {
            let scheduler = slot.as_mut().expect("initialized Worker");
            let removed = scheduler.timers.remove(&self.key);
            if removed.is_some() {
                scheduler.arm();
            }
            removed
        });
        drop(removed);
    }
}
