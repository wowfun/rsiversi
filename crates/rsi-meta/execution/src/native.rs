use super::{Backend, BoxFuture, Duration};
use tokio::runtime::Handle;
use tokio::time::Instant;

#[derive(Debug)]
pub(super) struct Native {
    handle: Handle,
    origin: Instant,
}

impl Native {
    pub(super) fn new(handle: Handle) -> Self {
        let origin = {
            let _entered = handle.enter();
            Instant::now()
        };
        Self { handle, origin }
    }
}

impl Backend for Native {
    fn spawn(&self, future: BoxFuture<'static, ()>) {
        self.handle.spawn(future);
    }

    fn prepare(&self, job: Box<dyn FnOnce() + Send>) {
        self.handle.spawn_blocking(job);
    }

    fn now(&self) -> Duration {
        let _entered = self.handle.enter();
        Instant::now().saturating_duration_since(self.origin)
    }

    fn sleep_until(&self, instant: Duration) -> BoxFuture<'static, ()> {
        let _entered = self.handle.enter();
        Box::pin(tokio::time::sleep_until(self.origin + instant))
    }
}
