//! Synchronous invalidation at the presentation state's mutation boundary.
use std::{
    ops::{Deref, DerefMut},
    sync::{LockResult, Mutex, MutexGuard, PoisonError},
};

#[derive(Debug)]
struct State<T> {
    revision: u64,
    value: T,
}
/// Presentation state whose mutable access invalidates cached document projections.
/// This revision is neither durable metadata nor an action authorization token.
#[derive(Debug)]
pub struct PresentationState<T>(Mutex<State<T>>);
impl<T: Default> Default for PresentationState<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}
impl<T> PresentationState<T> {
    /// Creates one independent presentation owner.
    pub fn new(value: T) -> Self {
        Self(Mutex::new(State { revision: 0, value }))
    }
    /// Locks this owner, preserving ordinary mutex poisoning semantics.
    pub fn lock(&self) -> LockResult<PresentationGuard<'_, T>> {
        match self.0.lock() {
            Ok(state) => Ok(PresentationGuard {
                state,
                mutated: false,
            }),
            Err(error) => Err(PoisonError::new(PresentationGuard {
                state: error.into_inner(),
                mutated: false,
            })),
        }
    }
    /// Captures the content revision under the owning lock.
    /// # Panics
    /// Panics if an earlier mutation poisoned this state.
    pub fn content_revision(&self) -> u64 {
        self.lock()
            .expect("presentation state poisoned")
            .state
            .revision
    }
}
/// Scoped presentation mutation; first mutable access advances its content revision.
#[derive(Debug)]
pub struct PresentationGuard<'a, T> {
    state: MutexGuard<'a, State<T>>,
    mutated: bool,
}
impl<T> PresentationGuard<'_, T> {
    /// Reads the revision while this owner's state remains locked.
    pub fn content_revision(&self) -> u64 {
        self.state.revision
    }
}
impl<T> Deref for PresentationGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.state.value
    }
}
impl<T> DerefMut for PresentationGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        if !self.mutated {
            self.state.revision = self
                .state
                .revision
                .checked_add(1)
                .expect("presentation revision exhausted");
            self.mutated = true;
        }
        &mut self.state.value
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reading_preserves_revision_and_mutation_publishes_before_unlock() {
        let state = PresentationState::new(Vec::<u8>::new());
        assert_eq!(state.content_revision(), 0);
        assert!(state.lock().unwrap().is_empty());
        assert_eq!(state.content_revision(), 0);
        {
            let mut value = state.lock().unwrap();
            value.push(1);
            value.push(2);
            assert_eq!(value.state.revision, 1);
        }
        assert_eq!(state.content_revision(), 1);
    }
}
