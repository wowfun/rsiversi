//! Settings-owned namespace API endpoint and client plugins.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod client;
mod endpoint;
mod wire;
pub use client::{SettingsClient, SettingsClientFactory};
pub use endpoint::{SettingsApi, SettingsApiFactory};

fn prepare(config: &rsi_meta::ConfigValue) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
    if !config.is_null() && !config.as_object().is_some_and(serde_json::Map::is_empty) {
        return Err(rsi_meta::MetaError::InvalidInput(
            "Settings API configuration must be null or empty".into(),
        ));
    }
    Ok(rsi_meta::PreparedActivation::new(
        rsi_meta::ConfigValue::Null,
    ))
}
