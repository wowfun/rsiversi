use rsi::{DEFAULT_AGENT_PRESET_ID, StandardComposition};
use rsi_agent_composition_protocol::{AgentCompositionContract, AgentGenerationSeed};
use rsi_agent_session_protocol::AgentPresetId;
use rsi_host::{HostPaths, Profile};
use rsi_retrieval::{RetrievalContract, RetrievalError};
use rsi_settings_protocol::SettingsContract;
use rsi_tools_protocol::{ToolCall, ToolExecutionPolicy, ToolStart};
use serde_json::json;
use std::{collections::BTreeMap, path::Path};
use tokio_util::sync::CancellationToken;
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
    arguments: serde_json::Value,
) -> rsi_tools_protocol::ToolResult {
    pin.tools()
        .prepare(
            id,
            ToolCall {
                id: id.into(),
                name: name.into(),
                arguments,
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
#[tokio::test]
async fn real_catalog_defaults_off_freezes_flags_restores_offline_and_current_disable_returns_tool_error()
 {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let host = composition(&root)
        .build()
        .unwrap()
        .start(Profile::default())
        .await
        .unwrap();
    let resolver = host.lookup_local::<AgentCompositionContract>().unwrap();
    let preset = AgentPresetId::new(DEFAULT_AGENT_PRESET_ID).unwrap();
    let disabled = resolver.pin(&preset, None).await.unwrap();
    assert!(
        !disabled
            .tools()
            .definitions()
            .iter()
            .any(|tool| matches!(tool.name(), "web_fetch" | "web_search"))
    );
    let scope = host
        .lookup_local::<SettingsContract>()
        .unwrap()
        .scope("rsi.retrieval")
        .unwrap();
    let saved = scope
        .replace(0, json!({"web_fetch":true,"web_search":true}))
        .await
        .unwrap();
    let enabled = resolver.pin(&preset, None).await.unwrap();
    assert_ne!(disabled.source_digest(), enabled.source_digest());
    for name in ["web_fetch", "web_search"] {
        assert!(
            enabled
                .tools()
                .definitions()
                .iter()
                .any(|tool| tool.name() == name)
        );
    }
    let blocked = call(
        &enabled,
        &host,
        &root,
        "private",
        "web_fetch",
        json!({"url":"http://127.0.0.1/"}),
    )
    .await;
    assert!(blocked.is_error);
    assert_eq!(blocked.value["error"], "blocked_url");
    let missing = call(
        &enabled,
        &host,
        &root,
        "missing",
        "web_search",
        json!({"query":"fixture query"}),
    )
    .await;
    assert!(missing.is_error);
    assert_eq!(missing.value["error"], "missing_credential");
    let seed = AgentGenerationSeed::new(enabled.domains().baseline().to_vec()).unwrap();
    let definitions = enabled.tools().definitions();
    scope
        .replace(
            saved.version().revision,
            json!({"web_fetch":false,"web_search":false}),
        )
        .await
        .unwrap();
    let result = call(
        &enabled,
        &host,
        &root,
        "disabled",
        "web_fetch",
        json!({"url":"https://example.com"}),
    )
    .await;
    assert!(result.is_error);
    assert_eq!(result.value["error"], "disabled");
    drop((enabled, disabled, resolver, scope));
    assert!(host.shutdown().await.is_clean());
    let reopened = composition(&root)
        .build()
        .unwrap()
        .start(Profile::default())
        .await
        .unwrap();
    let resolver = reopened.lookup_local::<AgentCompositionContract>().unwrap();
    let restored = resolver.pin(&preset, Some(&seed)).await.unwrap();
    assert_eq!(restored.tools().definitions(), definitions);
    assert_eq!(restored.domains().baseline(), seed.states());
    drop((restored, resolver));
    assert!(reopened.shutdown().await.is_clean());
}
#[tokio::test]
#[ignore = "explicit RSI_LIVE_RETRIEVAL=1 permits public external HTTP/S requests"]
async fn live_public_retrieval_uses_actual_checked_dns_pinning_and_html_extraction() {
    assert_eq!(std::env::var("RSI_LIVE_RETRIEVAL").as_deref(), Ok("1"));
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
        .scope("rsi.retrieval")
        .unwrap()
        .replace(0, json!({"web_fetch":true,"web_search":false}))
        .await
        .unwrap();
    let service = host.lookup_local::<RetrievalContract>().unwrap();
    assert_eq!(
        service
            .fetch("http://127.0.0.1/".into(), CancellationToken::new())
            .await,
        Err(RetrievalError::BlockedUrl)
    );
    let result = service
        .fetch("https://example.com/".into(), CancellationToken::new())
        .await
        .unwrap();
    result.validate().unwrap();
    assert_eq!(result.sources[0].title, "Example Domain");
    assert!(result.sources[0].text.contains("Example Domain"));
    if let Ok(path) = std::env::var("RSI_LIVE_RETRIEVAL_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    }
    drop(service);
    assert!(host.shutdown().await.is_clean());
}
