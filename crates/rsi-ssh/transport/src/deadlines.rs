use crate::Shared;
use std::sync::Arc;

pub(crate) async fn run(shared: Arc<Shared>) {
    loop {
        let changed = shared.requests_changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let deadline = {
            let state = shared.lock();
            state
                .pending
                .values()
                .map(|call| call.deadline)
                .chain(state.heartbeat_deadline)
                .min()
        };
        if let Some(deadline) = deadline {
            tokio::select! {
                () = shared.stop.cancelled() => return,
                () = &mut changed => {},
                () = tokio::time::sleep_until(deadline) => {
                    // A complete reply may have won the same scheduling turn.
                    let expired = {
                        let mut state = shared.lock();
                        let now = tokio::time::Instant::now();
                        let expired = state.pending.values().any(|call| call.deadline <= now) || state.heartbeat_deadline.is_some_and(|deadline| deadline <= now);
                        if expired { state.close(); }
                        expired
                    };
                    if expired { return; }
                },
            }
        } else {
            tokio::select! {
                () = shared.stop.cancelled() => return,
                () = &mut changed => {},
            }
        }
    }
}
