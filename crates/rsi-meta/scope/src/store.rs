use std::cell::RefCell;
use std::fmt;
use std::sync::{Arc, Mutex};

type UndoAction = Box<dyn FnOnce() + Send + 'static>;

thread_local! {
    static ACTIVE_UNDO_CAPTURES: RefCell<Vec<Vec<ScopeUndo>>> = const { RefCell::new(Vec::new()) };
}

/// Exact idempotent undo for one entry mutation.
#[derive(Clone)]
pub struct ScopeUndo {
    action: Arc<Mutex<Option<UndoAction>>>,
}

impl ScopeUndo {
    fn new(action: impl FnOnce() + Send + 'static) -> Self {
        let undo = Self {
            action: Arc::new(Mutex::new(Some(Box::new(action)))),
        };
        ACTIVE_UNDO_CAPTURES.with(|captures| {
            if let Some(capture) = captures.borrow_mut().last_mut() {
                capture.push(undo.clone());
            }
        });
        undo
    }

    /// Runs this exact undo at most once across every clone.
    pub fn run(&self) {
        let action = self.action.lock().expect("scope undo poisoned").take();
        if let Some(action) = action {
            action();
        }
    }

    pub(crate) fn same_action(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.action, &other.action)
    }
}

pub(crate) struct ScopeUndoCapture {
    active: bool,
}

impl ScopeUndoCapture {
    pub(crate) fn begin() -> Self {
        ACTIVE_UNDO_CAPTURES.with(|captures| captures.borrow_mut().push(Vec::new()));
        Self { active: true }
    }

    pub(crate) fn finish(mut self) -> Vec<ScopeUndo> {
        self.active = false;
        ACTIVE_UNDO_CAPTURES.with(|captures| {
            captures
                .borrow_mut()
                .pop()
                .expect("an active scope undo capture remains installed")
        })
    }
}

impl Drop for ScopeUndoCapture {
    fn drop(&mut self) {
        if self.active {
            ACTIVE_UNDO_CAPTURES.with(|captures| {
                captures
                    .borrow_mut()
                    .pop()
                    .expect("an active scope undo capture remains installed");
            });
        }
    }
}

impl fmt::Debug for ScopeUndo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let active = self.action.lock().expect("scope undo poisoned").is_some();
        formatter
            .debug_struct("ScopeUndo")
            .field("active", &active)
            .finish()
    }
}

struct NamedRecord<V> {
    token: Arc<()>,
    name: String,
    value: Arc<V>,
}

struct NamedState<V> {
    entries: Vec<NamedRecord<V>>,
}

/// Insertion-ordered named entries with exact independent ownership.
///
/// Duplicate insertion returns the rejected value so product code owns the
/// diagnostic and any replacement policy. Every read is an owned snapshot.
pub struct NamedEntries<V> {
    state: Arc<Mutex<NamedState<V>>>,
}

