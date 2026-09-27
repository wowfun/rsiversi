//! Explicit official native application catalog.
#![deny(unsafe_code)]
#![warn(missing_docs)]
use rsi::{
    ApplicationCatalog, ApplicationCatalogMetadata, ApplicationCatalogProvider,
    ApplicationProfileId, RsiError,
};
use rsi_meta::{PluginFactory, UpdateMode};
use std::sync::{Arc, LazyLock};
mod profiles;

/// Frozen official application catalog, with invocation-owned local Web inputs.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    web: rsi_serve::WebLaunchEnvironment,
}
impl Catalog {
    /// Captures local Web inputs without creating a Service or reading assets.
    pub fn new(web: rsi_serve::WebLaunchEnvironment) -> Self {
        Self { web }
    }
}
/// Official metadata shared by native applications and their Service daemons.
pub fn metadata() -> ApplicationCatalogMetadata {
    static METADATA: LazyLock<ApplicationCatalogMetadata> = LazyLock::new(|| {
        ApplicationCatalogMetadata::new(
            rsi::BASE_APPLICATION_PLUGINS
                .iter()
                .copied()
                .chain(APPLICATION_IDS.iter().copied())
                .chain(["rsi.terminal.ui", "rsi.terminal.portable"])
                .chain(if cfg!(target_os = "linux") {
                    Some("rsi.application.acp")
                } else {
                    None
                })
                .map(str::to_owned),
            profiles::builtins(),
        )
        .expect("valid linked application metadata")
    });
    METADATA.clone()
}
impl ApplicationCatalogProvider for Catalog {
    fn metadata(&self) -> &ApplicationCatalogMetadata {
        static METADATA: LazyLock<ApplicationCatalogMetadata> = LazyLock::new(metadata);
        &METADATA
    }
    fn build(
        &self,
        service: &rsi::StandardComposition,
        arguments: Vec<std::ffi::OsString>,
    ) -> rsi::Result<ApplicationCatalog> {
        let web = rsi_serve::WebLaunch::new(arguments.clone(), self.web.clone());
        let mut diagnostics = Vec::new();
        let mut builder = rsi::StandardAddonBuilder::new("rsi.standard.application");
        register_contracts(&mut builder)?;
        register_presentations(&mut builder)?;
        for (id, factory) in application_factories(arguments.clone(), web, &mut diagnostics) {
            builder
                .register_factory(
                    rsi::AddonScope::Application,
                    id,
                    env!("CARGO_PKG_VERSION"),
                    UpdateMode::RestartRequired,
                    factory,
                )
                .map_err(boot)?;
        }
        #[cfg(target_os = "linux")]
        builder
            .register_factory(
                rsi::AddonScope::Application,
                "rsi.application.acp",
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                Arc::new(rsi_acp_application::ApplicationFactory::new(arguments)),
            )
            .map_err(boot)?;
        let mut base = rsi::base_application_catalog(service.clone())?;
        base.addons = base
            .addons
            .merged(builder.build().map_err(boot)?)
            .map_err(boot)?;
        base.diagnostics.splice(0..0, diagnostics);
        Ok(base)
    }
}
fn boot(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(error.to_string())
}
fn diagnosed<T: PluginFactory + rsi_application::ApplicationDiagnostic + 'static>(
    factory: T,
    diagnostics: &mut Vec<Arc<dyn rsi_application::ApplicationDiagnostic>>,
) -> Arc<dyn PluginFactory> {
    let factory = Arc::new(factory);
    diagnostics.push(factory.clone());
    factory
}
fn register_presentations(builder: &mut rsi::StandardAddonBuilder) -> rsi::Result<()> {
    builder
        .register_local_contract_at::<rsi_terminal::presentation::FrameRendererContract>(
            rsi::AddonScope::Application,
        )
        .map_err(boot)?;
    for (id, factory) in [
        (
            "rsi.terminal.ui",
            Arc::new(rsi_terminal::presentation::LinkedPresentationFactory)
                as Arc<dyn PluginFactory>,
        ),
        (
            "rsi.terminal.portable",
            Arc::new(rsi_terminal::presentation::PortablePresentationFactory)
                as Arc<dyn PluginFactory>,
        ),
    ] {
        builder
            .register_factory(
                rsi::AddonScope::Application,
                id,
                env!("CARGO_PKG_VERSION"),
                UpdateMode::Replayable,
                factory,
            )
            .map_err(boot)?;
    }
    Ok(())
}

