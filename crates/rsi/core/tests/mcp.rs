#[path = "support/product.rs"]
mod product;
#[allow(dead_code)]
#[path = "../../../rsi-mcp/core/tests/support/mod.rs"]
mod support;
use async_trait::async_trait;
use rsi::{DEFAULT_AGENT_PRESET_ID, StandardComposition};
use rsi_agent_composition_protocol::{AgentCompositionContract, AgentGenerationSeed};
use rsi_agent_session_protocol::{AgentPresetId, DomainIdentity};
use rsi_api_protocol::*;
use rsi_configuration_api::{McpClient, McpOperation, McpRefreshRequest};
use rsi_host::{HostPaths, Profile};
use rsi_mcp::{MANIFEST_CODEC_VERSION, MANIFEST_DOMAIN, McpOwnerContract};
use rsi_settings_protocol::SettingsContract;
use rsi_tools_protocol::{ToolCall, ToolExecutionPolicy, ToolStart};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, atomic::Ordering},
};
use support::*;
use tokio_util::sync::CancellationToken;
#[derive(Debug)]
struct Client {
    dispatch: Arc<dyn ApiDispatch>,
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    origin: CallOrigin,
}
#[async_trait]
impl ApiClient for Client {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::new(1024 * 1024).unwrap()
    }
    async fn call(&self, operation: &OperationSpec, input: RetainedBytes) -> Result<ApiOutput> {
        self.dispatch
            .admit(&operation.id, self.origin.clone())?
            .invoke(input)
            .await
    }
}
fn composition(root: &Path) -> StandardComposition {
    StandardComposition::new(
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap(),
        BTreeMap::new(),
        None,
    )
}
async fn call(
    pin: &rsi_agent_composition_protocol::AgentCompositionPin,
    host: &rsi_host::RunningHost,
    root: &Path,
    id: &str,
    name: &str,
) -> rsi_tools_protocol::ToolResult {
    pin.tools()
        .prepare(
            id,
            ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: json!({"message":"from a sealed Agent catalog"}),
            },
        )
        .unwrap()
        .start(ToolStart {
            cancellation: CancellationToken::new(),
            policy: ToolExecutionPolicy {
                mode: rsi_sandbox::SandboxMode::ReadOnly,
                cwd: root.into(),
                workspace: root.into(),
            },
            sandbox: host.lookup_local::<rsi_sandbox::SandboxContract>().unwrap(),
            job_scope: None,
            extensions: rsi_tools_protocol::ToolExecutionExtensions::default(),
        })
        .await
        .unwrap()
}
fn assert_seed_reused(owner: &rsi_mcp::McpOwner) {
    let first_seed = owner.seed().unwrap();
    let second_seed = owner.seed().unwrap();
    assert!(
        std::ptr::eq(first_seed.states(), second_seed.states()),
        "unchanged readiness must reuse the immutable seed allocation"
    );
}

