//! Scope identity and layered contribution ownership above `rsi-meta` core.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

mod scope;
mod scoped_layers;
mod store;

pub use scope::{ScopeError, ScopeHandle, ScopeKey, ScopeParentBinding, ScopeRoot, ScopedContext};
pub use scoped_layers::{LayerContext, MutationError, ScopeLayer, ScopedLayers};
pub use store::{AnonymousEntries, NamedEntries, ScopeUndo};