impl<V> NamedEntries<V>
where
    V: Send + Sync + 'static,
{
    /// Creates an empty named table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts one name, returning the value unchanged when it is duplicate.
    pub fn insert(&self, name: impl Into<String>, value: V) -> Result<ScopeUndo, V> {
        let name = name.into();
        let token = Arc::new(());
        {
            let mut state = self.state.lock().expect("named entries poisoned");
            if state.entries.iter().any(|entry| entry.name == name) {
                return Err(value);
            }
            state.entries.push(NamedRecord {
                token: Arc::clone(&token),
                name,
                value: Arc::new(value),
            });
        }
        let state = Arc::clone(&self.state);
        Ok(ScopeUndo::new(move || {
            let removed = {
                let mut state = state.lock().expect("named entries poisoned");
                state
                    .entries
                    .iter()
                    .position(|entry| Arc::ptr_eq(&entry.token, &token))
                    .map(|position| state.entries.remove(position))
            };
            drop(removed);
        }))
    }

    /// Returns an owned clone of one value without retaining the table lock.
    pub fn get(&self, name: &str) -> Option<V>
    where
        V: Clone,
    {
        let value = {
            let state = self.state.lock().expect("named entries poisoned");
            state
                .entries
                .iter()
                .find(|entry| entry.name == name)
                .map(|entry| Arc::clone(&entry.value))
        };
        value.map(|value| value.as_ref().clone())
    }

    /// Returns whether one name is present.
    pub fn contains(&self, name: &str) -> bool {
        self.state
            .lock()
            .expect("named entries poisoned")
            .entries
            .iter()
            .any(|entry| entry.name == name)
    }

    /// Returns an owned insertion-ordered snapshot.
    pub fn snapshot(&self) -> Vec<(String, V)>
    where
        V: Clone,
    {
        Self::into_owned_snapshot(self.shared_snapshot())
    }

    pub(crate) fn into_owned_snapshot(values: Vec<(String, Arc<V>)>) -> Vec<(String, V)>
    where
        V: Clone,
    {
        values
            .into_iter()
            .map(|(name, value)| (name, value.as_ref().clone()))
            .collect()
    }

    pub(crate) fn shared_snapshot(&self) -> Vec<(String, Arc<V>)> {
        self.state
            .lock()
            .expect("named entries poisoned")
            .entries
            .iter()
            .map(|entry| (entry.name.clone(), Arc::clone(&entry.value)))
            .collect()
    }

    /// Returns whether this table has no entries.
    pub fn is_empty(&self) -> bool {
        self.state
            .lock()
            .expect("named entries poisoned")
            .entries
            .is_empty()
    }

    /// Returns the current entry count.
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .expect("named entries poisoned")
            .entries
            .len()
    }
}

impl<V> Default for NamedEntries<V> {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(NamedState {
                entries: Vec::new(),
            })),
        }
    }
}

impl<V> fmt::Debug for NamedEntries<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let entries = self
            .state
            .lock()
            .expect("named entries poisoned")
            .entries
            .len();
        formatter
            .debug_struct("NamedEntries")
            .field("entries", &entries)
            .finish()
    }
}

struct AnonymousRecord<V> {
    token: Arc<()>,
    value: Arc<V>,
}

struct AnonymousState<V> {
    entries: Vec<AnonymousRecord<V>>,
}

/// Insertion-ordered anonymous entries with independent identity.
///
/// Equal values are distinct registrations and reads return owned snapshots.
pub struct AnonymousEntries<V> {
    state: Arc<Mutex<AnonymousState<V>>>,
}

impl<V> AnonymousEntries<V>
where
    V: Send + Sync + 'static,
{
    /// Creates an empty anonymous table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one independently owned value.
    pub fn append(&self, value: V) -> ScopeUndo {
        let token = Arc::new(());
        {
            self.state
                .lock()
                .expect("anonymous entries poisoned")
                .entries
                .push(AnonymousRecord {
                    token: Arc::clone(&token),
                    value: Arc::new(value),
                });
        }
        let state = Arc::clone(&self.state);
        ScopeUndo::new(move || {
            let removed = {
                let mut state = state.lock().expect("anonymous entries poisoned");
                state
                    .entries
                    .iter()
                    .position(|entry| Arc::ptr_eq(&entry.token, &token))
                    .map(|position| state.entries.remove(position))
            };
            drop(removed);
        })
    }

    /// Returns an owned insertion-ordered snapshot.
    pub fn snapshot(&self) -> Vec<V>
    where
        V: Clone,
    {
        let values = {
            let state = self.state.lock().expect("anonymous entries poisoned");
            state
                .entries
                .iter()
                .map(|entry| Arc::clone(&entry.value))
                .collect::<Vec<_>>()
        };
        values
            .into_iter()
            .map(|value| value.as_ref().clone())
            .collect()
    }

    /// Returns whether this table has no entries.
    pub fn is_empty(&self) -> bool {
        self.state
            .lock()
            .expect("anonymous entries poisoned")
            .entries
            .is_empty()
    }

    /// Returns the current entry count.
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .expect("anonymous entries poisoned")
            .entries
            .len()
    }
}

impl<V> Default for AnonymousEntries<V> {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(AnonymousState {
                entries: Vec::new(),
            })),
        }
    }
}

impl<V> fmt::Debug for AnonymousEntries<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let entries = self
            .state
            .lock()
            .expect("anonymous entries poisoned")
            .entries
            .len();
        formatter
            .debug_struct("AnonymousEntries")
            .field("entries", &entries)
            .finish()
    }
}
