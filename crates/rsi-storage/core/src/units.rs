use crate::{Result, StorageError};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, Weak},
};

/// Backend-local unit-name ownership, retained through actual background work.
#[derive(Debug, Default)]
pub struct UnitNames {
    names: Mutex<BTreeSet<String>>,
}

impl UnitNames {
    /// Reserves a validated descriptor's name before its medium is opened.
    pub fn reserve(self: &Arc<Self>, name: &str) -> Result<UnitLease> {
        let mut names = self
            .names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !names.insert(name.to_owned()) {
            return Err(StorageError::BackendUnavailable(format!(
                "unit `{name}` is already open"
            )));
        }
        Ok(UnitLease(Arc::new(Lease {
            name: name.to_owned(),
            owner: Arc::downgrade(self),
        })))
    }
}

/// Cloned by an actual I/O worker; the name is freed only after its final clone drops.
#[derive(Clone, Debug)]
pub struct UnitLease(Arc<Lease>);

impl UnitLease {
    /// Identity retained by this lease.
    pub fn name(&self) -> &str {
        &self.0.name
    }
}

#[derive(Debug)]
struct Lease {
    name: String,
    owner: Weak<UnitNames>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner
                .names
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&self.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_worker_ownership_outlives_the_requesting_handle() {
        let names = Arc::new(UnitNames::default());
        let unit = names.reserve("one").unwrap();
        let worker = unit.clone();
        drop(unit);
        assert!(names.reserve("one").is_err());
        let other = names.reserve("two").unwrap();
        drop(worker);
        let replacement = names.reserve("one").unwrap();
        drop((other, replacement));
        assert!(names.names.lock().unwrap().is_empty());
    }
}
