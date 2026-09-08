use rsi_meta::Execution;
use std::{
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(crate) struct Tasks(Arc<Owner>);
struct Owner {
    execution: Execution,
    stopped: CancellationToken,
    state: Mutex<State>,
    drained: Notify,
}
#[derive(Default)]
struct State {
    closed: bool,
    active: usize,
}
impl Tasks {
    pub fn new(execution: Execution) -> Self {
        Self(Arc::new(Owner {
            execution,
            stopped: CancellationToken::new(),
            state: Mutex::new(State::default()),
            drained: Notify::new(),
        }))
    }
    pub async fn close(&self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed = true;
        self.0.stopped.cancel();
        loop {
            let changed = self.0.drained.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self
                .0
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active
                == 0
            {
                break;
            }
            changed.await;
        }
    }
}
struct Lease(Arc<Owner>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active -= 1;
        self.0.drained.notify_waiters();
    }
}
impl<F: Future<Output = ()> + Send + 'static> hyper::rt::Executor<F> for Tasks {
    fn execute(&self, future: F) {
        {
            let mut state = self
                .0
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Admission and close share this fence. Hyper has 32 stream slots;
            // the extra task ceiling bounds helper tasks as well.
            if state.closed || state.active == 64 {
                return;
            }
            state.active += 1;
        }
        let lease = Lease(self.0.clone());
        drop(self.0.execution.spawn(async move {
            tokio::select! { biased;
                () = lease.0.stopped.cancelled() => {},
                () = future => {},
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::rt::Executor;
    use tokio::sync::Semaphore;

    struct Dropped(Arc<Semaphore>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.add_permits(1);
        }
    }

    #[tokio::test]
    async fn connection_close_drains_tasks_and_fences_escaped_executor() {
        let tasks = Tasks::new(Execution::native(tokio::runtime::Handle::current()));
        let dropped = Arc::new(Semaphore::new(0));
        for _ in 0..64 {
            let guard = Dropped(dropped.clone());
            tasks.execute(async move {
                let _guard = guard;
                futures_util::future::pending::<()>().await;
            });
        }
        assert_eq!(tasks.0.state.lock().unwrap().active, 64);
        tasks.close().await;
        assert_eq!(tasks.0.state.lock().unwrap().active, 0);
        assert_eq!(dropped.available_permits(), 64);
        let guard = Dropped(dropped.clone());
        tasks.execute(async move {
            let _guard = guard;
            panic!("closed executor ran new work");
        });
        assert_eq!(dropped.available_permits(), 65);
    }
}
