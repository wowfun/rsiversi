use super::*;
use rsi_agent_session_protocol::AgentPresetId;
use rsi_api_protocol::{ApiError, ApiOutput, ByteBudget, CallOrigin, OperationSpec};
use rsi_configuration_api::agent_presets::{
    AgentPresetDocumentError, AgentPresetFormat, AgentPresetsClient, AgentPresetsOperation,
};
use serde_json::json;

async fn invoke(
    running: &RunningRsi,
    origin: CallOrigin,
    spec: OperationSpec,
    input: serde_json::Value,
) -> Result<ApiOutput, ApiError> {
    running
        .api_dispatch()
        .unwrap()
        .admit(&spec.call(), origin)?
        .invoke(ByteBudget::default().encode(&input, spec.maximum_request_bytes)?)
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preset_catalog_reads_observe_source_edits_and_distinguish_missing_from_broken() {
    let fixture = fixture("http://127.0.0.1:1");
    let daemon = DaemonFixture::new(&fixture).await;
    let client = AgentPresetsClient::new(daemon.connection.api_client());
    let id = AgentPresetId::new("read-test").unwrap();
    assert!(matches!(
        client.document(&id).await.unwrap(),
        Err(AgentPresetDocumentError::NotFound)
    ));
    let directory = fixture.paths.config().join("agent-presets/read-test");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("agent.profile.toml");
    std::fs::write(&path, "invalid TOML").unwrap();
    let row = client
        .list()
        .await
        .unwrap()
        .presets
        .into_iter()
        .find(|row| row.id == id)
        .unwrap();
    assert!(row.broken.is_some());
    assert!(matches!(
        client.document(&id).await.unwrap(),
        Err(AgentPresetDocumentError::Broken { .. })
    ));
    // Repair the same winning entry without restarting or refreshing a shadow catalog.
    let content = include_str!("../../presets/standard/agent.profile.toml");
    std::fs::write(&path, content).unwrap();
    std::fs::write(
        directory.join("preset.toml"),
        "name = '读取测试'\ndescription = 'Native preset source'\n",
    )
    .unwrap();
    let row = client
        .list()
        .await
        .unwrap()
        .presets
        .into_iter()
        .find(|row| row.id == id)
        .unwrap();
    assert!(row.broken.is_none());
    assert_eq!(row.name.as_deref(), Some("读取测试"));
    let document = client.document(&id).await.unwrap().unwrap();
    assert!(matches!(document.format, AgentPresetFormat::Toml));
    assert_eq!(document.content, content);
    assert_eq!(document.name, row.name);
    assert_eq!(document.description, row.description);
    std::fs::write(&path, format!("{content}\n# exact source edit\n")).unwrap();
    assert_eq!(
        client.document(&id).await.unwrap().unwrap().content,
        format!("{content}\n# exact source edit\n")
    );
    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preset_reads_require_the_actual_callers_configuration_grant() {
    let fixture = fixture("http://127.0.0.1:1");
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let device = running
        .device_administration()
        .unwrap()
        .register("ungranted")
        .await
        .unwrap();
    let origin = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&device.token)
            .unwrap(),
    );
    for (revision, granted) in [(None, false), (Some("0"), true), (Some("1"), false)] {
        if let Some(revision) = revision {
            invoke(
                &running,
                CallOrigin::Local,
                rsi_configuration_api::ConfigurationOperation::SetGrant.spec(),
                json!({"device":device.record.id,"expected_revision":revision,"granted":granted}),
            )
            .await
            .unwrap();
        }
        for (operation, input) in [
            (AgentPresetsOperation::List, json!({})),
            (AgentPresetsOperation::Document, json!({"id":"standard"})),
        ] {
            let result = invoke(&running, origin.clone(), operation.spec(), input).await;
            if granted {
                assert!(matches!(result, Ok(ApiOutput::Reply(_))), "{result:?}");
            } else {
                assert!(matches!(result, Err(ApiError::Unauthorized)), "{result:?}");
            }
        }
    }
    let spec = AgentPresetsOperation::Document.spec();
    for input in [
        json!({"id":"../standard"}),
        json!({"id":"standard", "path":"elsewhere"}),
    ] {
        let result = invoke(&running, CallOrigin::Local, spec.clone(), input).await;
        assert!(matches!(result, Err(ApiError::Invalid(_))), "{result:?}");
    }
    assert!(running.shutdown().await.is_clean());
}
