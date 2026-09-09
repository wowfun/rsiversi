use super::*;
use futures_util::{StreamExt as _, stream::FuturesUnordered};
use rsi::NativeAddonControlContract;
use std::time::Duration;

fn settings(root: &Path) {
    fs::create_dir_all(root.join("config")).unwrap();
    fs::write(
        root.join("config/settings.json"),
        br#"{"rsi.agent":{"default_model":{"deployment":"fixture","model":"unused"}}}"#,
    )
    .unwrap();
}

async fn wait_health(control: &dyn rsi::NativeAddonControl, health: NativeAddonHealth) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while control.inspect().health != health {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn standard_source_stages_before_pin_and_switches_real_native_code_only_after_enable() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let [a, b] = super::generations::artifacts(&root);
    let manifest = source(&root.join("source"), "fixture.native-addon");
    let text = fs::read_to_string(&manifest).unwrap();
    fs::write(
        &manifest,
        format!("{text}portable_services = ['fixture.native.tools']\n"),
    )
    .unwrap();
    fs::copy(a, root.join("source/artifact.bin")).unwrap();
    settings(&root);
    let preset = root.join("config/agent-presets/native");
    fs::create_dir_all(&preset).unwrap();
    fs::write(
        preset.join("agent.profile.toml"),
        super::generations::PROFILE,
    )
    .unwrap();
    let store = NativeAddonStore::open(root.join("config/native-addons")).unwrap();
    store.install(&manifest).unwrap();
    store.enable("fixture.addon").unwrap();
    let host = composition(&root)
        .build()
        .unwrap()
        .start(rsi_host::Profile::default())
        .await
        .unwrap();
    let control = host
        .lookup_local::<NativeAddonControlContract>()
        .unwrap_or_else(|| panic!("native control missing: {:?}", host.profile_status()));
    assert_eq!(control.inspect().health, NativeAddonHealth::Ready);
    let service = host
        .lookup_local::<rsi_agent_composition_protocol::AgentCompositionContract>()
        .unwrap();
    let id = rsi_agent_presets::AgentPresetId::new("native").unwrap();
    let old = service.pin(&id).await.unwrap();
    assert_eq!(
        old.tools().definitions()[0].description(),
        "Native fixture tool"
    );
    fs::copy(b, root.join("source/artifact.bin")).unwrap();
    store.install(&manifest).unwrap();
    assert!(!control.refresh().await.unwrap().changed);
    assert_eq!(
        old.source_digest(),
        service.pin(&id).await.unwrap().source_digest()
    );
    store.enable("fixture.addon").unwrap();
    wait_health(control.as_ref(), NativeAddonHealth::Ready).await;
    let new = service.pin(&id).await.unwrap();
    assert_ne!(old.source_digest(), new.source_digest());
    assert_eq!(
        new.tools().definitions()[0].description(),
        "Native fixture tool revision B"
    );
    assert_eq!(
        old.tools().definitions()[0].description(),
        "Native fixture tool"
    );
    drop((old, new, service));
    assert!(host.shutdown().await.is_clean());
    tokio::time::timeout(Duration::from_secs(5), async {
        while control.inspect().staging_bytes != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(control.inspect().retained_failed_finalizations, 0);
}

#[tokio::test]
async fn standard_host_owns_native_control_without_acquiring_it_during_preview() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let composition = composition(&root);
    composition.build_for_preview().unwrap();
    assert!(!root.join("config/native-addons").exists());
    assert!(!root.join("cache/native-addons").exists());
    settings(&root);
    let host = composition
        .build()
        .unwrap()
        .start(rsi_host::Profile::default())
        .await
        .unwrap();
    let control = host
        .lookup_local::<NativeAddonControlContract>()
        .expect("ordinary native control supply");
    assert_eq!(control.inspect().health, NativeAddonHealth::Ready);
    assert!(!control.refresh().await.unwrap().changed);
    assert!(root.join("config/native-addons").is_dir());
    assert!(root.join("cache/native-addons").is_dir());
    assert!(host.shutdown().await.is_clean());
    assert_eq!(control.inspect().health, NativeAddonHealth::Closed);
    assert!(matches!(
        control.refresh().await,
        Err(NativeAddonUpdateError::Closed)
    ));
    drop(control);
    // Frozen factory metadata must not retain the activation's live cache lease.
    let reopened =
        NativeCatalog::new(CatalogOptions::new(root.join("cache/native-addons"))).unwrap();
    drop(reopened);
    drop(host);
}

#[tokio::test]
async fn polling_does_not_reexecute_an_unchanged_failed_candidate_and_preview_keeps_its_launch_key()
{
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let artifact = super::races::blocking(&root);
    fs::write(root.join("release"), b"released").unwrap();
    let manifest = source(&root.join("source"), "fixture.deliberately-wrong-identity");
    fs::copy(artifact, root.join("source/artifact.bin")).unwrap();
    settings(&root);
    let composition = composition(&root);
    let profile = rsi::ProfileCatalog::new(composition.paths().clone())
        .host(&rsi::HostProfileId::new("standard").unwrap())
        .unwrap();
    let key = composition.preview_host(&profile).unwrap().launch_key;
    let host = composition
        .clone()
        .build()
        .unwrap()
        .start(rsi_host::Profile::default())
        .await
        .unwrap();
    let control = host.lookup_local::<NativeAddonControlContract>().unwrap();
    let store = NativeAddonStore::open(root.join("config/native-addons")).unwrap();
    store.install(&manifest).unwrap();
    store.enable("fixture.addon").unwrap();
    wait_health(control.as_ref(), NativeAddonHealth::Failed).await;
    assert_eq!(fs::read(root.join("entered")).unwrap(), b"entered");
    assert_eq!(composition.preview_host(&profile).unwrap().launch_key, key);
    tokio::time::sleep(Duration::from_millis(2100)).await;
    assert_eq!(fs::read(root.join("entered")).unwrap(), b"entered");
    assert_eq!(
        refresh_failure(&host).await,
        rsi_native_addons_api::RefreshFailure::Selection
    );
    assert_eq!(fs::read(root.join("entered")).unwrap(), b"enteredentered");
    let index = root.join("config/native-addons/state.json");
    let original = fs::read(&index).unwrap();
    fs::write(&index, b"invalid metadata").unwrap();
    wait_health(control.as_ref(), NativeAddonHealth::InvalidSource).await;
    assert_eq!(
        refresh_failure(&host).await,
        rsi_native_addons_api::RefreshFailure::Source
    );
    fs::write(&index, original).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(fs::read(root.join("entered")).unwrap(), b"enteredentered");
    assert_eq!(control.inspect().health, NativeAddonHealth::Failed);
    store.disable("fixture.addon").unwrap();
    wait_health(control.as_ref(), NativeAddonHealth::Ready).await;
    assert!(host.shutdown().await.is_clean());
}

struct Release(std::path::PathBuf);
impl Drop for Release {
    fn drop(&mut self) {
        fs::write(&self.0, b"release").unwrap();
    }
}

#[tokio::test]
async fn bounded_refresh_waiters_can_cancel_while_retirement_still_joins_the_native_callback() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let artifact = super::races::blocking(&root);
    let manifest = source(&root.join("source"), "fixture.retained-finalizer");
    fs::copy(artifact, root.join("source/artifact.bin")).unwrap();
    settings(&root);
    let host = composition(&root)
        .build()
        .unwrap()
        .start(rsi_host::Profile::default())
        .await
        .unwrap();
    let control = host.lookup_local::<NativeAddonControlContract>().unwrap();
    let store = NativeAddonStore::open(root.join("config/native-addons")).unwrap();
    store.install(&manifest).unwrap();
    store.enable("fixture.addon").unwrap();
    let release = Release(root.join("release"));
    let dispatch = host
        .lookup_local::<rsi_api_protocol::ApiDispatchContract>()
        .unwrap();
    let operation = rsi_api_protocol::OperationId::new("native-addons", "refresh", 1).unwrap();
    let call = dispatch
        .admit(&operation, rsi_api_protocol::CallOrigin::Local)
        .unwrap();
    let bytes = rsi_api_protocol::ByteBudget::new(1024)
        .unwrap()
        .encode(&serde_json::json!({}), 1024)
        .unwrap();
    let mut first = Box::pin(call.invoke(bytes));
    assert!(futures_util::poll!(first.as_mut()).is_pending());
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.join("entered").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut queued: FuturesUnordered<_> = (0..=rsi::MAXIMUM_NATIVE_ADDON_REFRESH_REQUESTS)
        .map(|_| control.refresh())
        .collect();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), queued.next())
            .await
            .unwrap(),
        Some(Err(NativeAddonUpdateError::Busy))
    ));
    drop((first, queued));
    let mut shutdown = Box::pin(host.shutdown());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                outcome = &mut shutdown => panic!("shutdown passed a running native callback: {outcome:?}"),
                () = tokio::time::sleep(Duration::from_millis(10)) => {
                    if control.inspect().health == NativeAddonHealth::Closed { break; }
                }
            }
        }
    }).await.unwrap();
    assert!(control.inspect().active_callbacks > 0);
    assert!(
        !dispatch
            .operations()
            .iter()
            .any(|value| value.id == operation)
    );
    assert!(futures_util::poll!(shutdown.as_mut()).is_pending());
    assert!(
        !can_claim_service_owner(&root).await,
        "Service Owner must outlive in-flight native retirement"
    );
    drop(release);
    assert!(shutdown.await.is_clean());
    assert!(can_claim_service_owner(&root).await);
    assert_eq!(control.inspect().health, NativeAddonHealth::Closed);
    assert!(control.inspect().staged.is_empty());
}

