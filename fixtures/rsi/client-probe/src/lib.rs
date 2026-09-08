#![deny(unsafe_code)]

#[path = "../../../../crates/rsi/client/tests/support/mod.rs"]
mod scenarios;
#[path = "../../../../crates/rsi/application/tests/support/mod.rs"]
mod shells;

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub async fn run_probe() -> Result<String, JsValue> {
    let header = r#"{"format_version":11,"session_id":"browser-header","created_at_ms":1788778132168,"canonical_cwd":"/workspace","workspace_trust":"untrusted","agent_preset_id":"standard","settings":{"settings_id":"standard","system_prompt":"You are a careful coding agent.","default_model":{"deployment":"fixture","model":"fixture-model"},"sandbox":"workspace-write","require_approval":false,"turn_budget":{"maximum_elapsed_ms":1800000,"maximum_provider_attempts":64,"maximum_tool_calls":256,"maximum_generated_records":65536,"maximum_generated_record_bytes":67108864}},"fork_origin":null}"#;
    let decoded: rsi_agent_session_protocol::SessionHeader = serde_json::from_str(header)
        .map_err(|error| JsValue::from_str(&format!("durable header decode: {error}")))?;
    assert_eq!(decoded.created_at_ms(), 1_788_778_132_168);
    for path in ["/workspace", r"C:\workspace", r"\\server\share\workspace", r"\\?\C:\workspace"] {
        let mut value: serde_json::Value = serde_json::from_str(header).unwrap();
        value["canonical_cwd"] = path.into();
        serde_json::from_value::<rsi_agent_session_protocol::SessionHeader>(value).unwrap();
        let request = rsi_approval_protocol::ApprovalRequest {
            subject: rsi_approval_protocol::ApprovalSubject::new("session", "turn", "effect").unwrap(),
            id: "review".into(), action: "Run a command".into(), reason: "Prepared fixture".into(),
            review: Some(rsi_approval_protocol::ApprovalReview {
                arguments: serde_json::json!({"command":"printf fixture"}), cwd: path.into(),
                sandbox: "workspace-write".into(), request_sha256: "0".repeat(64),
            }),
        };
        serde_json::from_slice::<rsi_approval_protocol::ApprovalRequest>(&serde_json::to_vec(&request).unwrap()).unwrap();
        let policy = rsi_tools_protocol::ToolExecutionPolicy {
            mode: rsi_sandbox::SandboxMode::WorkspaceWrite, cwd: path.into(), workspace: path.into(),
        };
        serde_json::from_slice::<rsi_tools_protocol::ToolExecutionPolicy>(&serde_json::to_vec(&policy).unwrap()).unwrap();
        let stamp = rsi_sandbox::EnforcementStamp {
            requested: rsi_sandbox::SandboxMode::WorkspaceWrite,
            backend: rsi_sandbox::SandboxBackend::Bubblewrap { sha256:"0".repeat(64) }, workspace: path.into(),
            filesystem: rsi_sandbox::SandboxFileSystem::WorkspaceWrite, scratch: rsi_sandbox::SandboxScratch::PrivateTmp,
            network: rsi_sandbox::SandboxNetwork::Host,
        };
        serde_json::from_slice::<rsi_sandbox::EnforcementStamp>(&serde_json::to_vec(&stamp).unwrap()).unwrap();
    }
    let execution =
        rsi_meta::Execution::browser().map_err(|error| JsValue::from_str(&error.to_string()))?;
    scenarios::isolated_controller_scopes(execution.clone()).await;
    scenarios::owned_submission_drain(execution.clone()).await;
    scenarios::exact_command_reconciliation(execution.clone()).await;
    scenarios::explicit_reconciliation_cancellation(execution.clone()).await;
    scenarios::acknowledged_cursor(execution.clone()).await;
    scenarios::message_claim_cancellation_and_terminal_delivery(execution.clone()).await;
    scenarios::bounded_read_capacity_recovery_and_cancellation(execution.clone()).await;
    shells::session_free_shell_owns_bounded_isolated_profiles_with_shared_domain(execution.clone())
        .await;
    shells::retirement_cancels_partial_surface_activation_and_drains_its_owner(execution.clone())
        .await;
    shells::abandoned_surface_cleanup_failure_cannot_be_reported_as_clean_shell_shutdown(execution)
        .await;
    let resources = rsi_meta_execution::browser_resource_snapshot();
    assert_eq!(resources.pending_timers, 0);
    assert_eq!(resources.active_alarms, 0);
    Ok(serde_json::json!({
        "status":"passed", "command_reconciliation":"passed", "pending_timers":resources.pending_timers,
        "active_alarms":resources.active_alarms,
        "cases":["command identity and query-only reconciliation", "durable header decoding", "message claim cancellation and terminal delivery", "isolated controller scopes with shared domain", "owned submission drain and bounded admission", "explicit reconciliation cancellation and retained identity", "acknowledged cursor without watermark skip", "bounded read capacity recovery and cancellation", "Session-free Shell profiles and dropped opens", "partial surface activation retirement", "child cleanup failure propagation"]
    }).to_string())
}
