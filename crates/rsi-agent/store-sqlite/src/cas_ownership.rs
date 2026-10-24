//! Bounded per-hash ownership shared by publication, readers and collection.
use super::{Arc, BTreeMap, Connection, Mutex, Result, StoreError, sql_error};
use std::sync::Weak;
#[derive(Default)]
pub(super) struct Ownership(Mutex<BTreeMap<String, Weak<Mutex<State>>>>);
#[derive(Default)]
struct State {
    users: usize,
    deleting: bool,
}
pub(super) struct Lease {
    state: Arc<Mutex<State>>,
    exclusive: bool,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.exclusive {
            state.deleting = false;
        } else {
            state.users -= 1;
        }
    }
}
impl Ownership {
    pub(super) fn acquire(&self, hash: &str, exclusive: bool) -> Result<Arc<Lease>> {
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let gate = if let Some(gate) = entries.get(hash).and_then(Weak::upgrade) {
            gate
        } else {
            entries.retain(|_, entry| entry.strong_count() != 0);
            if entries.len() >= 1024 {
                return Err(StoreError::ReadCapacity);
            }
            let gate = Arc::new(Mutex::new(State::default()));
            entries.insert(hash.into(), Arc::downgrade(&gate));
            gate
        };
        let mut state = gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.deleting || (exclusive && state.users != 0) {
            return Err(StoreError::ReadCapacity);
        }
        if exclusive {
            state.deleting = true;
        } else {
            state.users = state.users.checked_add(1).ok_or(StoreError::ReadCapacity)?;
        }
        drop(state);
        Ok(Arc::new(Lease {
            state: gate,
            exclusive,
        }))
    }
}
pub(super) fn available(connection: &Connection, hash: &str) -> Result<()> {
    let deleting = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM cas_deletions WHERE sha256=?1 AND state='deleting')",
            [hash],
            |row| row.get::<_, bool>(0),
        )
        .map_err(sql_error)?;
    if deleting {
        return Err(StoreError::ReadCapacity);
    }
    Ok(())
}
