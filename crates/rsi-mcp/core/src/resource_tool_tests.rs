use super::*;
#[path = "../tests/support/mod.rs"]
#[allow(dead_code)]
mod support;
use rsi_agent_session_protocol::{
    AgentPresetId, FrozenAgentSettings, SessionHeader, SessionId, TurnId,
};
use rsi_agent_turn_protocol::TurnClaimIssuer;
use rsi_tools_protocol::{ToolError, ToolExecutionExtensions, ToolExecutionPolicy, ToolStart};
use std::sync::atomic::Ordering;

#[derive(Debug)]
struct NoMedia;
#[async_trait]
impl Media for NoMedia {
    async fn import_image_with_options(
        &self,
        _: bytes::Bytes,
        _: rsi_media_protocol::ImageImportOptions,
    ) -> rsi_media_protocol::Result<rsi_media_protocol::MediaRef> {
        panic!("text needs no image import")
    }
    async fn read(
        &self,
        _: &rsi_media_protocol::MediaRef,
    ) -> rsi_media_protocol::Result<rsi_media_protocol::StoredMedia> {
        panic!("text needs no image read")
    }
}
fn execution(caller: bool) -> ToolExecution {
    let mut extensions = ToolExecutionExtensions::default();
    if caller {
        let issuer = TurnClaimIssuer::new();
        let session = SessionId::new("resource-test").unwrap();
        let settings = FrozenAgentSettings::new(
            "default",
            "system",
            serde_json::from_value(json!({"deployment":"test", "model":"test"})).unwrap(),
            rsi_sandbox::SandboxMode::ReadOnly,
            false,
        )
        .unwrap();
        let header = SessionHeader::new_local(
            session.clone(),
            1,
            "/workspace",
            AgentPresetId::new("test").unwrap(),
            settings,
        )
        .unwrap();
        let claim = issuer.issue(
            "test".into(),
            1,
            session,
            TurnId::new("turn").unwrap(),
            Arc::new(header),
            1,
            1,
            1,
        );
        extensions = extensions
            .with(Arc::new(issuer.agent_caller(&claim).unwrap()))
            .unwrap();
    }
    ToolExecution::from_start(
        "read".into(),
        ToolStart {
            cancellation: CancellationToken::new(),
            policy: ToolExecutionPolicy {
                mode: rsi_sandbox::SandboxMode::ReadOnly,
                cwd: "/workspace".into(),
                workspace: "/workspace".into(),
            },
            sandbox: Arc::new(support::TestSandbox),
            job_scope: None,
            extensions,
        },
    )
    .unwrap()
    .0
}
#[tokio::test]
async fn resource_tool_rejects_model_input_before_rpc_and_validates_resource_echo() {
    let fixture = support::HttpFixture::start(support::Mode {
        templates: Some(
            json!({"resourceTemplates":[{"uriTemplate":"fixture:{+path}","name":"Files"}]}),
        ),
        ..support::Mode::default()
    })
    .await;
    let service = Arc::new(fixture.service(Arc::new(support::Credentials::default())));
    let mut config = fixture.config();
    config.servers[0].resource_templates = true;
    service.configure(config).await.unwrap();
    let server = service
        .refresh("fixture", None, CancellationToken::new())
        .await
        .unwrap();
    let tool = ResourceTool(vec![Arc::new(Resources {
        service: service.clone(),
        server,
        media: Arc::new(NoMedia),
    })]);
    assert!(matches!(
        tool.execute(json!({"server":"fixture"}), execution(false))
            .await,
        Err(ToolError::InvalidInput(_))
    ));
    assert!(matches!(
        tool.execute(json!({"server":"fixture", "extra":true}), execution(true))
            .await,
        Err(ToolError::InvalidInput(_))
    ));
    let listed = tool
        .execute(json!({"server":"fixture"}), execution(true))
        .await
        .unwrap();
    assert!(!listed.is_error);
    assert_eq!(listed.value["resource"]["templates"][0]["id"], "template:0");
    let before = fixture.credentials_seen.load(Ordering::Acquire);
    for args in [
        json!({"server":"missing"}),
        json!({"server":"fixture", "parameters":{}}),
        json!({"server":"fixture", "id":"resource:0", "parameters":{}}),
        json!({"server":"fixture", "id":"resource:01"}),
        json!({"server":"fixture", "id":"template:00"}),
        json!({"server":"fixture", "id":"template:+0"}),
        json!({"server":"fixture", "id":"template:1"}),
        json!({"server":"fixture", "id":"template:0", "parameters":{"path":true}}),
        json!({"server":"fixture", "id":"template:0", "parameters":{"unknown":"x"}}),
    ] {
        let result = tool.execute(args.clone(), execution(true)).await.unwrap();
        assert!(result.is_error, "accepted {args}");
        result.validate().unwrap();
    }
    assert_eq!(
        fixture.credentials_seen.load(Ordering::Acquire),
        before,
        "invalid input must not reach server"
    );
    for args in [
        json!({"server":"fixture", "id":"resource:0"}),
        json!({"server":"fixture", "id":"template:0", "parameters":{"path":"/中文"}}),
    ] {
        let result = tool.execute(args.clone(), execution(true)).await.unwrap();
        assert!(!result.is_error, "{result:?}");
        result.validate().unwrap();
        assert_eq!(
            result.value["result"]["contents"][0]["uri"],
            result.value["resource"]["uri"]
        );
        for reply in [
            json!({"contents":[{"uri":"fixture:forged", "text":"untrusted"}]}),
            json!({"contents":[{"text":"missing URI"}]}),
            json!({"contents":[] , "padding":"x"}),
            json!({"contents":null}),
        ] {
            let empty_valid = reply["contents"] == json!([]);
            fixture.mode.lock().unwrap().resource_reply = Some(reply);
            let result = tool.execute(args.clone(), execution(true)).await.unwrap();
            assert_eq!(result.is_error, !empty_valid);
            result.validate().unwrap();
            if !empty_valid {
                assert_eq!(result.value["error"], json!(McpError::Protocol));
            }
        }
        fixture.mode.lock().unwrap().resource_reply = None;
    }
    service.shutdown().await.unwrap();
    fixture.shutdown().await;
}
