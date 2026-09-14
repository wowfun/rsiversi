//! Per-connection admission, with an explicit clock input for deterministic tests.
use rsi_session_protocol::{Result, SessionError};
use std::sync::Mutex;
use tokio::sync::{Semaphore, SemaphorePermit};
use web_time::Instant;

#[derive(Debug)]
pub(super) struct Budget {
    workers: Semaphore,
    last: Mutex<Option<Instant>>,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            workers: Semaphore::new(2),
            last: Mutex::new(None),
        }
    }
}
impl Budget {
    pub(super) fn reserve(&self, now: Instant) -> Result<SemaphorePermit<'_>> {
        let capacity = || SessionError::Api(rsi_api_protocol::ApiError::Capacity);
        let permit = self.workers.try_acquire().map_err(|_| capacity())?;
        let mut last = self.last.lock().map_err(|_| capacity())?;
        if last.is_some_and(|last| {
            now.saturating_duration_since(last) < std::time::Duration::from_millis(250)
        }) {
            return Err(capacity());
        }
        *last = Some(now);
        Ok(permit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rate_and_inflight_bounds_release_on_drop_without_queued_work() {
        let budget = Budget::default();
        let now = Instant::now();
        let first = budget.reserve(now).unwrap();
        assert!(
            budget
                .reserve(now + std::time::Duration::from_millis(249))
                .is_err()
        );
        let second = budget
            .reserve(now + std::time::Duration::from_millis(250))
            .unwrap();
        assert!(
            budget
                .reserve(now + std::time::Duration::from_millis(500))
                .is_err()
        );
        drop(first);
        assert!(
            budget
                .reserve(now + std::time::Duration::from_millis(500))
                .is_ok()
        );
        drop(second);
        assert!(budget.reserve(now).is_err());
    }
}