fn register_contracts(builder: &mut rsi::StandardAddonBuilder) -> rsi::Result<()> {
    let scope = rsi::AddonScope::Application;
    builder
        .register_local_contract_at::<rsi_workbench_ui::SetupFeatureContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_workbench_ui::PluginsFeatureContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_ui::UiContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_ui::UiTargetContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_api_http::HttpAssetsContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_web_assets::WebAssetControlContract>(scope)
        .map_err(boot)?;
    Ok(())
}
macro_rules! declare_factories {
    ($arguments:ident, $web:ident, $diagnostics:ident; $(($id:literal, $factory:expr $(,)?)),* $(,)?) => {
        const APPLICATION_IDS: &[&str] = &[$($id),*];
        fn application_factories($arguments: Vec<std::ffi::OsString>, $web: Arc<rsi_serve::WebLaunch>, $diagnostics: &mut Vec<Arc<dyn rsi_application::ApplicationDiagnostic>>) -> Vec<(&'static str, Arc<dyn PluginFactory>)> {
            vec![$(($id, $factory)),*]
        }
    }
}
declare_factories! { arguments, web, diagnostics;

        (
            "rsi.workbench.setup",
            Arc::new(rsi_workbench_ui::SetupFeatureFactory),
        ),
        (
            "rsi.workbench.plugins",
            Arc::new(rsi_workbench_ui::PluginsFeatureFactory),
        ),
        ("rsi.service.ui.client", Arc::new(rsi_service_ui::Factory)),
        (
            "rsi.workspace.review.ui",
            Arc::new(rsi_workspace_review_ui::Factory),
        ),
        ("rsi.ui", Arc::new(rsi_ui::UiFactory)),
        ("rsi.ui.target", Arc::new(rsi_ui::UiTargetFactory)),
        ("rsi.session.ui", Arc::new(rsi_session_ui::SessionUiFactory)),
        (
            "rsi.session.tree.ui",
            Arc::new(rsi_session_tree_ui::SessionTreeUiFactory),
        ),
        (
            "rsi.session.files.ui",
            Arc::new(rsi_session_files_ui::FilesUiFactory),
        ),
        ("rsi.application.web", diagnosed(rsi_serve::ServeFactory::local_web(web.clone()), diagnostics)),
        (
            "rsi.application.web-assets",
            diagnosed(rsi_serve::LocalWebAssetsFactory(web.clone()), diagnostics),
        ),
        ("rsi.application.serve-web", diagnosed(rsi_serve::ServeFactory::with_web_assets(arguments.clone()), diagnostics)),
        ("rsi.web.assets", diagnosed(rsi_web_assets::PairedWebAssetsFactory::new(rsi_build_info::family()), diagnostics)),
        ("rsi.application.devices", diagnosed(rsi_terminal::DevicesFactory::new(arguments.clone()), diagnostics)),
        ("rsi.application.inspector", diagnosed(rsi_terminal::InspectorFactory::new(arguments.clone()), diagnostics)),
        ("rsi.application.addons", diagnosed(rsi_terminal::NativeAddonsFactory::new(arguments.clone()), diagnostics)),
        ("rsi.application.cli", diagnosed(rsi_terminal::CliFactory::new(arguments.clone()), diagnostics)),
        ("rsi.application.headless", diagnosed(rsi_terminal::HeadlessFactory::new(arguments.clone()), diagnostics)),
        ("rsi.application.tui", diagnosed(rsi_terminal::TuiFactory::new(arguments.clone()), diagnostics)),
        ("rsi.application.serve", diagnosed(rsi_serve::ServeFactory::new(arguments.clone()), diagnostics)),
}

/// Selects the official catalog with no invocation-specific Web inputs or extras.
///
/// # Errors
/// Rejects Service metadata that differs from the official catalog metadata.
pub fn compose(service: rsi::StandardComposition) -> rsi::Result<rsi::ApplicationComposition> {
    rsi::ApplicationComposition::new(
        service,
        Arc::new(Catalog::default()),
        rsi::StandardAddonSet::default(),
    )
    .map_err(boot)
}
