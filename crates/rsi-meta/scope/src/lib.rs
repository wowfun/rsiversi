//! Scope identity and layered contribution ownership above `rsi-meta` core.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

mod scope;
mod scoped_layers;
mod store;
mod target;

pub use scope::{ScopeError, ScopeHandle, ScopeKey, ScopeParentBinding, ScopeRoot};
pub use scoped_layers::{MutationError, ScopeLayer, ScopedLayers};
pub use store::{AnonymousEntries, NamedEntries, ScopeUndo};
pub use target::ScopeTarget;
