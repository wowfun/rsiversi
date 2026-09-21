use super::*;

fn catalog_server(id: &str, tools: usize, resources: usize) -> ServerManifest {
    ServerManifest {
        id: id.into(),
        target_sha256: "a".repeat(64),
        protocol_version: rsi_mcp_protocol::LATEST_PROTOCOL_VERSION.into(),
        server_info: json!({"escaped":"\"\\\n界", "exact":18_446_744_073_709_551_615_u64}),
        capabilities: json!({}),
        instructions: None,
        tools: (0..tools)
            .map(|index| {
                let name = format!("tool{index}");
                rsi_mcp_protocol::FrozenTool {
                    public_name: rsi_mcp_protocol::public_tool_name(id, &name),
                    definition: serde_json::from_value(
                        json!({"name":name,"inputSchema":{"type":"object"}}),
                    )
                    .unwrap(),
                    selected: true,
                }
            })
            .collect(),
        resources: (0..resources)
            .map(|index| {
                serde_json::from_value(json!({"uri":format!("resource:{index}"),"name":"resource"}))
                    .unwrap()
            })
            .collect(),
    }
}

fn check_catalog(servers: Vec<ServerManifest>, accepted: bool) {
    let manifest = McpManifest { servers };
    let frozen: Vec<_> = manifest
        .servers
        .iter()
        .cloned()
        .map(|server| Arc::new(FrozenServer::new(server).unwrap()))
        .collect();
    let result = validate_frozen_servers(&frozen);
    assert_eq!(manifest.validate().is_ok(), accepted);
    assert_eq!(result.is_ok(), accepted);
    if let Ok(bytes) = result {
        assert_eq!(bytes, serde_json::to_vec(&manifest).unwrap().len());
    }
}

#[test]
fn frozen_and_decoded_catalogs_share_order_count_and_registration_limits() {
    check_catalog(vec![], true);
    for count in [8, 9] {
        check_catalog(
            (0..count)
                .map(|index| catalog_server(&format!("server{index}"), 0, 0))
                .collect(),
            count == 8,
        );
    }
    for ids in [["a", "b"], ["b", "a"], ["a", "a"]] {
        check_catalog(
            ids.map(|id| catalog_server(id, 0, 0)).into(),
            ids == ["a", "b"],
        );
    }
    check_catalog(
        vec![catalog_server("a", 32, 0), catalog_server("b", 32, 0)],
        true,
    );
    check_catalog(
        vec![catalog_server("a", 33, 0), catalog_server("b", 32, 0)],
        false,
    );
    check_catalog(
        vec![catalog_server("a", 0, 128), catalog_server("b", 0, 128)],
        true,
    );
    check_catalog(
        vec![catalog_server("a", 0, 128), catalog_server("b", 0, 129)],
        false,
    );
    for tools in [63, 64] {
        let mut reader = catalog_server("b", 0, 0);
        reader.instructions = Some("attributed instructions".into());
        check_catalog(vec![catalog_server("a", tools, 0), reader], tools == 63);
    }
    let mut unselected = catalog_server("a", 64, 1);
    for tool in &mut unselected.tools {
        tool.selected = false;
    }
    check_catalog(vec![unselected], true);
}

#[test]
fn frozen_manifest_size_matches_encoding_at_the_exact_domain_boundary() {
    let servers = vec![catalog_server("a", 1, 1), catalog_server("b", 1, 0)];
    let mut manifest = McpManifest { servers };
    manifest.servers[0].server_info["padding"] = "".into();
    let padding = rsi_agent_session_protocol::MAXIMUM_DOMAIN_STATE_BYTES
        - serde_json::to_vec(&manifest).unwrap().len();
    for extra in [-1isize, 0, 1] {
        manifest.servers[0].server_info["padding"] = "x"
            .repeat(padding.checked_add_signed(extra).unwrap())
            .into();
        check_catalog(manifest.servers.clone(), extra <= 0);
    }
}

#[derive(Debug)]
struct Unused;
#[async_trait::async_trait]
impl CredentialsResolve for Unused {
    async fn resolve(
        &self,
        _: &rsi_credentials_protocol::CredentialRef,
    ) -> rsi_credentials_protocol::Result<rsi_credentials_protocol::ResolvedCredential> {
        unreachable!()
    }
}
#[async_trait::async_trait]
impl Sandbox for Unused {
    async fn confine(
        &self,
        _: rsi_sandbox::ProcessRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
        unreachable!()
    }
    async fn workspace_read(
        &self,
        _: rsi_sandbox::WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
        unreachable!()
    }
}
impl DuplexProcess for Unused {
    fn spawn(
        &self,
        _: rsi_process::DuplexProcessSpec,
    ) -> rsi_process::Result<rsi_process::ManagedDuplexProcess> {
        unreachable!()
    }
}
#[tokio::test(start_paused = true)]
async fn configuration_wait_is_bounded_but_retirement_keeps_admission_until_settled() {
    let service = McpService::new(Arc::new(Unused), Arc::new(Unused), Arc::new(Unused));
    let entry = Arc::new(Entry::new(
        ServerConfig {
            id: "held".into(),
            enabled: true,
            tools: vec![],
            transport: TransportConfig::StreamableHttp {
                url: "http://127.0.0.1/".into(),
                credential: None,
            },
        },
        false,
        Arc::new(AtomicBool::new(false)),
    ));
    let held = entry.refresh.clone().acquire_owned().await.unwrap();
    service
        .entries
        .write()
        .unwrap()
        .insert("held".into(), entry);
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(31),
        service.configure(McpConfig::default()),
    )
    .await;
    assert_eq!(outcome, Ok(Err(McpError::Timeout)));
    assert!(
        service.status().is_empty(),
        "replacement has already applied"
    );
    assert_eq!(
        service.configure(McpConfig::default()).await,
        Err(McpError::Busy)
    );
    drop(held);
    tokio::time::timeout(std::time::Duration::from_secs(1), service.shutdown())
        .await
        .unwrap()
        .unwrap();
}