async fn can_claim_service_owner(root: &Path) -> bool {
    let paths =
        rsi_service_host::ServiceHostPaths::from_host_paths(composition(root).paths()).unwrap();
    let runtime = rsi_meta::Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            rsi_meta::ResolvedFactory::linked(
                "contender",
                "test",
                rsi_meta::UpdateMode::RestartRequired,
                Arc::new(rsi_service_host::ServiceOwnerFactory::acquiring(paths)),
            ),
            serde_json::Value::Null,
        )
        .await;
    let active =
        fiber.is_ok_and(|fiber| matches!(fiber.snapshot().state, rsi_meta::FiberState::Active));
    let _cleanup = runtime.shutdown().await;
    active
}

async fn refresh_failure(host: &rsi_host::RunningHost) -> rsi_native_addons_api::RefreshFailure {
    use rsi_api_protocol::{ApiDispatchContract, ApiError, ByteBudget, CallOrigin, OperationId};
    let dispatch = host.lookup_local::<ApiDispatchContract>().unwrap();
    let input = ByteBudget::new(1024)
        .unwrap()
        .encode(&serde_json::json!({}), 1024)
        .unwrap();
    let result = dispatch
        .admit(
            &OperationId::new("native-addons", "refresh", 1).unwrap(),
            CallOrigin::Local,
        )
        .unwrap()
        .invoke(input)
        .await;
    let Err(ApiError::Domain(bytes)) = result else {
        panic!("expected categorical refresh rejection: {result:?}")
    };
    serde_json::from_slice(bytes.as_bytes()).unwrap()
}
