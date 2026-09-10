use async_trait::async_trait;
use rsi::{StandardAddonBuilder, StandardAddonSet, StandardComposition};
use rsi_api_protocol::*;
use rsi_host::{HostPaths, Profile, ProfileEntry};
use rsi_inspector::{InspectorClient, PageRequest, RuntimeRequest};
use rsi_meta::{ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, UpdateMode};
use rsi_meta_profile::{ProfileGroup, ProfileNode, ProfileStep};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug)]
struct Noop;
#[async_trait]
impl PluginFactory for Noop {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.defer(
            "secret-effect-label",
            Box::new(|| Box::pin(async { Ok(()) })),
        )
    }
}
#[derive(Debug)]
struct Local {
    dispatch: Arc<dyn ApiDispatch>,
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
}
#[async_trait]
impl ApiClient for Local {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::new(4096).unwrap()
    }
    async fn call(&self, operation: &OperationSpec, input: RetainedBytes) -> Result<ApiOutput> {
        self.dispatch
            .admit(&operation.id, CallOrigin::Local)?
            .invoke(input)
            .await
    }
}

#[tokio::test]
async fn actual_host_inspection_is_redacted_paginated_local_and_withdrawn() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config/settings.json"),
        br#"{"rsi.agent":{"default_model":{"deployment":"fixture","model":"unused"}}}"#,
    )
    .unwrap();
    let mut addon = StandardAddonBuilder::new("fixture.inspector");
    addon
        .register_linked(
            "fixture.inspect",
            "exact-revision",
            UpdateMode::Replayable,
            Arc::new(Noop),
        )
        .unwrap();
    let composition = StandardComposition::new(
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap(),
        BTreeMap::new(),
        None,
    )
    .with_addons(StandardAddonSet::new([addon.build().unwrap()]).unwrap());
    let host = composition
        .build()
        .unwrap()
        .start(Profile::program([
            ProfileStep::Node(ProfileNode::Plugin(ProfileEntry::new(
                "visible",
                "fixture.inspect",
                json!({"secret": "do-not-serialize-config"}),
            ))),
            ProfileStep::Node(ProfileNode::Group(
                ProfileGroup::new(
                    "disabled",
                    [ProfileNode::Plugin(ProfileEntry::new(
                        "disabled-child",
                        "fixture.inspect",
                        json!({"secret": "disabled-config"}),
                    ))],
                )
                .enabled(false),
            )),
        ]))
        .await
        .unwrap();
    let dispatch = host.lookup_local::<ApiDispatchContract>().unwrap();
    let local = Arc::new(Local {
        description: (*host
            .lookup_local::<ConnectionDescriptionContract>()
            .unwrap())
        .clone(),
        operations: dispatch.operations(),
        dispatch: dispatch.clone(),
    });
    let client = InspectorClient::new(local.clone()).unwrap();
    assert_runtime_pages(&client, &root).await;
    assert_declaration_pages(&client).await;
    #[cfg(unix)]
    {
        let native = client.native().await.unwrap();
        assert_eq!(native["health"], "ready");
        assert_eq!(native["staging_bytes"], "0");
    }
    #[cfg(not(unix))]
    assert!(matches!(client.native().await, Err(ApiError::Unavailable)));
    assert_local_access_and_requests(&local).await;
    drop(client);
    assert!(host.shutdown().await.is_clean());
    assert!(
        !dispatch
            .operations()
            .iter()
            .any(|value| value.id.domain() == "inspector")
    );
    let client = InspectorClient::new(local).unwrap();
    assert!(client.runtime(&RuntimeRequest::default()).await.is_err());
}

async fn assert_runtime_pages(client: &InspectorClient, root: &std::path::Path) {
    let mut after = None;
    let mut ids = std::collections::BTreeSet::new();
    let mut saw_effect = false;
    let mut saw_binding = false;
    loop {
        let page = client
            .runtime(&RuntimeRequest {
                after,
                maximum_fibers: 3,
                maximum_items: 2,
            })
            .await
            .unwrap();
        assert!(page["revision"].is_string());
        let rows = page["fibers"].as_array().unwrap();
        assert!(rows.len() <= 3);
        for row in rows {
            assert!(ids.insert(row["id"].as_str().unwrap().to_owned()));
            assert!(row["generation"].is_string());
            saw_effect |= !row["effects"]["items"].as_array().unwrap().is_empty();
            saw_binding |= row["dependencies"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["provider"].is_object());
        }
        let text = page.to_string();
        assert!(
            !text.contains("do-not-serialize-config")
                && !text.contains("secret-effect-label")
                && !text.contains(root.to_str().unwrap())
        );
        after = page["next_after"].as_str().map(str::to_owned);
        if after.is_none() {
            break;
        }
    }
    assert!(saw_effect && saw_binding);
}

async fn assert_declaration_pages(client: &InspectorClient) {
    let mut offset = 0;
    let mut disabled = false;
    let mut revision = None;
    loop {
        let page = client
            .profile(&PageRequest { offset, limit: 2 })
            .await
            .unwrap();
        if let Some(revision) = &revision {
            assert_eq!(revision, &page["revision"]);
        } else {
            revision = Some(page["revision"].clone());
        }
        for row in page["nodes"].as_array().unwrap() {
            disabled |= row["id"] == "disabled" && row["enabled"] == false;
        }
        assert!(!page.to_string().contains("disabled-config"));
        let Some(next) = page["next_offset"].as_u64() else {
            break;
        };
        offset = usize::try_from(next).unwrap();
    }
    assert!(disabled);
    let mut offset = 0;
    let mut factories = Vec::new();
    loop {
        let page = client
            .factories(&PageRequest { offset, limit: 4 })
            .await
            .unwrap();
        factories.extend(page["factories"].as_array().unwrap().iter().cloned());
        let Some(next) = page["next_offset"].as_u64() else {
            break;
        };
        offset = usize::try_from(next).unwrap();
    }
    assert!(
        factories
            .iter()
            .any(|row| row["identity"].to_string().contains("exact-revision"))
    );
    assert!(
        factories
            .iter()
            .any(|row| row["identity"].to_string().contains("rsi.inspector.api"))
    );
}

async fn assert_local_access_and_requests(local: &Local) {
    assert_eq!(
        local
            .operations
            .iter()
            .filter(|value| value.id.domain() == "inspector")
            .count(),
        4
    );
    for operation in local
        .operations
        .iter()
        .filter(|value| value.id.domain() == "inspector")
    {
        assert_eq!(operation.access, OperationAccess::Local);
        let remote = CallOrigin::Device(AuthenticatedDevice {
            id: DeviceId::from_bytes([7; 16]),
            revoked: tokio_util::sync::CancellationToken::new(),
        });
        assert!(local.dispatch.admit(&operation.id, remote).is_err());
        for invalid in [json!({"unknown": true}), json!({"maximum_items": 0})] {
            let bytes = ByteBudget::new(1024)
                .unwrap()
                .encode(&invalid, 1024)
                .unwrap();
            assert!(local.call(operation, bytes).await.is_err());
        }
    }
}
