//! Process-local Session admission and read-to-source ownership transfer.
use crate::{Result, SessionError};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Open,
    Closing,
    Retiring,
    Purged,
}
#[derive(Debug)]
struct State {
    epoch: u64,
    phase: Phase,
    reads: usize,
    hard: usize,
    cancel: CancellationToken,
}
/// One shared lifecycle gate for an exact Session in a service generation.
#[derive(Debug)]
pub struct SessionLifecycle {
    state: Mutex<State>,
    changed: Notify,
}
impl Default for SessionLifecycle {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                epoch: 1,
                phase: Phase::Open,
                reads: 0,
                hard: 0,
                cancel: CancellationToken::new(),
            }),
            changed: Notify::new(),
        }
    }
}
/// A handle's immutable admission generation; reopening does not renew it.
#[derive(Clone, Debug)]
pub struct SessionEpoch {
    gate: Arc<SessionLifecycle>,
    epoch: u64,
}
/// An actual operation owner, retained until the work has really drained.
#[derive(Debug)]
pub struct SessionLifecycleActivity {
    gate: Arc<SessionLifecycle>,
    epoch: u64,
    hard: bool,
    cancel: CancellationToken,
}
/// Exclusive closing owner; only pre-retirement failure can reopen admission.
#[derive(Debug)]
pub struct SessionClosing {
    gate: Arc<SessionLifecycle>,
    epoch: u64,
}
impl SessionLifecycle {
    // A poisoned partially transitioned gate cannot safely admit new ownership.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("Session lifecycle poisoned")
    }

    /// Binds a newly attached handle to the current open generation.
    pub fn attach(self: &Arc<Self>) -> Result<SessionEpoch> {
        let state = self.lock();
        if state.phase != Phase::Open {
            return Err(SessionError::ShuttingDown);
        }
        Ok(SessionEpoch {
            gate: self.clone(),
            epoch: state.epoch,
        })
    }
    /// Samples hard ownership without changing admission or cancelling readers.
    pub fn has_hard_owners(&self) -> bool {
        let state = self.lock();
        state.phase != Phase::Open || state.hard != 0
    }
    /// Atomically closes distinct gates in caller-supplied stable identity order.
    /// No reader is cancelled unless every gate admits retirement.
    pub fn close_many(gates: &[Arc<Self>]) -> Result<Vec<SessionClosing>> {
        if gates.len() > 256
            || gates
                .iter()
                .enumerate()
                .any(|(i, g)| gates[..i].iter().any(|old| Arc::ptr_eq(old, g)))
        {
            return Err(SessionError::Invalid(
                "duplicate or excessive lifecycle gates".into(),
            ));
        }
        let mut states = Vec::with_capacity(gates.len());
        for gate in gates {
            states.push(gate.state.try_lock().map_err(|_| SessionError::Capacity)?);
        }
        if states
            .iter()
            .any(|state| state.phase != Phase::Open || state.hard != 0)
        {
            return Err(SessionError::Capacity);
        }
        let mut owners = Vec::with_capacity(gates.len());
        for (gate, state) in gates.iter().zip(&mut states) {
            state.phase = Phase::Closing;
            state.cancel.cancel();
            owners.push(SessionClosing {
                gate: gate.clone(),
                epoch: state.epoch,
            });
        }
        Ok(owners)
    }
}
impl SessionEpoch {
    /// Rejects closed or previous generations, including cached Header reads.
    pub fn check(&self) -> Result<()> {
        let state = self.gate.lock();
        if state.phase != Phase::Open || state.epoch != self.epoch {
            Err(SessionError::ShuttingDown)
        } else {
            Ok(())
        }
    }
    /// Admits a cancellable read with a retained completion guard.
    pub fn read(&self) -> Result<SessionLifecycleActivity> {
        self.begin(false)
    }
    /// Admits mutation or live-source work that must block maintenance.
    pub fn hard(&self) -> Result<SessionLifecycleActivity> {
        self.begin(true)
    }
    fn begin(&self, hard: bool) -> Result<SessionLifecycleActivity> {
        let mut state = self.gate.lock();
        if state.phase != Phase::Open || state.epoch != self.epoch {
            return Err(SessionError::ShuttingDown);
        }
        let count = if hard {
            &mut state.hard
        } else {
            &mut state.reads
        };
        *count = count.checked_add(1).ok_or(SessionError::Capacity)?;
        Ok(SessionLifecycleActivity {
            gate: self.gate.clone(),
            epoch: self.epoch,
            hard,
            cancel: state.cancel.clone(),
        })
    }
}
impl SessionLifecycleActivity {
    /// Cancels only when this admission generation closes.
    pub fn retiring(&self) -> &CancellationToken {
        &self.cancel
    }
    /// Atomically promotes the exact read owner before it can authorize native work.
    pub fn promote(&mut self) -> Result<()> {
        let mut state = self.gate.lock();
        if state.phase != Phase::Open || state.epoch != self.epoch {
            return Err(SessionError::ShuttingDown);
        }
        if !self.hard {
            state.hard = state.hard.checked_add(1).ok_or(SessionError::Capacity)?;
            state.reads -= 1;
            self.hard = true;
        }
        Ok(())
    }
}
impl Drop for SessionLifecycleActivity {
    fn drop(&mut self) {
        let mut state = self.gate.lock();
        if self.hard {
            state.hard -= 1;
        } else {
            state.reads -= 1;
        }
        drop(state);
        self.gate.changed.notify_waiters();
    }
}
impl SessionClosing {
    /// Waits for actual retained read ownership, with registration before observation.
    pub async fn drained(&self) {
        loop {
            let changed = self.gate.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.gate.lock().reads == 0 {
                return;
            }
            changed.await;
        }
    }
    /// Records that Store acknowledged durable retirement. No subsequent error reopens it.
    pub fn retiring(&self) {
        let mut state = self.gate.lock();
        // The move-only closing owner keeps this epoch closed until its Drop.
        debug_assert_eq!(state.epoch, self.epoch);
        if state.phase != Phase::Purged {
            state.phase = Phase::Retiring;
        }
    }
    /// Marks the same durable retirement complete after the Store receipt confirms it.
    pub fn purged(&self) -> Result<()> {
        let mut state = self.gate.lock();
        if state.epoch != self.epoch || state.phase != Phase::Retiring {
            return Err(SessionError::ShuttingDown);
        }
        state.phase = Phase::Purged;
        Ok(())
    }
}
impl Drop for SessionClosing {
    fn drop(&mut self) {
        let mut state = self.gate.lock();
        if state.epoch == self.epoch
            && state.phase == Phase::Closing
            && let Some(epoch) = state.epoch.checked_add(1)
        {
            state.epoch = epoch;
            state.phase = Phase::Open;
            state.cancel = CancellationToken::new();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_close_never_cancels_an_earlier_reader_when_a_later_owner_is_busy() {
        let first = Arc::new(SessionLifecycle::default());
        let second = Arc::new(SessionLifecycle::default());
        let read = first.attach().unwrap().read().unwrap();
        let hard = second.attach().unwrap().hard().unwrap();
        assert!(SessionLifecycle::close_many(&[first.clone(), second.clone()]).is_err());
        assert!(!read.retiring().is_cancelled());
        assert!(first.attach().is_ok());
        drop(hard);
        let closing = SessionLifecycle::close_many(&[first.clone(), second.clone()]).unwrap();
        assert!(read.retiring().is_cancelled());
        assert!(first.attach().is_err());
        assert!(second.attach().is_err());
        drop(read);
        drop(closing);
        assert!(first.attach().is_ok());
        assert!(second.attach().is_ok());
    }
    #[tokio::test]
    async fn promotion_close_and_rollback_are_one_atomic_generation_boundary() {
        let gate = Arc::new(SessionLifecycle::default());
        let old = gate.attach().unwrap();
        let mut read = old.read().unwrap();
        read.promote().unwrap();
        assert!(matches!(
            SessionLifecycle::close_many(std::slice::from_ref(&gate)),
            Err(SessionError::Capacity)
        ));
        assert!(!read.retiring().is_cancelled());
        drop(read);
        let mut read = old.read().unwrap();
        let closing = SessionLifecycle::close_many(std::slice::from_ref(&gate))
            .unwrap()
            .remove(0);
        assert!(read.retiring().is_cancelled());
        assert!(read.promote().is_err());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), closing.drained())
                .await
                .is_err()
        );
        drop(read);
        closing.drained().await;
        drop(closing);
        assert!(old.hard().is_err());
        assert!(gate.attach().unwrap().read().is_ok());
        let closing = SessionLifecycle::close_many(std::slice::from_ref(&gate))
            .unwrap()
            .remove(0);
        closing.retiring();
        closing.retiring();
        closing.purged().unwrap();
        closing.retiring();
        assert_eq!(gate.lock().phase, Phase::Purged);
        drop(closing);
        assert!(gate.attach().is_err());
    }
}
