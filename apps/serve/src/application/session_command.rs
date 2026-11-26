use super::BoundSessions;
use super::session_history::resource_key;
use rsi_agent_session_protocol::{DomainRequestId, SessionCommandInvocation, SessionId};
use rsi_extension::{Error, Result};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug)]
struct Maintenance {
    session: SessionId,
    lease: Box<dyn rsi_agent_turn_protocol::SessionMaintenance>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    List,
    Prepare {
        expected_draft_revision: Option<u64>,
    },
    Execute {
        invocation: SessionCommandInvocation,
    },
    Goal {
        control: rsi_goal::GoalControl,
    },
    DisarmGoal,
    BeginMaintenance {
        key: String,
    },
    WaitMaintenanceCancel {
        key: String,
    },
    FinishMaintenance {
        key: String,
    },
    Compact {
        request_id: DomainRequestId,
    },
    CompactRegion {
        region: rsi_agent_turn_protocol::CompactionRegion,
    },
    CheckCheckpointOwner,
    InstallPluginCheckpoint {
        turn: rsi_agent_session_protocol::TurnId,
        effect: rsi_agent_session_protocol::EffectId,
        checkpoint: rsi_agent_session_protocol::PluginCheckpoint,
    },
    Status {
        request_id: DomainRequestId,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    request: String,
    session_id: SessionId,
    operation: Operation,
}
impl BoundSessions {
    pub(super) async fn command(&self, payload: Value) -> Result<Value> {
        let input: Input = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let read_only = matches!(input.operation, Operation::List | Operation::Status { .. });
        let handle = async {
            if read_only {
                self.interactive_handle(&input.request, &input.session_id)
                    .await
            } else {
                self.interactive_credential_handle(&input.request, &input.session_id)
                    .await
            }
        };
        let finish = matches!(input.operation, Operation::FinishMaintenance { .. });
        let reply = match input.operation {
            Operation::List => {
                serde_json::to_value(handle.await?.commands().await.map_err(remote)?)
            }
            Operation::Prepare {
                expected_draft_revision,
            } => serde_json::to_value(
                handle
                    .await?
                    .prepare_application_commands(expected_draft_revision)
                    .await
                    .map_err(remote)?,
            ),
            Operation::Execute { invocation } => serde_json::to_value(
                handle
                    .await?
                    .execute_command(invocation)
                    .await
                    .map_err(remote)?,
            ),
            Operation::Goal { control } => serde_json::to_value(
                handle
                    .await?
                    .control_application_goal(control)
                    .await
                    .map_err(remote)?,
            ),
            Operation::DisarmGoal => {
                handle
                    .await?
                    .disarm_application_goal()
                    .await
                    .map_err(remote)?;
                return Ok(Value::Null);
            }
            Operation::BeginMaintenance { key } => {
                let key = resource_key("maintenance", &key)?;
                let Some(lease) = handle
                    .await?
                    .begin_application_maintenance()
                    .await
                    .map_err(remote)?
                else {
                    return Ok(Value::Bool(false));
                };
                self.routes
                    .retain_resource(
                        self.plugin_callback.id(),
                        key,
                        Arc::new(Maintenance {
                            session: input.session_id,
                            lease,
                        }),
                    )
                    .map_err(|_| Error::Cancelled)?;
                return Ok(Value::Bool(true));
            }
            Operation::WaitMaintenanceCancel { key } | Operation::FinishMaintenance { key } => {
                let key = resource_key("maintenance", &key)?;
                let (held, retired) = self
                    .routes
                    .shared_resource::<Maintenance>(&input.request, &key)
                    .map_err(|_| Error::Cancelled)?;
                if held.session != input.session_id {
                    return Err(Error::Protocol);
                }
                if finish {
                    self.routes.release_resource(&input.request, &key);
                } else {
                    let stop = held.lease.cancellation();
                    drop(held);
                    tokio::select! {
                        () = retired.cancelled() => return Err(Error::Cancelled),
                        () = stop.cancelled() => {},
                    }
                }
                return Ok(Value::Null);
            }
            Operation::Compact { request_id } => {
                serde_json::to_value(handle.await?.compact(request_id).await.map_err(remote)?)
            }
            Operation::CompactRegion { region } => serde_json::to_value(
                handle
                    .await?
                    .compact_application_region(region)
                    .await
                    .map_err(remote)?,
            ),
            Operation::CheckCheckpointOwner => Ok(Value::Bool(
                handle.await?.is_host_owner().await.map_err(remote)?,
            )),
            Operation::InstallPluginCheckpoint {
                turn,
                effect,
                checkpoint,
            } => match handle
                .await?
                .install_plugin_checkpoint(turn, effect, checkpoint)
                .await
            {
                Ok(()) => Ok(Value::Null),
                Err(rsi_session_protocol::SessionError::Invalid(reason)) => {
                    Ok(serde_json::json!({ "rejected": reason }))
                }
                Err(error) => return Err(remote(error)),
            },
            Operation::Status { request_id } => serde_json::to_value(
                handle
                    .await?
                    .command_status(&request_id)
                    .await
                    .map_err(remote)?,
            ),
        };
        reply.map_err(|_| Error::Protocol)
    }
}
fn remote(error: impl std::fmt::Display) -> Error {
    Error::Remote(error.to_string())
}
