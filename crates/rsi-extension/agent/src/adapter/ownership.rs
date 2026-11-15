use super::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug)]
pub(super) struct Ownership(Mutex<Option<OwnedSemaphorePermit>>);
impl Ownership {
    pub(super) fn reserve(budget: &Arc<Semaphore>) -> Option<Arc<Self>> {
        budget
            .clone()
            .try_acquire_owned()
            .ok()
            .map(|permit| Arc::new(Self(Mutex::new(Some(permit)))))
    }
    pub(super) fn release(&self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unresolved_and_prepared_owners_share_one_bound_until_definite_settlement() {
        let budget = Arc::new(Semaphore::new(256));
        let mut unknown: Vec<_> = (0..256)
            .map(|_| Ownership::reserve(&budget).unwrap())
            .collect();
        assert!(Ownership::reserve(&budget).is_none());
        let prepared = unknown[0].clone();
        unknown.remove(0);
        assert!(Ownership::reserve(&budget).is_none());
        prepared.release();
        let next = Ownership::reserve(&budget).unwrap();
        prepared.release();
        assert!(Ownership::reserve(&budget).is_none());
        drop((next, unknown));
        assert_eq!(budget.available_permits(), 256);
    }
}
