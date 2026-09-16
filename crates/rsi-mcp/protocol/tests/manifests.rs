use rsi_mcp_protocol::*;
use serde_json::json;
fn server(id: &str) -> ServerManifest {
    ServerManifest {
        id: id.into(),
        target_sha256: "a".repeat(64),
        protocol_version: PROTOCOL_VERSIONS[0].into(),
        server_info: json!({"name":"fixture","version":"1"}),
        capabilities: json!({"tools":{}}),
        instructions: None,
        tools: vec![],
        resources: vec![],
    }
}
fn tool(server: &str, name: &str) -> FrozenTool {
    FrozenTool { definition: serde_json::from_value(json!({"name":name,"inputSchema":{"type":"object","properties":{"count":{"const":18_446_744_073_709_551_615_u64}}},"outputSchema":{"type":"object"},"annotations":{"readOnlyHint":true},"_meta":{"title":"完整 metadata"}})).unwrap(), selected: true, public_name: public_tool_name(server,name) }
}
#[test]
fn saved_manifest_round_trips_complete_metadata_and_exact_numbers() {
    let mut server = server("fixture");
    server.tools.push(tool("fixture", "tool 中文"));
    let manifest = McpManifest {
        servers: vec![server],
    };
    let snapshot = manifest.snapshot().unwrap();
    let decoded: McpManifest = serde_json::from_value(snapshot.state().value().clone()).unwrap();
    assert_eq!(manifest, decoded);
    decoded.validate().unwrap();
    assert!(
        snapshot
            .state()
            .value()
            .to_string()
            .contains("18446744073709551615")
    );
    assert!(decoded.servers[0].tools[0].public_name.len() <= 64);
    assert_ne!(
        public_tool_name("fixture", "tool 中文"),
        public_tool_name("fixture", "tool____")
    );
}
#[test]
fn bounds_reject_complete_oversize_and_cross_server_identity_collisions() {
    let mut server = server("fixture");
    server.tools = (0..65)
        .map(|i| tool("fixture", &format!("tool{i}")))
        .collect();
    assert!(
        McpManifest {
            servers: vec![server.clone()]
        }
        .validate()
        .is_err()
    );
    server.tools.truncate(64);
    for tool in &mut server.tools {
        tool.definition
            .extensions
            .insert("large".into(), "x".repeat(5000).into());
    }
    assert!(
        McpManifest {
            servers: vec![server]
        }
        .snapshot()
        .is_err()
    );
    let mut one = self::server("a");
    one.tools.push(tool("a", "b__c"));
    let mut two = self::server("a__b");
    two.tools.push(tool("a__b", "c"));
    assert!(
        McpManifest {
            servers: vec![one, two]
        }
        .validate()
        .is_err(),
        "an ambiguous public name must never route to a different raw identity"
    );
}
#[test]
fn invalid_schema_and_tampered_public_identity_do_not_restore() {
    let mut server = server("fixture");
    server.tools.push(tool("fixture", "echo"));
    server.tools[0].definition.input_schema = false.into();
    assert!(server.validate().is_err());
    server.tools[0] = tool("fixture", "echo");
    server.tools[0].public_name = "other".into();
    assert!(server.validate().is_err());
}
#[test]
fn remote_replacements_cannot_create_change_disable_or_remove_stdio_configuration() {
    let local: McpConfig = serde_json::from_value(json!({"servers":[{"id":"local","enabled":true,"tools":["echo"],"transport":{"kind":"stdio","program":"/usr/bin/python3","cwd":"/tmp","arguments":[],"environment":{}}}]})).unwrap();
    local.validate().unwrap();
    assert!(local.remote_replacement_of(&local));
    assert!(!local.remote_replacement_of(&McpConfig::default()));
    assert!(!McpConfig::default().remote_replacement_of(&local));
    let mut changed = local.clone();
    changed.servers[0].enabled = false;
    assert!(!changed.remote_replacement_of(&local));
    changed.servers[0].transport = TransportConfig::StreamableHttp {
        url: "http://localhost:8080/mcp".into(),
        credential: None,
    };
    assert!(!changed.remote_replacement_of(&local));
}
#[test]
fn endpoints_and_credentials_have_explicit_owners_and_no_ambient_resolution() {
    for endpoint in [
        "file:///tmp/mcp",
        "http://secret@localhost/",
        "http://localhost/#fragment",
    ] {
        let config = McpConfig {
            servers: vec![ServerConfig {
                id: "fixture".into(),
                enabled: true,
                tools: vec![],
                transport: TransportConfig::StreamableHttp {
                    url: endpoint.into(),
                    credential: None,
                },
            }],
        };
        assert!(config.validate().is_err());
    }
    let config: McpConfig = serde_json::from_value(json!({"servers":[{"id":"fixture","transport":{"kind":"stdio","program":"python3","cwd":"/tmp"}}]})).unwrap();
    assert!(config.validate().is_err());
    let config: McpConfig = serde_json::from_value(json!({"servers":[{"id":"fixture","transport":{"kind":"streamable_http","url":"https://example.com/mcp","credential":{"owner":"another-owner","slot":"token"}}}]})).unwrap();
    assert!(config.validate().is_err());
}

#[test]
fn bearer_endpoints_require_tls_outside_explicit_loopback() {
    for (url, allowed) in [
        ("https://example.com/mcp", true),
        ("http://localhost:8080/mcp", true),
        ("http://127.0.0.1/mcp", true),
        ("http://[::1]/mcp", true),
        ("http://example.com/mcp", false),
        ("http://localhost.evil.test/mcp", false),
        ("http://169.254.169.254/mcp", false),
        ("http://192.168.1.2/mcp", false),
        ("http://[::ffff:169.254.169.254]/mcp", false),
    ] {
        let config: McpConfig = serde_json::from_value(json!({"servers":[{
            "id":"fixture", "transport":{"kind":"streamable_http", "url":url,
            "credential":{"owner":"rsi.mcp", "slot":"token"}}
        }]}))
        .unwrap();
        assert_eq!(config.validate().is_ok(), allowed, "{url}");
    }
}

#[test]
fn selected_tools_leave_room_for_the_mcp_resource_reader() {
    let mut server = server("capacity");
    server.tools = (0..MAXIMUM_TOOLS)
        .map(|i| tool("capacity", &format!("tool{i}")))
        .collect();
    let mut manifest = McpManifest {
        servers: vec![server],
    };
    manifest.validate().unwrap();
    manifest.servers[0].instructions = Some("External instructions".into());
    assert!(
        manifest.validate().is_err(),
        "the resource reader occupies the shared Tool budget"
    );
    manifest.servers[0].tools[0].selected = false;
    manifest.validate().unwrap();
}
