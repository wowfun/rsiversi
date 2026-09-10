//! Domain-owned Workspace endpoints and client proxy over the generic API.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod client;
mod endpoint;
mod wire;
pub use client::{WorkspaceClient, WorkspaceClientFactory};
pub use endpoint::{WorkspaceApi, WorkspaceApiFactory};

fn empty(config: &rsi_meta::ConfigValue) -> rsi_meta::Result<()> {
    if config.is_null() || config.as_object().is_some_and(serde_json::Map::is_empty) {
        Ok(())
    } else {
        Err(rsi_meta::MetaError::InvalidInput(
            "Workspace API configuration must be null or empty".into(),
        ))
    }
}
