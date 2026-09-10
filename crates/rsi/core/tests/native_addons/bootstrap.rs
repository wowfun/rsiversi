use super::*;
use async_trait::async_trait;
use rsi_application::{ApplicationRun, ApplicationRunContract};
use rsi_host::{Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, UpdateMode};

#[derive(Debug)]
struct Entry;
#[async_trait]
impl PluginFactory for Entry {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let result = PreparedActivation::new(config.clone());
        Ok(
            if config
                .get("session")
                .and_then(ConfigValue::as_bool)
                .unwrap_or(false)
            {
                result.requiring_local::<rsi_session_protocol::SessionContract>()
            } else {
                result
            },
        )
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<ApplicationRunContract>(Arc::new(Entry))?;
        Ok(())
    }
}
impl ApplicationRun for Entry {
    fn run(
        self: Arc<Self>,
    ) -> futures_util::future::BoxFuture<'static, rsi_application::Result<u8>> {
        Box::pin(async { Ok(37) })
    }
}

#[tokio::test]
async fn application_bootstrap_reuses_only_unlocked_cache_slots() {
    let temp = tempfile::tempdir().unwrap();
    let (composition, _) = application_fixture(temp.path(), "application");
    let program = ProfileProgram::from_profile(Profile::new(vec![ProfileEntry::new(
        "entry",
        "fixture.application.entry",
        ConfigValue::Null,
    )]));
    let first = rsi::start_application(composition.clone(), vec![], program.clone())
        .await
        .unwrap();
    let second = rsi::start_application(composition.clone(), vec![], program.clone())
        .await
        .unwrap();
    let caches = temp.path().join("cache/native-applications");
    assert_eq!(fs::read_dir(&caches).unwrap().count(), 2);
    assert!(first.shutdown().await.is_clean());
    for _ in 0..3 {
        let next = rsi::start_application(composition.clone(), vec![], program.clone())
            .await
            .unwrap();
        assert_eq!(
            fs::read_dir(&caches).unwrap().count(),
            2,
            "reuse the retired owner without sharing the live owner's cache"
        );
        assert!(next.shutdown().await.is_clean());
    }
    assert!(second.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn product_bootstrap_stages_native_application_and_embedded_service_with_one_loader() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let [artifact, replacement] = generations::artifacts(&root);
    for scope in ["application", "service"] {
        let root = root.join(scope);
        fs::create_dir_all(&root).unwrap();
        let manifest = source(&root.join("source"), "fixture.native-addon");
        let text = fs::read_to_string(&manifest)
            .unwrap()
            .replace("scope = 'agent'", &format!("scope = '{scope}'"));
        fs::write(&manifest, text).unwrap();
        fs::copy(&artifact, root.join("source/artifact.bin")).unwrap();
        let store = NativeAddonStore::open(root.join("config/native-addons")).unwrap();
        let initial = store.install(&manifest).unwrap().record.unwrap();
        store.enable("fixture.addon").unwrap();
        let (composition, program) = application_fixture(&root, scope);
        let owner_paths =
            rsi_service_host::ServiceHostPaths::from_host_paths(composition.paths()).unwrap();
        let running = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            rsi::start_application(composition, vec![], program),
        )
        .await
        .expect("bootstrap starts")
        .unwrap();
        let control = running
            .lookup_local::<rsi::NativeAddonControlContract>()
            .unwrap();
        assert_eq!(control.inspect().staged.len(), 1);
        assert!(control.inspect().active_instances > 0);
        assert_eq!(
            running
                .lookup_local::<ApplicationRunContract>()
                .unwrap()
                .run()
                .await
                .unwrap(),
            37
        );
        assert_eq!(
            fs::read_dir(root.join("cache/native-applications"))
                .unwrap()
                .count(),
            1
        );
        assert!(
            !root.join("cache/native-addons").exists(),
            "Service must reuse bootstrap's loader"
        );
        if scope == "service" {
            assert!(rsi_service_host::HostOwnerLease::try_acquire(owner_paths.clone()).is_err());
        } else {
            assert!(!owner_paths.owner_lock().exists());
        }
        fs::copy(&replacement, root.join("source/artifact.bin")).unwrap();
        let next = store.install(&manifest).unwrap().record.unwrap();
        store.enable_exact(&next, Some(&initial)).unwrap();
        control.refresh().await.unwrap();
        wait_application_factory(&running, &next, scope).await;
        let cleanup = tokio::time::timeout(std::time::Duration::from_secs(20), running.shutdown())
            .await
            .expect("bootstrap shutdown drains");
        assert!(cleanup.is_clean(), "{cleanup:?}");
        assert_eq!(control.inspect().staging_bytes, 0);
        if scope == "service" {
            drop(rsi_service_host::HostOwnerLease::try_acquire(owner_paths).unwrap());
        }
    }
}

