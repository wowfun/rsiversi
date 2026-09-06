//! One pause/deadline arbitration point shared by Kernel admission and executor.

use super::*;

#[derive(Debug, Default)]
pub(super) struct ElapsedState {
    state: Mutex<State>,
    changed: Notify,
}

#[derive(Debug, Default)]
struct State {
    paused_at: Option<u64>,
    excluded_ms: u64,
    expired: bool,
}

impl State {
    fn consumed(&self, accepted: u64, now: u64) -> u64 {
        self.paused_at
            .unwrap_or(now)
            .saturating_sub(accepted)
            .saturating_sub(self.excluded_ms)
    }
}

impl ElapsedState {
    pub(super) fn consumed(&self, accepted: u64, now: u64) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .consumed(accepted, now)
    }

    pub(super) fn pause(&self, accepted: u64, now: u64, limit: u64) -> TurnResult<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let consumed = state.consumed(accepted, now);
        if state.expired || consumed >= limit {
            state.expired = true;
            drop(state);
            self.changed.notify_waiters();
            return Err(TurnError::BudgetExceeded {
                dimension: BudgetDimension::Elapsed,
                consumed: consumed.max(limit),
                limit,
            });
        }
        if state.paused_at.is_some() {
            return Err(TurnError::Invalid("human wait is already parked".into()));
        }
        state.paused_at = Some(now);
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }

    pub(super) fn resume(&self, now: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(start) = state.paused_at.take() {
            state.excluded_ms = state.excluded_ms.saturating_add(now.saturating_sub(start));
        }
        drop(state);
        self.changed.notify_waiters();
    }
}

pub(super) struct Watch {
    pub(super) elapsed: Arc<ElapsedState>,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) accepted: u64,
    pub(super) limit: u64,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ElapsedBudgetWatch")
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl rsi_agent_turn_protocol::ElapsedBudget for Watch {
    fn consumed_ms(&self) -> u64 {
        self.elapsed.consumed(self.accepted, self.clock.now_ms())
    }

    async fn exhausted(&self) -> u64 {
        loop {
            let changed = self.elapsed.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let remaining = {
                let mut state = self
                    .elapsed
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let consumed = state.consumed(self.accepted, self.clock.now_ms());
                if state.expired || consumed >= self.limit {
                    state.expired = true;
                    return consumed.max(self.limit);
                }
                state.paused_at.is_none().then(|| self.limit - consumed)
            };
            if let Some(remaining) = remaining {
                tokio::select! {
                    () = &mut changed => {},
                    () = tokio::time::sleep(Duration::from_millis(remaining)) => {},
                }
            } else {
                changed.await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_turn_protocol::ElapsedBudget as _;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug)]
    struct TestClock(AtomicU64);
    impl Clock for TestClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::Acquire)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn human_wait_excludes_time_and_exhaustion_cannot_be_revived_by_parking() {
        let elapsed = Arc::new(ElapsedState::default());
        let clock = Arc::new(TestClock(AtomicU64::new(50)));
        let watch = Arc::new(Watch {
            elapsed: elapsed.clone(),
            clock: clock.clone(),
            accepted: 1,
            limit: 100,
        });
        elapsed.pause(1, 50, 100).unwrap();
        let waiter = tokio::spawn({
            let watch = watch.clone();
            async move { watch.exhausted().await }
        });
        clock.0.store(100_050, Ordering::Release);
        tokio::time::advance(Duration::from_secs(100)).await;
        assert!(!waiter.is_finished());
        assert_eq!(watch.consumed_ms(), 49);
        elapsed.resume(100_050);
        clock.0.store(100_101, Ordering::Release);
        tokio::time::advance(Duration::from_millis(51)).await;
        assert_eq!(waiter.await.unwrap(), 100);
        assert!(matches!(
            elapsed.pause(1, 100_101, 100),
            Err(TurnError::BudgetExceeded { .. })
        ));
    }
}
