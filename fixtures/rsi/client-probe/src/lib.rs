#![deny(unsafe_code)]

#[path = "../../../../crates/rsi/client/tests/support/mod.rs"]
mod scenarios;
#[path = "../../../../crates/rsi/application/tests/support/mod.rs"]
mod shells;
#[path = "../../../../crates/rsi/ui/tests/support/mod.rs"]
mod ui;

#[path = "../../../../crates/rsi/session-files/tests/support/mod.rs"]
mod files;

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = rsiProbeProgress)]
    fn progress(state: &str, detail: &str);
}

async fn scenario(
    name: &'static str,
    work: impl std::future::Future<Output = ()>,
    completed: &mut Vec<&'static str>,
) {
    progress("started", name);
    work.await;
    completed.push(name);
    progress("passed", name);
}

#[wasm_bindgen]
pub async fn run_probe() -> Result<String, JsValue> {
    std::panic::set_hook(Box::new(|info| {
        progress(
            "panic",
            &info.to_string().chars().take(4096).collect::<String>(),
        );
    }));
    let mut cases = Vec::new();
    progress("started", "durable header decoding");
    let settings = serde_json::from_value(serde_json::json!({"settings_id": "standard", "system_prompt": "You are a careful coding agent.", "default_model": {"deployment": "fixture", "model": "fixture-model"}, "sandbox": "workspace-write", "require_approval": false})).unwrap();
    let header = rsi_agent_session_protocol::SessionHeader::new_local(
        rsi_agent_session_protocol::SessionId::new("browser-header").unwrap(),
        1_788_778_132_168,
        "/workspace",
        rsi_agent_session_protocol::AgentPresetId::new("standard").unwrap(),
        settings,
    )
    .unwrap();
    let header = serde_json::to_string(&header).unwrap();
    let decoded: rsi_agent_session_protocol::SessionHeader = serde_json::from_str(&header)
        .map_err(|error| JsValue::from_str(&format!("durable header decode: {error}")))?;
    assert_eq!(decoded.created_at_ms(), 1_788_778_132_168);
    for path in [
        "/workspace",
        r"C:\workspace",
        r"\\server\share\workspace",
        r"\\?\C:\workspace",
    ] {
        let mut value: serde_json::Value = serde_json::from_str(&header).unwrap();
        value["coordinates"]["path"] = path.into();
        let decoded = serde_json::from_value::<rsi_agent_session_protocol::SessionHeader>(value)
            .map_err(|error| JsValue::from_str(&format!("foreign header path {path}: {error}")))?;
        assert_eq!(decoded.canonical_cwd(), path);
        let request = rsi_approval_protocol::ApprovalRequest {
            subject: rsi_approval_protocol::ApprovalSubject::new("session", "turn", "effect")
                .unwrap(),
            id: "review".into(),
            action: "Run a command".into(),
            reason: "Prepared fixture".into(),
            review: Some(rsi_approval_protocol::ApprovalReview {
                execution: None,
                arguments: serde_json::json!({"command":"printf fixture"}),
                cwd: path.into(),
                sandbox: "workspace-write".into(),
                request_sha256: "0".repeat(64),
            }),
        };
        serde_json::from_slice::<rsi_approval_protocol::ApprovalRequest>(
            &serde_json::to_vec(&request).unwrap(),
        )
        .unwrap();
        let policy = rsi_tools_protocol::ToolExecutionPolicy {
            mode: rsi_sandbox::SandboxMode::WorkspaceWrite,
            cwd: path.into(),
            workspace: path.into(),
        };
        serde_json::from_slice::<rsi_tools_protocol::ToolExecutionPolicy>(
            &serde_json::to_vec(&policy).unwrap(),
        )
        .unwrap();
        let stamp = rsi_sandbox::EnforcementStamp {
            requested: rsi_sandbox::SandboxMode::WorkspaceWrite,
            backend: rsi_sandbox::SandboxBackend::Bubblewrap {
                sha256: "0".repeat(64),
            },
            workspace: path.into(),
            filesystem: rsi_sandbox::SandboxFileSystem::WorkspaceWrite,
            scratch: rsi_sandbox::SandboxScratch::PrivateTmp,
            network: rsi_sandbox::SandboxNetwork::Host,
        };
        serde_json::from_slice::<rsi_sandbox::EnforcementStamp>(
            &serde_json::to_vec(&stamp).unwrap(),
        )
        .unwrap();
    }
    cases.push("durable header decoding");
    progress("passed", "durable header decoding");
    let execution =
        rsi_meta::Execution::browser().map_err(|error| JsValue::from_str(&error.to_string()))?;
    scenario(
        "scenarios::isolated_controller_scopes",
        scenarios::isolated_controller_scopes(execution.clone()),
        &mut cases,
    )
    .await;
    scenario(
        "scenarios::owned_submission_drain",
        scenarios::owned_submission_drain(execution.clone()),
        &mut cases,
    )
    .await;
    scenario(
        "scenarios::exact_command_reconciliation",
        scenarios::exact_command_reconciliation(execution.clone()),
        &mut cases,
    )
    .await;
    scenario(
        "scenarios::owned_source_window_reads",
        scenarios::owned_source_window_reads(execution.clone()),
        &mut cases,
    )
    .await;
    scenario(
        "scenarios::independent_projection_observation",
        scenarios::independent_projection_observation(execution.clone()),
        &mut cases,
    )
    .await;
    scenario(
        "scenarios::explicit_reconciliation_cancellation",
        scenarios::explicit_reconciliation_cancellation(execution.clone()),
        &mut cases,
    )
    .await;
    scenario(
        "scenarios::acknowledged_cursor",
        scenarios::acknowledged_cursor(execution.clone()),
        &mut cases,
    )
    .await;
    scenario(
        "scenarios::message_claim_cancellation_and_terminal_delivery",
        scenarios::message_claim_cancellation_and_terminal_delivery(execution.clone()),
        &mut cases,
    )
    .await;
    scenario(
        "scenarios::bounded_read_capacity_recovery_and_cancellation",
        scenarios::bounded_read_capacity_recovery_and_cancellation(execution.clone()),
        &mut cases,
    )
    .await;
    scenario(
        "shells::session_free_shell_owns_bounded_isolated_profiles_with_shared_domain",
        shells::session_free_shell_owns_bounded_isolated_profiles_with_shared_domain(
            execution.clone(),
        ),
        &mut cases,
    )
    .await;
    scenario(
        "shells::retirement_cancels_partial_surface_activation_and_drains_its_owner",
        shells::retirement_cancels_partial_surface_activation_and_drains_its_owner(
            execution.clone(),
        ),
        &mut cases,
    )
    .await;
    scenario(
        "shells::abandoned_surface_cleanup_failure_cannot_be_reported_as_clean_shell_shutdown",
        shells::abandoned_surface_cleanup_failure_cannot_be_reported_as_clean_shell_shutdown(
            execution.clone(),
        ),
        &mut cases,
    )
    .await;
    scenario(
        "ui::declaration_reorder_drives_surfaces_and_renderers_with_actual_target_mappings",
        ui::declaration_reorder_drives_surfaces_and_renderers_with_actual_target_mappings(
            execution.clone(),
        ),
        &mut cases,
    )
    .await;
    scenario(
        "ui::dropped_waiter_and_contribution_retirement_drain_admitted_mutation_once",
        ui::dropped_waiter_and_contribution_retirement_drain_admitted_mutation_once(
            execution.clone(),
        ),
        &mut cases,
    )
    .await;
    scenario(
        "ui::replacement_target_foreign_application_and_business_payload_are_fenced",
        ui::replacement_target_foreign_application_and_business_payload_are_fenced(
            execution.clone(),
        ),
        &mut cases,
    )
    .await;
    scenario(
        "ui::action_capacity_is_not_released_when_response_waiters_are_dropped",
        ui::action_capacity_is_not_released_when_response_waiters_are_dropped(execution.clone()),
        &mut cases,
    )
    .await;
    scenario(
        "ui::duplicate_and_failed_activation_preserve_existing_bundle_and_reject_foreign_buttons",
        ui::duplicate_and_failed_activation_preserve_existing_bundle_and_reject_foreign_buttons(
            execution.clone(),
        ),
        &mut cases,
    )
    .await;
    scenario(
        "ui::independent_registry_and_reactivated_bundle_never_reuse_old_references",
        ui::independent_registry_and_reactivated_bundle_never_reuse_old_references(
            execution.clone(),
        ),
        &mut cases,
    )
    .await;
    scenario(
        "ui::presentation_close_signals_reads_and_preserves_admitted_mutations",
        ui::presentation_close_signals_reads_and_preserves_admitted_mutations(execution.clone()),
        &mut cases,
    )
    .await;
    scenario(
        "files::malformed_open_binding_and_file_pages_never_escape_or_replay",
        files::malformed_open_binding_and_file_pages_never_escape_or_replay(),
        &mut cases,
    )
    .await;
    scenario(
        "files::directory_client_checks_parent_exact_names_order_and_forward_progress",
        files::directory_client_checks_parent_exact_names_order_and_forward_progress(),
        &mut cases,
    )
    .await;
    let resources = rsi_meta_execution::browser_resource_snapshot();
    assert_eq!(resources.pending_timers, 0);
    assert_eq!(resources.active_alarms, 0);
    Ok(serde_json::json!({
        "status":"passed", "command_reconciliation":"passed", "pending_timers":resources.pending_timers,
        "active_alarms":resources.active_alarms,
        "cases":cases
    }).to_string())
}
