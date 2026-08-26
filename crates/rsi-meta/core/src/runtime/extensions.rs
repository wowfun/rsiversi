use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// Associates one marker type with the safe-Rust value stored in a [`Context`](super::Context).
///
/// Marker identity is process-local Rust type identity. Extension values are
/// inherited by derived Contexts, but are never serialized or exposed through
/// a native boundary.
pub trait ContextExtension: 'static {
    /// The immutable value associated with this marker type.
    type Value: Send + Sync + 'static;
}

type ExtensionValue = Arc<dyn Any + Send + Sync>;

#[derive(Clone, Default)]
pub(crate) struct ContextExtensions {
    values: HashMap<TypeId, ExtensionValue>,
}

impl fmt::Debug for ContextExtensions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContextExtensions")
            .field("entries", &self.values.len())
            .finish()
    }
}

impl ContextExtensions {
    pub(super) fn contains<K: ContextExtension>(&self) -> bool {
        self.values.contains_key(&TypeId::of::<K>())
    }

    pub(super) fn insert<K: ContextExtension>(&mut self, value: K::Value) {
        self.values.insert(TypeId::of::<K>(), Arc::new(value));
    }

    pub(crate) fn get<K: ContextExtension>(&self) -> Option<Arc<K::Value>> {
        Arc::downcast(self.values.get(&TypeId::of::<K>())?.clone()).ok()
    }
}
