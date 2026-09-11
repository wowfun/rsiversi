use crate::{ApplicationComposition, Result, RsiError};
use rsi_host::{ProfileProgram, RunningHost};
use std::ffi::OsString;

/// Starts the product's stable bootstrap and its ordinary Application child Profile.
/// Native staging precedes child catalog construction and shares the same Runtime.
pub async fn start_application(
    composition: impl Into<ApplicationComposition>,
    arguments: Vec<OsString>,
    program: ProfileProgram,
) -> Result<RunningHost> {
    let composition = composition.into();
    #[cfg(unix)]
    {
        native::start(composition, arguments, program).await
    }
    #[cfg(not(unix))]
    {
        let (host, diagnostics) = crate::standard_application_host(composition, arguments)?;
        host.start_program(program).await.map_err(|error| {
            diagnostics
                .take()
                .unwrap_or_else(|| RsiError::Boot(error.to_string()))
        })
    }
}

#[cfg(unix)]
mod native {
    use super::{ApplicationComposition, OsString, ProfileProgram, Result, RsiError, RunningHost};
    use crate::native_addons::{NativeStaging, NativeStagingContract};
    use async_trait::async_trait;
    use rsi_application::{ApplicationRunContract, ScopedProfile};
    use rsi_host::{HostBuilder, Profile, ProfileEntry};
    use rsi_meta::{
        ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation, UpdateMode,
    };
    use std::sync::Arc;

    #[derive(Debug)]
    struct Bootstrap {
        composition: ApplicationComposition,
        arguments: Vec<OsString>,
        program: ProfileProgram,
        diagnostic: Arc<std::sync::Mutex<Option<RsiError>>>,
    }

    pub(super) async fn start(
        composition: ApplicationComposition,
        arguments: Vec<OsString>,
        program: ProfileProgram,
    ) -> Result<RunningHost> {
        // Linked argument validators run before opening even the staging directories.
        // Complete native-aware preflight still occurs when mounting the child Profile.
        let (preflight, diagnostics) =
            crate::standard_application_host(composition.clone(), arguments.clone())?;
        preflight
            .profile_input(program.clone())
            .map_err(boot)?
            .preflight_linked(&crate::native_addons::bootstrap::deferred(
                &composition.service,
                crate::AddonScope::Application,
            )?)
            .map_err(|error| diagnostics.take().unwrap_or_else(|| boot(error)))?;
        let diagnostic = Arc::new(std::sync::Mutex::new(None));
        let mut builder = HostBuilder::new(composition.service.paths().clone());
        builder
            .register_local_contract::<ApplicationRunContract>()
            .map_err(boot)?;
        builder
            .register_local_contract::<crate::NativeAddonControlContract>()
            .map_err(boot)?;
        builder
            .register_linked(
                "rsi.application.bootstrap",
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                Arc::new(Bootstrap {
                    composition,
                    arguments,
                    program,
                    diagnostic: diagnostic.clone(),
                }),
            )
            .map_err(boot)?;
        builder
            .build()
            .map_err(boot)?
            .start(Profile::new(vec![ProfileEntry::new(
                "bootstrap",
                "rsi.application.bootstrap",
                ConfigValue::Null,
            )]))
            .await
            .map_err(|error| {
                diagnostic
                    .lock()
                    .expect("bootstrap diagnostic poisoned")
                    .take()
                    .unwrap_or_else(|| boot(error))
            })
    }

    #[async_trait]
    impl PluginFactory for Bootstrap {
        fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
            if !config.is_null() {
                return Err(activation("bootstrap configuration must be null"));
            }
            Ok(PreparedActivation::new(ConfigValue::Null))
        }
        async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
            let result = self.activate_inner(plan).await;
            if let Err(error) = &result {
                let mut message = error.to_string();
                let mut end = message.len().min(4096);
                while !message.is_char_boundary(end) {
                    end -= 1;
                }
                message.truncate(end);
                self.diagnostic
                    .lock()
                    .expect("bootstrap diagnostic poisoned")
                    .get_or_insert_with(|| boot(message));
            }
            result
        }
    }
    impl Bootstrap {
        async fn activate_inner(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
            let native = crate::native_addons::bootstrap::stage(
                &self.composition.service,
                &mut plan,
                false,
                self.composition.reserved_plugins(),
            )
            .await?;
            let staging = native
                .lookup_local::<NativeStagingContract>()
                .ok_or_else(|| activation("native staging did not publish its source"))?;
            let mut composition = self.composition.clone();
            composition.service = composition
                .service
                .with_published_native_staging(staging.as_ref().clone())
                .map_err(activation)?;
            let (host, diagnostics) =
                crate::standard_application_host(composition, self.arguments.clone())
                    .map_err(activation)?;
            let source = Arc::new(ApplicationCatalog {
                composition: self.composition.clone(),
                arguments: self.arguments.clone(),
                staging: staging.as_ref().clone(),
            });
            plan.context()
                .provide_local::<rsi_application::ProfileCatalogContract>(source.clone())?;
            let mut child = ScopedProfile::start(&host, plan.context(), self.program.clone())
                .await
                .map_err(|error| {
                    let diagnostic = diagnostics.take().unwrap_or_else(|| boot(error));
                    let failure = activation(&diagnostic);
                    *self
                        .diagnostic
                        .lock()
                        .expect("bootstrap diagnostic poisoned") = Some(diagnostic);
                    failure
                })?;
            child
                .follow_catalog(source, self.program.clone())
                .map_err(activation)?;
            let child = Arc::new(child);
            let cleanup = child.clone();
            plan.defer(
                "close Application child Profile",
                Box::new(move || {
                    Box::pin(async move {
                        let result = cleanup.shutdown().await;
                        if result.is_clean() {
                            Ok(())
                        } else {
                            Err("Application Profile cleanup failed".into())
                        }
                    })
                }),
            )?;
            let entry = child
                .lookup_local::<ApplicationRunContract>()
                .ok_or_else(|| activation("Application Profile did not publish an entry point"))?;
            plan.context()
                .provide_local::<ApplicationRunContract>(entry)?;
            plan.context()
                .provide_local::<crate::NativeAddonControlContract>(staging.control.clone())?;
            Ok(())
        }
    }

    #[derive(Debug)]
    struct ApplicationCatalog {
        composition: ApplicationComposition,
        arguments: Vec<OsString>,
        staging: NativeStaging,
    }
    impl rsi_application::ProfileCatalogSource for ApplicationCatalog {
        fn snapshot(&self) -> rsi_host::Result<Arc<rsi_host::Host>> {
            let mut composition = self.composition.clone();
            composition.service = composition
                .service
                .with_native_staging(self.staging.clone())
                .map_err(|error| rsi_host::HostError::Bootstrap(error.to_string()))?;
            let (host, _) = crate::standard_application_host(composition, self.arguments.clone())
                .map_err(|error| rsi_host::HostError::Bootstrap(error.to_string()))?;
            Ok(Arc::new(host))
        }
        fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
            self.staging.manager.changes()
        }
    }
    fn activation(error: impl std::fmt::Display) -> MetaError {
        MetaError::Activation(error.to_string())
    }
    fn boot(error: impl std::fmt::Display) -> RsiError {
        RsiError::Boot(error.to_string())
    }
}
