use super::panic_boundary::{caught_panic, drop_catching_unwind};
use super::{BoundedDiagnostic, ChangeFuture};
use std::panic::AssertUnwindSafe;
use std::task::Poll;

struct ChangeFutureGuard {
    future: Option<ChangeFuture>,
}

impl ChangeFutureGuard {
    fn new(future: ChangeFuture) -> Self {
        Self {
            future: Some(future),
        }
    }

    fn poll(&mut self, context: &mut std::task::Context<'_>) -> Poll<Result<(), String>> {
        self.future
            .as_mut()
            .expect("change future guard retains its future")
            .as_mut()
            .poll(context)
    }

    fn destroy(mut self) -> bool {
        drop_catching_unwind(
            self.future
                .take()
                .expect("change future guard destroys its future once"),
        )
    }
}

impl Drop for ChangeFutureGuard {
    fn drop(&mut self) {
        if let Some(future) = self.future.take() {
            let _destruction_panicked = drop_catching_unwind(future);
        }
    }
}

pub(super) async fn run_change_future(
    future: ChangeFuture,
    maximum: usize,
) -> Result<(), BoundedDiagnostic> {
    let mut future = ChangeFutureGuard::new(future);
    let outcome = std::future::poll_fn(|context| {
        match std::panic::catch_unwind(AssertUnwindSafe(|| future.poll(context))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(result)) => Poll::Ready(Ok(result)),
            Err(payload) => Poll::Ready(Err(payload)),
        }
    })
    .await;
    let destruction_panicked = future.destroy();
    let primary = match outcome {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(BoundedDiagnostic::from_string(error, maximum)),
        Err(payload) => Some(caught_panic(
            payload,
            "scope change callback panicked",
            "scope change callback panic payload destruction panicked",
            maximum,
        )),
    };
    let destruction = destruction_panicked.then(|| {
        BoundedDiagnostic::from_string(
            "scope change callback future destruction panicked".to_owned(),
            maximum,
        )
    });
    match (primary, destruction) {
        (None, None) => Ok(()),
        (Some(failure), None) | (None, Some(failure)) => Err(failure),
        (Some(primary), Some(destruction)) => {
            let inherited_truncation = primary.truncated || destruction.truncated;
            let mut combined = BoundedDiagnostic::from_string(
                format!("{}; {}", primary.message, destruction.message),
                maximum,
            );
            combined.truncated |= inherited_truncation;
            Err(combined)
        }
    }
}