#[tokio::test]
async fn linked_argument_preflight_precedes_native_store_creation() {
    let temp = tempfile::tempdir().unwrap();
    let composition = composition(temp.path());
    let paths = composition.paths().clone();
    let profile = Profile::new(vec![ProfileEntry::new(
        "entry",
        "rsi.application.devices",
        ConfigValue::Null,
    )]);
    assert!(
        rsi::start_application(
            composition,
            vec!["register".into(), "".into()],
            ProfileProgram::from_profile(profile)
        )
        .await
        .is_err()
    );
    assert!(!paths.config().exists());
    assert!(!paths.state().exists());
    assert!(!paths.cache().exists());
}

#[tokio::test]
async fn invalid_linked_service_preflight_creates_no_product_directories() {
    let temp = tempfile::tempdir().unwrap();
    let composition = composition(temp.path());
    let paths = composition.paths().clone();
    let profile = temp.path().join("invalid.profile.toml");
    fs::write(&profile, "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'invalid'\nplugin = 'rsi.session'\nconfig = { invalid = true }\n").unwrap();
    assert!(rsi::RunningRsi::boot(composition, &profile).await.is_err());
    assert!(!paths.config().exists());
    assert!(!paths.state().exists());
    assert!(!paths.cache().exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standalone_service_stages_before_resolving_and_follows_native_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let [a, b] = generations::artifacts(&root);
    let manifest = source(&root.join("source"), "fixture.native-addon");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest)
            .unwrap()
            .replace("scope = 'agent'", "scope = 'service'"),
    )
    .unwrap();
    fs::copy(a, root.join("source/artifact.bin")).unwrap();
    let store = NativeAddonStore::open(root.join("config/native-addons")).unwrap();
    let initial = store.install(&manifest).unwrap().record.unwrap();
    store.enable_exact(&initial, None).unwrap();
    fs::write(
        root.join("config/settings.json"),
        r#"{"rsi.agent":{"default_model":{"deployment":"fixture","model":"unused"}}}"#,
    )
    .unwrap();
    let profile = root.join("service.profile.toml");
    fs::write(&profile, "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'native'\nplugin = 'fixture.native-addon'\nconfig = { label = 'standalone' }\n").unwrap();
    let paths =
        rsi_service_host::ServiceHostPaths::from_host_paths(composition(&root).paths()).unwrap();
    let running = rsi::RunningRsi::boot(composition(&root), &profile)
        .await
        .unwrap();
    assert!(root.join("cache/native-addons").is_dir());
    assert!(!root.join("cache/native-applications").exists());
    assert!(rsi_service_host::HostOwnerLease::try_acquire(paths.clone()).is_err());
    fs::copy(b, root.join("source/artifact.bin")).unwrap();
    let next = store.install(&manifest).unwrap().record.unwrap();
    store.enable_exact(&next, Some(&initial)).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let mut after = None;
            loop {
                let page = running
                    .inspect(rsi_meta::InspectionRequest {
                        after,
                        maximum_fibers: 64,
                        ..Default::default()
                    })
                    .unwrap()
                    .runtime;
                if page.fibers.iter().any(|fiber| {
                    fiber.factory
                        == rsi_meta::FactoryIdentity::native(next.plugin(), next.artifact_sha256())
                        && fiber.state == rsi_meta::InspectedFiberState::Active
                }) {
                    return;
                }
                after = page.next_after;
                if after.is_none() {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Service follower activates replacement");
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(20), running.shutdown())
            .await
            .unwrap()
            .is_clean()
    );
    drop(rsi_service_host::HostOwnerLease::try_acquire(paths).unwrap());
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_application_follows_native_client_without_acquiring_local_service_owner() {
    remote_client_replacement(false).await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_application_follows_native_client_replacement() {
    remote_client_replacement(true).await;
}

#[cfg(target_os = "linux")]
async fn remote_client_replacement(uds: bool) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let [a, b] = generations::artifacts(&root);
    let (server, address, endpoint, token) = remote_server(&root.join("server")).await;

    let root = root.join("client");
    let manifest = source(&root.join("source"), "fixture.native-addon");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest)
            .unwrap()
            .replace("scope = 'agent'", "scope = 'client'"),
    )
    .unwrap();
    fs::copy(a, root.join("source/artifact.bin")).unwrap();
    let store = NativeAddonStore::open(root.join("config/native-addons")).unwrap();
    let initial = store.install(&manifest).unwrap().record.unwrap();
    store.enable_exact(&initial, None).unwrap();
    let composition = composition(&root)
        .with_addons(client_addons())
        .with_credential_store(Arc::new(FixtureSecret(token)));
    let owner = rsi_service_host::ServiceHostPaths::from_host_paths(composition.paths()).unwrap();
    let mut entries = vec![
        ProfileEntry::new(
            "credentials",
            "rsi.credentials.local",
            serde_json::json!({"service":"fixture"}),
        ),
        ProfileEntry::new(
            "connection",
            "rsi.application.http",
            serde_json::json!({
                "origin":format!("http://{address}"), "endpoint_id":endpoint,
                "credential":{"owner":"fixture","slot":"device"}, "allow_loopback_http":true,
            }),
        ),
        ProfileEntry::new(
            "entry",
            "fixture.application.entry",
            serde_json::json!({"session":true}),
        ),
    ];
    let stop = tokio_util::sync::CancellationToken::new();
    let daemon = if uds {
        let daemon = start_client_daemon(&composition, &root, &owner).await;
        entries.remove(0);
        entries[0] = ProfileEntry::new(
            "connection",
            "rsi.application.connection",
            serde_json::json!({"host_profile":"standard"}),
        );
        Some(tokio::spawn(daemon.run(stop.clone())))
    } else {
        None
    };
    let program = ProfileProgram::from_profile(Profile::new(entries));
    let running = rsi::start_application(composition, vec![], program)
        .await
        .unwrap();
    assert_eq!(owner.owner_lock().exists(), uds);
    fs::copy(b, root.join("source/artifact.bin")).unwrap();
    let next = store.install(&manifest).unwrap().record.unwrap();
    store.enable_exact(&next, Some(&initial)).unwrap();
    let control = running
        .lookup_local::<rsi::NativeAddonControlContract>()
        .unwrap();
    control.refresh().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if running.runtime_snapshot().fibers.iter().any(|fiber| {
                fiber.factory
                    == rsi_meta::FactoryIdentity::native(next.plugin(), next.artifact_sha256())
                    && fiber.state == rsi_meta::FiberState::Active
            }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Client follower activates native replacement");
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(20), running.shutdown())
            .await
            .unwrap()
            .is_clean()
    );
    assert_eq!(control.inspect().staging_bytes, 0);
    assert_eq!(owner.owner_lock().exists(), uds);
    stop.cancel();
    if let Some(daemon) = daemon {
        daemon.await.unwrap().unwrap();
    }
    assert!(server.shutdown().await.is_clean());
}