#[tokio::test]
async fn real_standard_composition_freezes_manifest_reports_drift_as_tool_result_and_restores_offline()
 {
    let fixture = HttpFixture::start(Mode::default()).await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let host = composition(&root)
        .build()
        .unwrap()
        .start(Profile::default())
        .await
        .unwrap();
    product::ready(&host).await;
    let resolver = host.lookup_local::<AgentCompositionContract>().unwrap();
    let preset = AgentPresetId::new(DEFAULT_AGENT_PRESET_ID).unwrap();
    let disabled = resolver.pin(&preset, None).await.unwrap();
    let identity = DomainIdentity::new(MANIFEST_DOMAIN, MANIFEST_CODEC_VERSION).unwrap();
    assert!(
        disabled
            .domains()
            .baseline()
            .iter()
            .any(|value| value.identity() == &identity)
    );
    assert!(
        disabled
            .tools()
            .definitions()
            .iter()
            .all(|tool| !tool.name().starts_with("mcp"))
    );
    let owner = host.lookup_local::<McpOwnerContract>().unwrap();
    let settings = host
        .lookup_local::<SettingsContract>()
        .unwrap()
        .scope("rsi.mcp")
        .unwrap();
    settings
        .replace(0, serde_json::to_value(fixture.config()).unwrap())
        .await
        .unwrap();
    let unavailable = resolver.pin(&preset, None).await.unwrap_err().to_string();
    assert!(
        unavailable.contains("MCP") && unavailable.contains("Plugins"),
        "{unavailable}"
    );
    owner.refresh(None, CancellationToken::new()).await.unwrap();
    assert_seed_reused(&owner);
    let frozen = resolver.pin(&preset, None).await.unwrap();
    let name = rsi_mcp::public_tool_name("fixture", "echo");
    assert!(
        frozen
            .tools()
            .definitions()
            .iter()
            .any(|tool| tool.name() == name)
    );
    assert!(
        frozen
            .tools()
            .definitions()
            .iter()
            .any(|tool| tool.name() == "mcp_resource_read")
    );
    let success = call(&frozen, &host, &root, "before-drift", &name).await;
    assert!(!success.is_error);
    assert_eq!(
        success.value["result"]["structuredContent"]["exact"].to_string(),
        "18446744073709551615"
    );
    let saved = AgentGenerationSeed::new(frozen.domains().baseline().to_vec()).unwrap();
    fixture.mode.lock().unwrap().changed = true;
    owner
        .refresh(Some("fixture"), CancellationToken::new())
        .await
        .unwrap();
    let stale = call(&frozen, &host, &root, "after-drift", &name).await;
    assert!(stale.is_error);
    assert_eq!(stale.value["error"], "catalog_changed");
    assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
    let current = resolver.pin(&preset, None).await.unwrap();
    assert_ne!(frozen.source_digest(), current.source_digest());
    let frozen_definitions = frozen.tools().definitions();
    settings.replace(1, json!({"servers":[]})).await.unwrap();
    owner.refresh(None, CancellationToken::new()).await.unwrap();
    drop((disabled, frozen, current, resolver, owner));
    assert!(host.shutdown().await.is_clean());
    fixture.shutdown().await;
    let reopened = composition(&root)
        .build()
        .unwrap()
        .start(Profile::default())
        .await
        .unwrap();
    product::ready(&reopened).await;
    let resolver = reopened.lookup_local::<AgentCompositionContract>().unwrap();
    let restored = resolver.pin(&preset, Some(&saved)).await.unwrap();
    assert_eq!(restored.tools().definitions(), frozen_definitions);
    assert_eq!(restored.domains().baseline(), saved.states());
    let unavailable = call(&restored, &reopened, &root, "offline-call", &name).await;
    assert!(unavailable.is_error);
    drop((restored, resolver));
    assert!(reopened.shutdown().await.is_clean());
}
#[tokio::test]
async fn real_grants_fence_status_and_hold_refresh_through_reply_loss_and_revocation() {
    let fixture = HttpFixture::start(Mode::default()).await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let host = composition(&root)
        .build()
        .unwrap()
        .start(Profile::default())
        .await
        .unwrap();
    host.lookup_local::<SettingsContract>()
        .unwrap()
        .scope("rsi.mcp")
        .unwrap()
        .replace(0, serde_json::to_value(fixture.config()).unwrap())
        .await
        .unwrap();
    let dispatch = host.lookup_local::<ApiDispatchContract>().unwrap();
    let registered = host
        .lookup_local::<DeviceAdministrationContract>()
        .unwrap()
        .register("mcp-fixture")
        .await
        .unwrap();
    let origin = CallOrigin::Device(AuthenticatedDevice {
        id: registered.record.id.clone(),
        revoked: CancellationToken::new(),
    });
    let client = Arc::new(Client {
        description: (*host
            .lookup_local::<ConnectionDescriptionContract>()
            .unwrap())
        .clone(),
        operations: dispatch.operations().clone(),
        dispatch,
        origin: origin.clone(),
    });
    let api = McpClient::new(client.clone()).unwrap();
    assert!(matches!(api.status().await, Err(ApiError::Unauthorized)));
    let grant = host
        .lookup_local::<rsi_configuration_access::ConfigurationAccessContract>()
        .unwrap();
    grant
        .set_grant(&CallOrigin::Local, registered.record.id.clone(), "0", true)
        .unwrap()
        .await
        .unwrap();
    assert!(api.status().await.unwrap().settings_pending);
    fixture.mode.lock().unwrap().wait_list = true;
    let request = McpRefreshRequest { server: None };
    let peer = api.clone();
    let refresh = tokio::spawn(async move { peer.refresh(&request).await });
    fixture.started.notified().await;
    refresh.abort();
    assert!(refresh.await.unwrap_err().is_cancelled());
    let revoke = grant
        .set_grant(&CallOrigin::Local, registered.record.id, "1", false)
        .unwrap();
    let revoke = tokio::spawn(revoke);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while grant.allowed(&origin) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!revoke.is_finished());
    fixture.release.notify_one();
    revoke.await.unwrap().unwrap();
    assert!(matches!(api.status().await, Err(ApiError::Unauthorized)));
    let raw = json!({"unknown":"field"});
    let input = client
        .input_budget(OperationClass::Data)
        .encode(&raw, 4096)
        .unwrap();
    assert!(
        client
            .call(&McpOperation::Status.spec(), input)
            .await
            .is_err()
    );
    drop((api, client, grant));
    assert!(host.shutdown().await.is_clean());
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_selection_obeys_the_actual_shared_tool_ceiling_without_partial_publication() {
    let fixture = HttpFixture::start(Mode {
        tool_count: Some(64),
        ..Mode::default()
    })
    .await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let host = composition(&root)
        .build()
        .unwrap()
        .start(Profile::default())
        .await
        .unwrap();
    product::ready(&host).await;
    let resolver = host.lookup_local::<AgentCompositionContract>().unwrap();
    let preset = AgentPresetId::new(DEFAULT_AGENT_PRESET_ID).unwrap();
    let before = resolver.pin(&preset, None).await.unwrap();
    let definitions = before.tools().definitions();
    let settings = host
        .lookup_local::<SettingsContract>()
        .unwrap()
        .scope("rsi.mcp")
        .unwrap();
    let mut config = fixture.config();
    config.servers[0].tools = (0..64)
        .map(|index| {
            if index == 0 {
                "echo".into()
            } else {
                format!("extra{index}")
            }
        })
        .collect();
    settings
        .replace(0, serde_json::to_value(&config).unwrap())
        .await
        .unwrap();
    let owner = host.lookup_local::<McpOwnerContract>().unwrap();
    assert_eq!(
        owner
            .refresh(None, CancellationToken::new())
            .await
            .unwrap_err(),
        rsi_mcp::McpError::Capacity
    );
    assert_eq!(
        owner.seed().unwrap_err(),
        rsi_mcp::McpError::Capacity,
        "64 selected Tools plus the resource reader are already impossible before Session creation"
    );
    assert!(!owner.observation().fresh_ready);
    assert!(
        resolver.pin(&preset, None).await.is_err(),
        "64 MCP Tools plus ordinary Session Tools must fail the actual shared registrar"
    );
    assert_eq!(
        before.tools().definitions(),
        definitions,
        "failure must not mutate an existing frozen catalog"
    );
    config.servers[0].tools = vec!["echo".into()];
    settings
        .replace(1, serde_json::to_value(&config).unwrap())
        .await
        .unwrap();
    owner.refresh(None, CancellationToken::new()).await.unwrap();
    let selected = resolver.pin(&preset, None).await.unwrap();
    assert_eq!(
        selected
            .tools()
            .definitions()
            .iter()
            .filter(|tool| tool.name().starts_with("mcp"))
            .count(),
        2
    );
    assert_eq!(
        owner.status()[0].tools.len(),
        64,
        "selection must not truncate the complete saved server manifest"
    );
    drop((before, selected, resolver, owner));
    assert!(host.shutdown().await.is_clean());
    fixture.shutdown().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One sequential public-seam scenario preserves causality and exact evidence"
)]
async fn remote_mcp_credentials_bind_current_http_reference_and_stdio_remains_observation_only() {
    use rsi_configuration_api::McpCredentialTarget;
    use rsi_credentials_protocol::{CredentialAvailability, CredentialRef, SecretValue};
    let fixture = HttpFixture::start(Mode::default()).await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let private = json!({"servers":[{"id":"private-stdio","enabled":false,"transport":{"kind":"stdio","program":"/private/stdio-program","cwd":"/private/stdio-cwd","arguments":["hidden-argument"],"environment":{}}}]});
    let profile = Profile::program([rsi_host::ProfileStep::Patch(
        rsi_host::ProfilePatch::ReplaceConfig {
            target: "rsi-mcp".into(),
            config: private,
        },
    )]);
    let host = composition(&root)
        .build()
        .unwrap()
        .start(profile)
        .await
        .unwrap();
    let registered = host
        .lookup_local::<DeviceAdministrationContract>()
        .unwrap()
        .register("mcp-credentials")
        .await
        .unwrap();
    let origin = CallOrigin::Device(AuthenticatedDevice {
        id: registered.record.id.clone(),
        revoked: CancellationToken::new(),
    });
    let grant = host
        .lookup_local::<rsi_configuration_access::ConfigurationAccessContract>()
        .unwrap();
    grant
        .set_grant(&CallOrigin::Local, registered.record.id, "0", true)
        .unwrap()
        .await
        .unwrap();
    let dispatch = host.lookup_local::<ApiDispatchContract>().unwrap();
    let raw = Arc::new(Client {
        description: (*host
            .lookup_local::<ConnectionDescriptionContract>()
            .unwrap())
        .clone(),
        operations: dispatch.operations().clone(),
        dispatch,
        origin,
    });
    let api = McpClient::new(raw.clone()).unwrap();
    let encoded = serde_json::to_string(&api.status().await.unwrap()).unwrap();
    for forbidden in ["/private/", "hidden-argument", "environment"] {
        assert!(!encoded.contains(forbidden));
    }
    assert!(matches!(
        api.refresh(&McpRefreshRequest {
            server: Some("private-stdio".into())
        })
        .await,
        Err(ApiError::Unauthorized)
    ));
    let mut config = fixture.config();
    let reference = CredentialRef::new("rsi.mcp", "http-token").unwrap();
    if let rsi_mcp::TransportConfig::StreamableHttp { credential, .. } =
        &mut config.servers[0].transport
    {
        *credential = Some(reference.clone());
    }
    let settings = host
        .lookup_local::<SettingsContract>()
        .unwrap()
        .scope("rsi.mcp")
        .unwrap();
    settings
        .replace(0, serde_json::to_value(&config).unwrap())
        .await
        .unwrap();
    let refreshed = api
        .refresh(&McpRefreshRequest { server: None })
        .await
        .unwrap();
    assert_eq!(
        refreshed.error,
        Some(rsi_mcp::McpError::CredentialUnavailable)
    );
    let target = McpCredentialTarget {
        server: "fixture".into(),
        reference: reference.clone(),
    };
    let status = api.credential_status(&target).await.unwrap();
    assert_eq!(status.availability, CredentialAvailability::Missing);
    assert!(status.editable);
    api.credential_set(&target, SecretValue::new("fixture-secret").unwrap())
        .await
        .unwrap();
    let status = api.credential_status(&target).await.unwrap();
    assert!(matches!(
        status.availability,
        CredentialAvailability::Configured { .. }
    ));
    let text = serde_json::to_string(&status).unwrap();
    assert!(!text.contains("fixture-secret"));
    assert!(!text.contains("store_path"));
    assert!(
        api.refresh(&McpRefreshRequest { server: None })
            .await
            .unwrap()
            .error
            .is_none()
    );
    assert!(fixture.credentials_seen.load(Ordering::Acquire) >= 5);
    let changed = rsi_configuration_api::McpCredentialTarget {
        server: "another-server".into(),
        reference: reference.clone(),
    };
    assert!(matches!(
        api.credential_unset(&changed).await,
        Err(ApiError::Unauthorized)
    ));
    if let rsi_mcp::TransportConfig::StreamableHttp { credential, .. } =
        &mut config.servers[0].transport
    {
        *credential = Some(CredentialRef::new("rsi.mcp", "replacement").unwrap());
    }
    settings
        .replace(1, serde_json::to_value(&config).unwrap())
        .await
        .unwrap();
    assert!(
        matches!(
            api.credential_unset(&target).await,
            Err(ApiError::Unauthorized)
        ),
        "a stale UI must not mutate another current binding"
    );
    let retained = host
        .lookup_local::<rsi_credentials_protocol::CredentialsResolveContract>()
        .unwrap()
        .resolve(&reference)
        .await
        .unwrap();
    assert_eq!(retained.secret.expose_secret(), "fixture-secret");
    drop((api, raw, grant));
    assert!(host.shutdown().await.is_clean());
    fixture.shutdown().await;
}

