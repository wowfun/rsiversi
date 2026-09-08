use crate::{ScopeError, ScopeKey, ScopeRoot};
use rsi_meta::{
    MetaError, RegistrationContext, RegistrationLease, RegistrationOrderSnapshot,
    RegistrationPosition, RuntimeIdentity,
};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

struct Entry<V: ?Sized> {
    scope: Option<ScopeKey>,
    position: RegistrationPosition,
    value: Arc<V>,
}

struct Cache<V: ?Sized> {
    chain: Vec<ScopeKey>,
    order: RegistrationOrderSnapshot,
    entries: Vec<Arc<Entry<V>>>,
    values: Arc<[Arc<V>]>,
}

struct State<V: ?Sized> {
    next: u64,
    entries: BTreeMap<u64, Arc<Entry<V>>>,
    cache: Option<Cache<V>>,
}

/// Bounded declaration-ordered contributions with explicit scope visibility.
///
/// This product-owned table has no Runtime service or ambient scope identity.
/// Equal values remain independent registrations. Callers execute business
/// callbacks only after obtaining their immutable snapshot.
pub struct ScopedContributions<V: ?Sized> {
    runtime: RuntimeIdentity,
    root: ScopeRoot,
    maximum: usize,
    state: Arc<Mutex<State<V>>>,
}

impl<V: ?Sized + Send + Sync + 'static> ScopedContributions<V> {
    /// Creates a table with an explicit nonzero total live-entry bound.
    pub fn new(
        runtime: RuntimeIdentity,
        root: ScopeRoot,
        maximum_entries: usize,
    ) -> Result<Self, ScopeError> {
        if maximum_entries == 0 {
            return Err(
                MetaError::InvalidInput("contribution entry bound must be nonzero".into()).into(),
            );
        }
        Ok(Self {
            runtime,
            root,
            maximum: maximum_entries,
            state: Arc::new(Mutex::new(State {
                next: 0,
                entries: BTreeMap::new(),
                cache: None,
            })),
        })
    }

    /// Registers one value under the exact generation and optional scope.
    /// Loading joins setup rollback; Active owns a dynamic effect.
    pub fn register(
        &self,
        context: &RegistrationContext,
        scope: Option<&ScopeKey>,
        value: Arc<V>,
    ) -> Result<RegistrationLease, ScopeError> {
        if context.runtime_identity() != self.runtime {
            return Err(
                MetaError::InvalidInput("contribution belongs to another Runtime".into()).into(),
            );
        }
        if let Some(scope) = scope {
            self.root.ensure_local(scope)?;
        }
        let id = {
            let mut state = self.state.lock().expect("contribution state poisoned");
            state.next = state
                .next
                .checked_add(1)
                .ok_or(MetaError::CapacityExhausted {
                    resource: "scoped contribution identities",
                })?;
            state.next
        };
        let weak = Arc::downgrade(&self.state);
        let ((), lease) = context.register(
            "withdraw scoped contribution",
            move || {
                if let Some(state) = weak.upgrade() {
                    let removed = {
                        let mut state = state.lock().expect("contribution state poisoned");
                        let removed = state.entries.remove(&id);
                        let invalidates = removed.as_ref().is_some_and(|removed| {
                            state.cache.as_ref().is_some_and(|cache| {
                                cache
                                    .entries
                                    .iter()
                                    .any(|entry| Arc::ptr_eq(entry, removed))
                            })
                        });
                        let cache = invalidates.then(|| state.cache.take());
                        (removed, cache)
                    };
                    drop(removed);
                }
                Ok(())
            },
            |position| {
                let mut state = self.state.lock().expect("contribution state poisoned");
                if state.entries.len() >= self.maximum {
                    return Err(MetaError::CapacityExhausted {
                        resource: "scoped contributions",
                    });
                }
                state.entries.insert(
                    id,
                    Arc::new(Entry {
                        scope: scope.cloned(),
                        position,
                        value,
                    }),
                );
                if state
                    .cache
                    .as_ref()
                    .is_some_and(|cache| scope.is_none_or(|scope| cache.chain.contains(scope)))
                {
                    state.cache = None;
                }
                Ok(())
            },
        )?;
        Ok(lease)
    }

    /// Captures global then farthest-to-nearest contributions in declaration order.
    /// Reuses the last Arc while its selected membership and order are unchanged.
    pub fn snapshot(&self, scope: Option<&ScopeKey>) -> Result<Arc<[Arc<V>]>, ScopeError> {
        let mut chain = scope.map_or_else(|| Ok(Vec::new()), |scope| self.root.chain(scope))?;
        chain.reverse();
        let mut state = self.state.lock().expect("contribution state poisoned");
        if let Some(cache) = &state.cache
            && cache.chain == chain
            && cache.order.is_current()
            && cache
                .entries
                .iter()
                .all(|entry| entry.position.is_admitting())
        {
            return Ok(cache.values.clone());
        }
        let eligible: Vec<_> = state
            .entries
            .values()
            .filter_map(|entry| {
                if !entry.position.is_admitting() {
                    return None;
                }
                let layer = entry.scope.as_ref().map_or(Some(0), |scope| {
                    chain
                        .iter()
                        .position(|key| key == scope)
                        .map(|index| index + 1)
                })?;
                Some((layer, entry.clone()))
            })
            .collect();
        let positions: Vec<_> = eligible
            .iter()
            .map(|(_, entry)| entry.position.clone())
            .collect();
        let order = RegistrationOrderSnapshot::capture(&positions)?;
        let mut ranked: Vec<_> = eligible.into_iter().zip(order.ranks()).collect();
        ranked.sort_by(|((left, _), left_rank), ((right, _), right_rank)| {
            left.cmp(right).then_with(|| left_rank.cmp(right_rank))
        });
        let entries: Vec<_> = ranked.into_iter().map(|((_, entry), _)| entry).collect();
        let values = state
            .cache
            .as_ref()
            .filter(|cache| {
                cache.entries.len() == entries.len()
                    && cache
                        .entries
                        .iter()
                        .zip(&entries)
                        .all(|(left, right)| Arc::ptr_eq(left, right))
            })
            .map_or_else(
                || entries.iter().map(|entry| entry.value.clone()).collect(),
                |cache| cache.values.clone(),
            );
        state.cache = Some(Cache {
            chain,
            order,
            entries,
            values: values.clone(),
        });
        Ok(values)
    }
}

impl<V: ?Sized> fmt::Debug for ScopedContributions<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScopedContributions")
            .field(
                "entries",
                &self
                    .state
                    .lock()
                    .expect("contribution state poisoned")
                    .entries
                    .len(),
            )
            .field("maximum", &self.maximum)
            .finish_non_exhaustive()
    }
}