#[cfg(target_os = "linux")]
async fn start_client_daemon(
    composition: &StandardComposition,
    root: &Path,
    owner: &rsi_service_host::ServiceHostPaths,
) -> rsi::StandardServiceDaemon {
    fs::write(
        root.join("config/settings.json"),
        r#"{"rsi.agent":{"default_model":{"deployment":"fixture","model":"unused"}}}"#,
    )
    .unwrap();
    let profile = rsi::ProfileCatalog::new(composition.paths().clone())
        .host(&rsi::HostProfileId::new("standard").unwrap())
        .unwrap();
    let lease = rsi_service_host::HostOwnerLease::try_acquire(owner.clone()).unwrap();
    rsi::StandardServiceDaemon::start(composition.clone(), &profile, lease)
        .await
        .unwrap()
}

#[cfg(target_os = "linux")]
fn client_addons() -> rsi::StandardAddonSet {
    let mut addon = rsi::StandardAddonBuilder::new("fixture.client");
    addon
        .register_factory(
            rsi::AddonScope::Application,
            "fixture.application.entry",
            "1",
            UpdateMode::RestartRequired,
            Arc::new(Entry),
        )
        .unwrap();
    addon
        .register_fragment_at(
            rsi::AddonScope::Client,
            rsi_host::ProfileFragment::new(
                "fixture.client.native",
                [ProfileEntry::new(
                    "native",
                    "fixture.native-addon",
                    serde_json::json!({"label":"client"}),
                )],
            ),
        )
        .unwrap();
    rsi::StandardAddonSet::new([addon.build().unwrap()]).unwrap()
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct FixtureSecret(rsi_credentials_protocol::SecretValue);
#[cfg(target_os = "linux")]
impl rsi_credentials_local::SecretStore for FixtureSecret {
    fn get(
        &self,
        _: &str,
        _: &str,
    ) -> rsi_credentials_protocol::Result<Option<rsi_credentials_protocol::SecretValue>> {
        Ok(Some(self.0.clone()))
    }
    fn set(
        &self,
        _: &str,
        _: &str,
        _: &rsi_credentials_protocol::SecretValue,
    ) -> rsi_credentials_protocol::Result<()> {
        unreachable!("read-only fixture")
    }
    fn unset(&self, _: &str, _: &str) -> rsi_credentials_protocol::Result<bool> {
        unreachable!("read-only fixture")
    }
}

fn application_fixture(root: &Path, scope: &str) -> (StandardComposition, ProfileProgram) {
    let mut addon = rsi::StandardAddonBuilder::new("fixture.application");
    addon
        .register_factory(
            rsi::AddonScope::Application,
            "fixture.application.entry",
            "1",
            UpdateMode::RestartRequired,
            Arc::new(Entry),
        )
        .unwrap();
    let composition = composition(root)
        .with_addons(rsi::StandardAddonSet::new([addon.build().unwrap()]).unwrap());
    let mut entries = Vec::new();
    if scope == "application" {
        entries.push(ProfileEntry::new(
            "native",
            "fixture.native-addon",
            serde_json::json!({"label":"application"}),
        ));
    } else {
        fs::create_dir_all(root.join("config/host-profiles/dev")).unwrap();
        fs::write(root.join("config/host-profiles/dev/host.profile.toml"), "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'native'\nplugin = 'fixture.native-addon'\nconfig = { label = 'service' }\n").unwrap();
        fs::write(
            root.join("config/settings.json"),
            r#"{"rsi.agent":{"default_model":{"deployment":"fixture","model":"unused"}}}"#,
        )
        .unwrap();
        entries.push(ProfileEntry::new(
            "connection",
            "rsi.application.connection",
            serde_json::json!({"host_profile":"dev"}),
        ));
    }
    entries.push(ProfileEntry::new(
        "entry",
        "fixture.application.entry",
        serde_json::json!({"session":scope == "service"}),
    ));
    (
        composition,
        ProfileProgram::from_profile(Profile::new(entries)),
    )
}

#[cfg(target_os = "linux")]
async fn remote_server(
    server_root: &Path,
) -> (
    rsi_host::RunningHost,
    std::net::SocketAddr,
    rsi_api_protocol::EndpointId,
    rsi_credentials_protocol::SecretValue,
) {
    use rsi_api_protocol::{ConnectionDescriptionContract, DeviceAdministrationContract};
    fs::create_dir_all(server_root.join("config")).unwrap();
    fs::write(
        server_root.join("config/settings.json"),
        r#"{"rsi.agent":{"default_model":{"deployment":"fixture","model":"unused"}}}"#,
    )
    .unwrap();
    let server_composition = composition(server_root);
    let program = rsi::ProfileCatalog::new(server_composition.paths().clone())
        .application(&rsi::ApplicationProfileId::new("serve").unwrap())
        .unwrap()
        .program()
        .unwrap();
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reserve.local_addr().unwrap();
    drop(reserve);
    let (server, _) = rsi::standard_application_host(
        server_composition,
        vec![
            "--bind".into(),
            address.to_string().into(),
            "--origin".into(),
            format!("http://{address}").into(),
            "--dev-http".into(),
        ],
    )
    .unwrap();
    let server = server.start_program(program).await.unwrap();
    let address = server
        .lookup_local::<rsi_api_http::HttpListenerContract>()
        .unwrap()
        .address();
    let endpoint = server
        .lookup_local::<ConnectionDescriptionContract>()
        .unwrap()
        .endpoint_id
        .clone();
    let device = server
        .lookup_local::<DeviceAdministrationContract>()
        .unwrap()
        .register("Client fixture")
        .await
        .unwrap();

    (server, address, endpoint, device.token)
}

async fn wait_application_factory(
    running: &rsi_host::RunningHost,
    next: &rsi::NativeAddonRecord,
    scope: &str,
) {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let mut after = None;
            let mut active = false;
            loop {
                let inspection = running
                    .inspect(rsi_meta::InspectionRequest {
                        maximum_fibers: rsi_meta::MAXIMUM_INSPECTION_FIBERS,
                        after,
                        ..Default::default()
                    })
                    .unwrap();
                if inspection.fibers.iter().any(|fiber| {
                    fiber.factory
                        == rsi_meta::FactoryIdentity::native(next.plugin(), next.artifact_sha256())
                        && fiber.state == rsi_meta::InspectedFiberState::Active
                }) {
                    active = true;
                    break;
                }
                after = inspection.next_after;
                if after.is_none() {
                    break;
                }
            }
            if active {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{scope} did not activate the replacement factory"));
}

#[tokio::test]
async fn staged_service_rejects_invalid_linked_config_before_materializing_presets() {
    let temp = tempfile::tempdir().unwrap();
    let (composition, program) = application_fixture(temp.path(), "service");
    fs::write(temp.path().join("config/host-profiles/dev/host.profile.toml"),
        "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'invalid'\nplugin = 'rsi.session'\nconfig = { invalid = true }\n").unwrap();
    let result = rsi::start_application(composition, vec![], program).await;
    assert!(matches!(result, Err(rsi::RsiError::Boot(_))));
    assert!(
        !temp.path().join("cache/agent-presets").exists(),
        "invalid linked input must not materialize builtin preset assets"
    );
}
