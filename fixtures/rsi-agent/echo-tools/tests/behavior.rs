use std::time::Duration;

use rsi_agent_fixture_echo_tools::rsi_meta_plugin_entry_v0;
use rsi_agent_protocol::{
    TOOLS_SERVICE_KEY, ToolResult, ToolsBody, ToolsEnvelope, ToolsInvokeRequest,
};
use rsi_meta_plugin::{
    CallOutcome, EVENT_CREDIT, EVENT_END, Frame, FrameBody, Lane, LifecyclePhase, OP_CREDIT,
    OP_HALF_CLOSE, OP_OPEN,
};
use rsi_meta_plugin_testkit::PluginHarness;
use serde_json::json;

fn committed_tools() -> PluginHarness {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).expect("start tools");
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Prepare, 1, None),
            )
            .expect("prepare callback"),
        CallOutcome::Ok
    );
    assert!(matches!(
        plugin
            .recv(Duration::from_secs(1))
            .expect("prepared")
            .frame
            .body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            ..
        }
    ));
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 1, None),
            )
            .expect("commit callback"),
        CallOutcome::Ok
    );
    plugin
}

fn open_tools(plugin: &mut PluginHarness, stream_id: &str) {
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_request(
                    stream_id,
                    TOOLS_SERVICE_KEY,
                    OP_OPEN,
                    json!({"consumer": "agent-capability-anchor", "sequence": 0}),
                ),
            )
            .expect("open callback"),
        CallOutcome::Ok
    );
    assert!(matches!(
        plugin
            .recv(Duration::from_secs(1))
            .expect("input credit")
            .frame
            .body,
        FrameBody::ServiceEvent { event, .. } if event == EVENT_CREDIT
    ));
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_request(
                    stream_id,
                    TOOLS_SERVICE_KEY,
                    OP_CREDIT,
                    json!({"bytes": 1024 * 1024}),
                ),
            )
            .expect("credit callback"),
        CallOutcome::Ok
    );
}

fn invoke_request() -> ToolsEnvelope {
    ToolsEnvelope::invoke_request(
        "invoke-1",
        ToolsInvokeRequest {
            call_id: "echo-call-1".to_owned(),
            name: "echo".to_owned(),
            arguments: r#"{"text":"hello"}"#.to_owned(),
        },
    )
}

fn send_request(
    plugin: &mut PluginHarness,
    stream_id: &str,
    request: &ToolsEnvelope,
) -> ToolsEnvelope {
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_data_request(
                    stream_id,
                    TOOLS_SERVICE_KEY,
                    request.encode().expect("request wire"),
                ),
            )
            .expect("request callback"),
        CallOutcome::Ok
    );
    let FrameBody::ServiceDataEvent { payload, .. } = plugin
        .recv(Duration::from_secs(1))
        .expect("tools response")
        .frame
        .body
    else {
        panic!("expected tools DATA response")
    };
    ToolsEnvelope::decode(&payload).expect("valid tools response")
}

#[test]
fn tools_provider_catalogs_two_streams_and_invokes_echo_on_its_origin_stream() {
    let mut plugin = committed_tools();
    open_tools(&mut plugin, "echo-tools-stream");
    open_tools(&mut plugin, "direct-tools-stream");

    let catalog = send_request(
        &mut plugin,
        "echo-tools-stream",
        &ToolsEnvelope::catalog_request("catalog-1"),
    );
    let ToolsBody::CatalogResponse(catalog) = catalog.body else {
        panic!("expected catalog response")
    };
    assert_eq!(catalog.tools.len(), 1);
    assert_eq!(catalog.tools[0].name, "echo");

    let direct_catalog = send_request(
        &mut plugin,
        "direct-tools-stream",
        &ToolsEnvelope::catalog_request("catalog-2"),
    );
    assert!(matches!(direct_catalog.body, ToolsBody::CatalogResponse(_)));

    let response = send_request(&mut plugin, "echo-tools-stream", &invoke_request());
    let ToolsBody::InvokeResponse(response) = response.body else {
        panic!("expected invoke response")
    };
    assert_eq!(response.call_id, "echo-call-1");
    assert_eq!(
        response.result,
        ToolResult::Ok {
            value: json!({"text": "hello"})
        }
    );

    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_request(
                    "echo-tools-stream",
                    TOOLS_SERVICE_KEY,
                    OP_HALF_CLOSE,
                    json!({"sequence": 3}),
                ),
            )
            .expect("half-close callback"),
        CallOutcome::Ok
    );
    assert!(matches!(
        plugin
            .recv(Duration::from_secs(1))
            .expect("stream end")
            .frame
            .body,
        FrameBody::ServiceEvent { event, .. } if event == EVENT_END
    ));
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_request(
                    "direct-tools-stream",
                    TOOLS_SERVICE_KEY,
                    OP_HALF_CLOSE,
                    json!({"sequence": 2}),
                ),
            )
            .expect("direct half-close callback"),
        CallOutcome::Ok
    );
    assert!(matches!(
        plugin
            .recv(Duration::from_secs(1))
            .expect("direct stream end")
            .frame
            .body,
        FrameBody::ServiceEvent { event, .. } if event == EVENT_END
    ));
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_request(
                    "unexpected-replay-stream",
                    TOOLS_SERVICE_KEY,
                    OP_OPEN,
                    json!({"consumer": "agent-capability-anchor", "sequence": 0}),
                ),
            )
            .expect("third open callback"),
        CallOutcome::Failed,
        "two-session conformance provider must witness zero service opens during replay"
    );
}

#[test]
fn invocation_before_catalog_is_rejected_without_consuming_the_fixture_call() {
    let mut plugin = committed_tools();
    open_tools(&mut plugin, "tools-stream");

    let response = send_request(&mut plugin, "tools-stream", &invoke_request());
    let ToolsBody::Error { error } = response.body else {
        panic!("invoke before catalog must be a service error")
    };
    assert_eq!(error.code, "catalog_required");

    let catalog = send_request(
        &mut plugin,
        "tools-stream",
        &ToolsEnvelope::catalog_request("catalog-1"),
    );
    assert!(matches!(catalog.body, ToolsBody::CatalogResponse(_)));
    let response = send_request(&mut plugin, "tools-stream", &invoke_request());
    assert!(matches!(response.body, ToolsBody::InvokeResponse(_)));
}
