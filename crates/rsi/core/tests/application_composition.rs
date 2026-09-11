use async_trait::async_trait;
use rsi::{
    AddonScope, ApplicationComposition, StandardAddonBuilder, StandardAddonSet, StandardComposition,
};
use rsi_application::{ApplicationRun, ApplicationRunContract};
use rsi_host::{HostPaths, Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, UpdateMode};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Debug)]
struct Entry {
    catalog: Arc<Mutex<Option<Arc<dyn rsi_application::ProfileCatalogSource>>>>,
}
#[async_trait]
impl PluginFactory for Entry {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<ApplicationRunContract>(Arc::new(Run {
                context: plan.context().clone(),
                catalog: self.catalog.clone(),
            }))?;
        Ok(())
    }
}
#[derive(Debug)]
struct Run {
    context: rsi_meta::Context,
    catalog: Arc<Mutex<Option<Arc<dyn rsi_application::ProfileCatalogSource>>>>,
}
impl ApplicationRun for Run {
    fn run(
        self: Arc<Self>,
    ) -> futures_util::future::BoxFuture<'static, rsi_application::Result<u8>> {
        Box::pin(async move {
            *self.catalog.lock().unwrap() = self
                .context
                .lookup_local::<rsi_application::ProfileCatalogContract>();
            Ok(37)
        })
    }
}
fn composition(root: &std::path::Path) -> StandardComposition {
    StandardComposition::new(
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap(),
        BTreeMap::new(),
        None,
    )
}
fn extras(scope: AddonScope, entry: Arc<Entry>) -> StandardAddonSet {
    let mut addon = StandardAddonBuilder::new("fixture.desktop");
    addon
        .register_factory(
            scope,
            "fixture.desktop.entry",
            "1",
            UpdateMode::RestartRequired,
            entry,
        )
        .unwrap();
    StandardAddonSet::new([addon.build().unwrap()]).unwrap()
}

#[tokio::test]
async fn application_extra_survives_bootstrap_and_catalog_refresh_without_changing_host_identity() {
    let temp = tempfile::tempdir().unwrap();
    let service = composition(temp.path());
    let profile = rsi::ProfileCatalog::new(service.paths().clone())
        .host(&rsi::HostProfileId::new("standard").unwrap())
        .unwrap();
    let before = service.preview_host(&profile).unwrap().launch_key;
    let catalog = Arc::new(Mutex::new(None));
    let app = ApplicationComposition::new(
        service,
        extras(
            AddonScope::Application,
            Arc::new(Entry {
                catalog: catalog.clone(),
            }),
        ),
    )
    .unwrap();
    assert_eq!(
        before,
        app.service().preview_host(&profile).unwrap().launch_key
    );
    let program = ProfileProgram::from_profile(Profile::new(vec![ProfileEntry::new(
        "entry",
        "fixture.desktop.entry",
        ConfigValue::Null,
    )]));
    let running = rsi::start_application(app, vec![], program.clone())
        .await
        .unwrap();
    assert_eq!(
        running
            .lookup_local::<ApplicationRunContract>()
            .unwrap()
            .run()
            .await
            .unwrap(),
        37
    );
    let source = catalog
        .lock()
        .unwrap()
        .clone()
        .expect("application source inherited");
    source
        .snapshot()
        .unwrap()
        .profile_input(program)
        .unwrap()
        .preflight_linked(&std::collections::BTreeSet::default())
        .unwrap();
    assert!(running.shutdown().await.is_clean());
}

#[test]
fn application_extras_reject_other_roles_before_any_state_is_created() {
    let temp = tempfile::tempdir().unwrap();
    for scope in [AddonScope::Service, AddonScope::Agent, AddonScope::Client] {
        let result = ApplicationComposition::new(
            composition(temp.path()),
            extras(
                scope,
                Arc::new(Entry {
                    catalog: Arc::new(Mutex::new(None)),
                }),
            ),
        );
        assert!(result.is_err(), "accepted {scope:?}");
    }
    assert!(!temp.path().join("state").exists());
    assert!(!temp.path().join("config").exists());
}