#[tokio::test]
async fn remote_exa_credential_uses_fixed_owner_and_grant_without_enabling_or_querying() {
    use rsi_configuration_api::ExaClient;
    use rsi_credentials_protocol::{
        CredentialAvailability, CredentialsResolveContract, SecretValue,
    };
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let host = composition(&root)
        .build()
        .unwrap()
        .start(Profile::default())
        .await
        .unwrap();
    let registered = host
        .lookup_local::<DeviceAdministrationContract>()
        .unwrap()
        .register("exa-credentials")
        .await
        .unwrap();
    let dispatch = host.lookup_local::<ApiDispatchContract>().unwrap();
    let raw = Arc::new(Client {
        description: (*host
            .lookup_local::<ConnectionDescriptionContract>()
            .unwrap())
        .clone(),
        operations: dispatch.operations().clone(),
        dispatch,
        origin: CallOrigin::Device(AuthenticatedDevice {
            id: registered.record.id.clone(),
            revoked: CancellationToken::new(),
        }),
    });
    let api = ExaClient::new(raw).unwrap();
    assert!(matches!(api.status().await, Err(ApiError::Unauthorized)));
    assert!(matches!(
        api.set(SecretValue::new("rejected-fixture-key").unwrap())
            .await,
        Err(ApiError::Unauthorized)
    ));
    let grant = host
        .lookup_local::<rsi_configuration_access::ConfigurationAccessContract>()
        .unwrap();
    grant
        .set_grant(&CallOrigin::Local, registered.record.id.clone(), "0", true)
        .unwrap()
        .await
        .unwrap();
    let status = api.status().await.unwrap();
    assert_eq!(status.availability, CredentialAvailability::Missing);
    assert!(status.editable);
    assert!(
        api.set(SecretValue::new("exa-fixture-secret").unwrap())
            .await
            .unwrap()
            .removed
            .is_none()
    );
    let status = api.status().await.unwrap();
    assert!(matches!(
        status.availability,
        CredentialAvailability::Configured { .. }
    ));
    let encoded = serde_json::to_string(&status).unwrap();
    assert!(!encoded.contains("exa-fixture-secret"));
    assert!(!encoded.contains("store_path"));
    let resolved = host
        .lookup_local::<CredentialsResolveContract>()
        .unwrap()
        .resolve(&rsi_retrieval::exa_credential())
        .await
        .unwrap();
    assert_eq!(resolved.secret.expose_secret(), "exa-fixture-secret");
    drop(resolved);
    assert_eq!(
        host.lookup_local::<rsi_retrieval::RetrievalContract>()
            .unwrap()
            .config()
            .unwrap(),
        rsi_retrieval::RetrievalConfig::default()
    );
    assert_eq!(api.unset().await.unwrap().removed, Some(true));
    assert_eq!(api.unset().await.unwrap().removed, Some(false));
    grant
        .set_grant(&CallOrigin::Local, registered.record.id, "1", false)
        .unwrap()
        .await
        .unwrap();
    assert!(matches!(api.unset().await, Err(ApiError::Unauthorized)));
    drop((api, grant));
    assert!(host.shutdown().await.is_clean());
}

