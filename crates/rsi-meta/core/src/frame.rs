//! Host-specific helpers around the SDK-owned native plugin frame contract.

pub(crate) use rsi_meta_plugin::{
    DurableCommand as DurablePluginCommand, Frame as PluginFrame, FrameBody as PluginFrameBody,
    LifecyclePhase,
};

use crate::protocol::{CommandOutcome, CommandOutcomeEnvelope};

pub(crate) fn durable_command_unavailable(command_id: String) -> PluginFrame {
    PluginFrame::service_event(
        Some(command_id),
        "control.apply-manifest",
        "failed",
        serde_json::json!({"code": "command_unavailable_during_lifecycle"}),
    )
}

pub(crate) fn durable_command_result(
    command_id: String,
    result: crate::Result<CommandOutcomeEnvelope>,
) -> PluginFrame {
    let (event, payload) = match result {
        Ok(outcome) => match outcome.payload {
            CommandOutcome::Applied => ("applied", serde_json::json!({})),
            CommandOutcome::NoChange => ("unchanged", serde_json::json!({})),
            CommandOutcome::RestartRequired { packages, .. } => (
                "restart_required",
                serde_json::json!({"packages": packages}),
            ),
            CommandOutcome::Rejected { code, message } => (
                "rejected",
                serde_json::json!({"code": code, "message": message}),
            ),
            _ => (
                "failed",
                serde_json::json!({"code": "invalid_command_outcome"}),
            ),
        },
        Err(error) => (
            "failed",
            serde_json::json!({"code": "host_error", "message": error.to_string()}),
        ),
    };
    PluginFrame::service_event(Some(command_id), "control.apply-manifest", event, payload)
}
