use crate::{Result, invalid};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

static AVAILABLE: AtomicUsize = AtomicUsize::new(7);

struct Reservation(usize);
impl Drop for Reservation {
    fn drop(&mut self) {
        AVAILABLE.fetch_add(self.0, Ordering::Relaxed);
    }
}

/// The archive or directory producer handles overflow itself, so a full queue
/// neither retains more files nor makes the caller idle.
pub(super) fn run<T: Send, W: FnMut(T) -> Result<()>>(
    files: usize,
    maximum: usize,
    worker: impl Fn() -> W + Sync,
    produce: impl FnOnce(&mut dyn FnMut(T) -> Result<()>) -> Result<()>,
) -> Result<()> {
    let mut local = worker();
    let wanted = files.div_ceil(512).saturating_sub(1).min(maximum);
    if wanted == 0 {
        return produce(&mut local);
    }
    let free = AVAILABLE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |free| {
            Some(free.saturating_sub(wanted))
        })
        .expect("reservation always returns an update");
    let reserved = Reservation(free.min(wanted));
    if reserved.0 == 0 {
        return produce(&mut local);
    }
    std::thread::scope(|scope| {
        let (sender, receiver) = mpsc::sync_channel(2 * reserved.0);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(reserved.0);
        for _ in 0..reserved.0 {
            let receiver = receiver.clone();
            let factory = &worker;
            workers.push(
                std::thread::Builder::new()
                    .name("rsi-payload-io".into())
                    .spawn_scoped(scope, move || -> Result<()> {
                        let mut consume = factory();
                        loop {
                            let item = receiver
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .recv();
                            let Ok(item) = item else { return Ok(()) };
                            consume(item)?;
                        }
                    })?,
            );
        }
        drop(receiver);
        let result = produce(&mut |item| match sender.try_send(item) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(item)) => local(item),
            Err(mpsc::TrySendError::Disconnected(_)) => Err(invalid("payload workers stopped")),
        });
        drop(sender);
        for thread in workers {
            thread
                .join()
                .map_err(|_| invalid("payload worker failed"))??;
        }
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_queued_file_error_reaches_the_publisher() {
        let result = run(4096, 3, || |()| Err(crate::Error::Digest), |send| send(()));
        assert!(matches!(result, Err(crate::Error::Digest)));
    }
}
