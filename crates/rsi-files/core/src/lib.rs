//! Ordinary generation-owned native read-only filesystem provider.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_files_protocol::FilesContract;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::sync::Arc;

mod service;
pub use service::LocalFiles;

/// Ordinary owner of one read service and its retained tokens.
#[derive(Clone, Debug, Default)]
pub struct FilesFactory;
#[async_trait]
impl PluginFactory for FilesFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Files configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let service = Arc::new(
            LocalFiles::new().map_err(|error| MetaError::InvalidInput(error.to_string()))?,
        );
        let cleanup = service.clone();
        plan.defer(
            "stop Files",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.close().await;
                    Ok(())
                })
            }),
        )?;
        let supply = plan.context().provide_local::<FilesContract>(service)?;
        plan.defer(
            "withdraw Files",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
