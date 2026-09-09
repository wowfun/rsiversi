//! Session-scoped Files browsing as ordinary shared UI contributions.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod browser;
mod view;
use async_trait::async_trait;
use browser::Browser;
use rsi_client::SessionControllerContract;
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use rsi_session_files::SessionFilesContract;
use rsi_session_protocol::SessionContract;
use rsi_ui::{ActionContribution, Contributions, SurfaceContribution, TargetKind, UiContract};
use std::sync::Arc;

/// UI-owned preference within the domain's file page bound.
pub const FILE_PAGE_BYTES: usize = 4096;
/// UI-owned preference within the domain's directory page bound.
pub const DIRECTORY_PAGE_ENTRIES: usize = 16;
/// Maximum UTF-8 path submitted through the browser form.
pub const INPUT_PATH_BYTES: usize = 4096;

/// Per-surface browser state; isolate with the actual controller contract.
#[derive(Debug)]
pub struct FilesBrowserContract;
impl LocalContract for FilesBrowserContract {
    const KEY: &'static str = "rsi.session.files.browser";
    type Service = Browser;
}
fn prepare(config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
    if !config.is_null() {
        return Err(MetaError::InvalidInput(
            "Files UI configuration must be null".into(),
        ));
    }
    Ok(PreparedActivation::new(ConfigValue::Null))
}
fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}

/// State provider owned by the actual Session surface Profile.
#[derive(Clone, Debug, Default)]
pub struct FilesUiTargetFactory;
#[async_trait]
impl PluginFactory for FilesUiTargetFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(config)?
            .requiring_local::<SessionControllerContract>()
            .requiring_local::<SessionContract>()
            .requiring_local::<SessionFilesContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let controller = plan.local::<SessionControllerContract>()?;
        let session = plan
            .local::<SessionContract>()?
            .attach(controller.session_id())
            .await
            .map_err(meta)?;
        let browser =
            Arc::new(Browser::new(session, plan.local::<SessionFilesContract>()?).map_err(meta)?);
        let cleanup = browser.clone();
        plan.defer(
            "drain Files browser",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.close().await;
                    Ok(())
                })
            }),
        )?;
        let supply = plan
            .context()
            .provide_local::<FilesBrowserContract>(browser)?;
        plan.defer(
            "withdraw Files browser",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

/// Shared Files browser card and actions, with no adapter-specific dispatch.
#[derive(Clone, Debug, Default)]
pub struct FilesUiFactory;
#[async_trait]
impl PluginFactory for FilesUiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(config)?.requiring_local::<UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<UiContract>()?
            .register(
                &plan,
                Contributions {
                    name: "rsi.session.files".into(),
                    surfaces: vec![SurfaceContribution {
                        name: "files".into(),
                        title: "Workspace files".into(),
                        target: TargetKind::Surface,
                        renderer: Arc::new(view::Card),
                    }],
                    actions: vec![ActionContribution {
                        name: "browse".into(),
                        target: TargetKind::Surface,
                        handler: Arc::new(browser::Browse),
                    }],
                    renderers: Vec::new(),
                },
            )
            .map_err(meta)?;
        plan.defer(
            "withdraw Files UI contributions",
            Box::new(move || {
                Box::pin(async move {
                    let report = lease.dispose().await;
                    if report.is_clean() {
                        Ok(())
                    } else {
                        Err("Files UI cleanup failed".into())
                    }
                })
            }),
        )
    }
}