#[tokio::test]
async fn saved_codec_one_cannot_restore_even_with_an_empty_mcp_manifest() {
    use rsi_agent_session_protocol::DomainSnapshot;
    let temporary = tempfile::tempdir().unwrap();
    let host = composition(temporary.path())
        .build()
        .unwrap()
        .start(Profile::default())
        .await
        .unwrap();
    product::ready(&host).await;
    let resolver = host.lookup_local::<AgentCompositionContract>().unwrap();
    let preset = AgentPresetId::new(DEFAULT_AGENT_PRESET_ID).unwrap();
    let current = resolver.pin(&preset, None).await.unwrap();
    let states = current
        .domains()
        .baseline()
        .iter()
        .map(|state| {
            if state.identity().id() == MANIFEST_DOMAIN {
                DomainSnapshot::new(
                    DomainIdentity::new(MANIFEST_DOMAIN, 1).unwrap(),
                    state.state().clone(),
                )
            } else {
                state.clone()
            }
        })
        .collect();
    let old = AgentGenerationSeed::new(states).unwrap();
    let error = resolver.pin(&preset, Some(&old)).await.unwrap_err();
    assert_eq!(
        error,
        rsi_agent_composition_protocol::AgentCompositionError::UnsupportedSeedCodec {
            stored: DomainIdentity::new(MANIFEST_DOMAIN, 1).unwrap(),
            expected: DomainIdentity::new(MANIFEST_DOMAIN, MANIFEST_CODEC_VERSION).unwrap(),
        }
    );
    let error = error.to_string();
    assert!(
        error.contains("unsupported saved Domain codec")
            && error.contains(MANIFEST_DOMAIN)
            && error.contains("start a new conversation"),
        "{error}"
    );
    assert!(
        resolver
            .pin(&preset, None)
            .await
            .unwrap()
            .same_generation(&current)
    );
    drop((current, resolver));
    assert!(host.shutdown().await.is_clean());
}
