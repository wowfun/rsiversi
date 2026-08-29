#![allow(dead_code)] // Each integration-test binary uses a different resolver subset.

use rsi_meta::{PluginFactory, ResolvedFactory, UpdateMode};
use std::sync::Arc;

pub(crate) fn resolved<T: PluginFactory>(factory: Arc<T>) -> ResolvedFactory {
    ResolvedFactory::linked("test", "1", UpdateMode::Replayable, factory)
}

pub(crate) fn resolved_dyn(factory: Arc<dyn PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked("test", "1", UpdateMode::Replayable, factory)
}
