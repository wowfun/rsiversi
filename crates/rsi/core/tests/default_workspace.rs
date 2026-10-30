use rsi::{ApplicationCatalogMetadata, StandardComposition};
use rsi_api_protocol::{ApiDispatchContract, ApiOutput, ByteBudget, CallOrigin};
use rsi_host::HostPaths;
use rsi_workspace_protocol::WorkspaceRegistryContract;
use std::collections::BTreeMap;

#[test]
fn reserved_application_factories_match_the_linked_catalog() {
    let temp = tempfile::tempdir_in(rsi_workspace_protocol::canonical_native_path(
        std::env::temp_dir().canonicalize().unwrap(),
    ))
    .unwrap();
    let root = temp.path();
    let composition = StandardComposition::new(
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap(),
        BTreeMap::new(),
        None,
        ApplicationCatalogMetadata::default(),
    );
    let catalog = rsi::base_application_catalog(composition).unwrap();
    let actual: std::collections::BTreeSet<_> = catalog
        .addons
        .descriptions()
        .map(|entry| entry.plugin.clone())
        .collect();
    let reserved = rsi::BASE_APPLICATION_PLUGINS
        .iter()
        .map(|id| (*id).to_owned())
        .collect();
    assert_eq!(actual, reserved);
}

async fn initialize(host: &rsi_host::RunningHost) -> serde_json::Value {
    let spec = rsi_workspace_default_api::operation();
    let dispatch = host.lookup_local::<ApiDispatchContract>().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !dispatch
            .operations()
            .unwrap()
            .iter()
            .any(|entry| entry.id == spec.id)
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let payload = ByteBudget::default()
        .encode(&serde_json::json!({}), spec.maximum_request_bytes)
        .unwrap();
    let output = dispatch
        .admit(&spec.call(), CallOrigin::Local)
        .unwrap()
        .invoke(payload)
        .await;
    let ApiOutput::Reply(reply) = (match output {
        Ok(value) => value,
        Err(rsi_api_protocol::ApiError::Domain(_)) => return serde_json::json!({"error":true}),
        Err(error) => panic!("{error:?}"),
    }) else {
        panic!("finite reply")
    };
    serde_json::from_slice(reply.json.as_bytes()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_free_initialization_is_idempotent_and_removal_survives_restart() {
    let temp = tempfile::tempdir_in(rsi_workspace_protocol::canonical_native_path(
        std::env::temp_dir().canonicalize().unwrap(),
    ))
    .unwrap();
    let root = temp.path();
    let profile = root.join("profile.toml");
    std::fs::write(&profile, "format = 1\nsteps = []\n").unwrap();
    let documents = root.join("Documents 世界");
    std::fs::create_dir(&documents).unwrap();
    let composition = StandardComposition::new(
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap(),
        BTreeMap::new(),
        None,
        ApplicationCatalogMetadata::default(),
    )
    .with_documents_directory(Some(documents.clone()))
    .unwrap();
    let host = composition
        .clone()
        .build()
        .unwrap()
        .start_file(&profile)
        .await
        .unwrap();
    let (one, two) = tokio::join!(initialize(&host), initialize(&host));
    assert_eq!(one, two);
    let record: rsi_workspace_protocol::WorkspaceRecord =
        serde_json::from_value(one.clone()).unwrap();
    assert_eq!(
        std::path::Path::new(record.coordinates.path()),
        documents.join("RSIversi/default-workspace")
    );
    assert!(documents.join("RSIversi/default-workspace").is_dir());
    let store = host
        .lookup_local::<rsi_agent_store_protocol::SessionStoreContract>()
        .unwrap();
    assert!(
        store
            .list_sessions(None, 1)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    let registry = host.lookup_local::<WorkspaceRegistryContract>().unwrap();
    assert_eq!(registry.list(None, 10).await.unwrap().records.len(), 1);
    registry.delete_registration(&record.id).await.unwrap();
    assert_eq!(initialize(&host).await, serde_json::Value::Null);
    assert!(host.shutdown().await.is_clean());
    drop(store);
    drop(registry);
    drop(host);
    let host = composition
        .build()
        .unwrap()
        .start_file(&profile)
        .await
        .unwrap();
    assert_eq!(initialize(&host).await, serde_json::Value::Null);
    assert!(documents.join("RSIversi/default-workspace").is_dir());
    assert!(host.shutdown().await.is_clean());
}

#[tokio::test]
async fn directory_failure_is_retryable_and_existing_workspace_suppresses_creation() {
    let temp = tempfile::tempdir_in(rsi_workspace_protocol::canonical_native_path(
        std::env::temp_dir().canonicalize().unwrap(),
    ))
    .unwrap();
    let root = temp.path();
    let profile = root.join("profile.toml");
    std::fs::write(&profile, "format = 1\nsteps = []\n").unwrap();
    let documents = root.join("Documents");
    let composition = StandardComposition::new(
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap(),
        BTreeMap::new(),
        None,
        ApplicationCatalogMetadata::default(),
    )
    .with_documents_directory(Some(documents.clone()))
    .unwrap();
    let host = composition
        .build()
        .unwrap()
        .start_file(&profile)
        .await
        .unwrap();
    assert_eq!(initialize(&host).await, serde_json::json!({"error":true}));
    assert!(
        !documents.exists(),
        "initialization must not create the Documents root"
    );
    std::fs::write(&documents, "blocked").unwrap();
    assert_eq!(initialize(&host).await, serde_json::json!({"error":true}));
    std::fs::remove_file(&documents).unwrap();
    let existing = root.join("existing");
    std::fs::create_dir(&existing).unwrap();
    host.lookup_local::<WorkspaceRegistryContract>()
        .unwrap()
        .get_or_create(&existing)
        .await
        .unwrap();
    assert_eq!(initialize(&host).await, serde_json::Value::Null);
    assert!(!documents.exists());
    assert!(host.shutdown().await.is_clean());
}

#[test]
fn invalid_documents_spelling_fails_before_initialization() {
    let root = tempfile::tempdir_in(rsi_workspace_protocol::canonical_native_path(
        std::env::temp_dir().canonicalize().unwrap(),
    ))
    .unwrap();
    let composition = StandardComposition::new(
        HostPaths::new(
            root.path().join("config"),
            root.path().join("state"),
            root.path().join("cache"),
        )
        .unwrap(),
        BTreeMap::new(),
        None,
        ApplicationCatalogMetadata::default(),
    );
    assert!(
        composition
            .clone()
            .with_documents_directory(Some("relative".into()))
            .is_err()
    );
    #[cfg(windows)]
    for path in [r"c:\Documents", r"C:\Users\.\Documents"] {
        assert!(
            composition
                .clone()
                .with_documents_directory(Some(path.into()))
                .is_err()
        );
    }
}
