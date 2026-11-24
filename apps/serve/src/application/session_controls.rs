use rsi_agent_session_protocol::{
    AgentControlRecord, AgentControlRecordBody as Body, AgentMessage, DomainMutationSource,
    DomainStateCommit, MessageId, MessageTarget, ModelSelection, QueueSlot,
    SessionCommandInvocation, SessionCommandReceipt, SessionHeader, TurnId,
};
use rsi_agent_store_protocol::{SessionStore, StoreError};
use rsi_extension::{Error, Result};
use serde::Serialize;

#[derive(Serialize)]
struct ControlEvent<'a> {
    seq: u64,
    after_fact_seq: u64,
    time: u64,
    #[serde(flatten)]
    body: ControlBody<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    goal: Option<GoalChange<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ControlBody<'a> {
    Goal,
    WorkingDirectoryChanged {
        cwd: &'a str,
    },
    Accepted {
        message_id: &'a MessageId,
        message: &'a AgentMessage,
        target: &'a MessageTarget,
    },
    Successor {
        predecessor_id: &'a MessageId,
        successor_id: &'a MessageId,
        slot: &'a QueueSlot,
    },
    Claimed {
        message_id: &'a MessageId,
        turn_id: &'a TurnId,
    },
    Promoted {
        message_id: &'a MessageId,
    },
    Discarded {
        message_id: &'a MessageId,
    },
    Model {
        selection: ModelSelection,
        #[serde(flatten)]
        origin: Option<CommandOrigin<'a>>,
    },
}

/// A change can share its Store position with the baseline model selection.
#[derive(Serialize)]
struct GoalChange<'a> {
    current: Option<GoalSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<GoalCommand<'a>>,
}

#[derive(Serialize)]
struct GoalSummary {
    id: rsi_agent_session_protocol::DomainRequestId,
    objective: String,
    phase: rsi_agent_goal::GoalPhase,
    max_rounds: u64,
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    block_code: Option<String>,
}

#[derive(Serialize)]
struct GoalCommand<'a> {
    request_id: &'a rsi_agent_session_protocol::DomainRequestId,
    action: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    published_at_ms: Option<u64>,
}

fn goal_change(commit: &DomainStateCommit) -> Result<Option<GoalChange<'_>>> {
    use rsi_agent_goal::{GOAL_COMMAND, GOAL_DOMAIN, GoalState};
    // Reservation charges and unverified model reports do not change the public phase.
    if matches!(
        commit.source(),
        DomainMutationSource::Turn { .. }
            | DomainMutationSource::Continuation {
                reservation: Some(_),
                ..
            }
    ) {
        return Ok(None);
    }
    let update = commit
        .updates()
        .iter()
        .find(|u| u.snapshot().identity().id() == GOAL_DOMAIN);
    let current = if let Some(update) = update {
        let state = GoalState::decode(update.snapshot()).map_err(Error::Remote)?;
        if commit.source().is_initial() && state.goal.is_none() {
            return Ok(None);
        }
        state.goal.map(|goal| GoalSummary {
            id: goal.id,
            objective: goal.objective,
            phase: goal.phase,
            max_rounds: goal.max_rounds,
            reason: goal.reason,
            block_code: goal.block_code,
        })
    } else if commit.removed().iter().any(|id| id.id() == GOAL_DOMAIN) {
        None
    } else {
        return Ok(None);
    };
    let command = match commit.source() {
        DomainMutationSource::Command { invocation }
            if invocation.command.as_str() == GOAL_COMMAND =>
        {
            Some(GoalCommand {
                request_id: &invocation.request_id,
                published_at_ms: invocation.arguments.value()["published_at_ms"].as_u64(),
                action: invocation.arguments.value()["action"]
                    .as_str()
                    .ok_or(Error::Protocol)?,
            })
        }
        _ => None,
    };
    Ok(Some(GoalChange { current, command }))
}

#[derive(Serialize)]
struct CommandOrigin<'a> {
    invocation: &'a SessionCommandInvocation,
    receipt: SessionCommandReceipt,
}

pub(super) async fn read_controls(
    store: &dyn SessionStore,
    header: &SessionHeader,
    after: u64,
    through: u64,
) -> Result<(u64, String)> {
    let limit = through
        .checked_sub(after)
        .filter(|count| *count > 0)
        .ok_or(Error::Protocol)?
        .min(64) as usize;
    let page = match store
        .read_controls(
            header.session_id(),
            after,
            limit,
            super::session_history::HISTORY_PAGE_BYTES,
        )
        .await
    {
        Err(StoreError::ReadRequiresCapacity { minimum_bytes }) => {
            store
                .read_controls(header.session_id(), after, 1, minimum_bytes)
                .await
        }
        result => result,
    }
    .map_err(|error| Error::Remote(error.to_string()))?;
    encode_controls(header, page, after, through)
}

pub(super) fn encode_controls(
    header: &SessionHeader,
    page: rsi_agent_store_protocol::StoreControlPage,
    after: u64,
    through: u64,
) -> Result<(u64, String)> {
    if through > page.durable_seq {
        return Err(Error::Protocol);
    }
    let mut cursor = after;
    let mut events = Vec::new();
    for (record, anchor) in page.records.iter().zip(page.fact_cursors) {
        if record.seq() > through {
            break;
        }
        cursor = record.seq();
        if let Some(event) = project_control(header, record, anchor)? {
            events.push(event);
        }
    }
    let encoded = serde_json::to_string(&events).map_err(|_| Error::Protocol)?;
    Ok((cursor, encoded))
}

fn project_control<'a>(
    header: &'a SessionHeader,
    record: &'a AgentControlRecord,
    anchor: u64,
) -> Result<Option<ControlEvent<'a>>> {
    let mut goal = None;
    let body =
        if let Some(value) = mailbox_event(record.body()) {
            value
        } else if let Body::WorkingDirectoryChanged { cwd } = record.body() {
            ControlBody::WorkingDirectoryChanged { cwd }
        } else if let Body::DomainStateCommitted { commit } = record.body() {
            goal = goal_change(commit)?;
            if let Some(update) = commit.updates().iter().find(|update| {
                update.snapshot().identity().id() == rsi_agent_model_selection::DOMAIN
            }) {
                ControlBody::Model {
                    selection: rsi_agent_model_selection::resolve_selection(
                        header,
                        Some(update.snapshot()),
                        None,
                    )
                    .map_err(|error| Error::Remote(error.to_string()))?,
                    origin: command_origin(record.seq(), commit)?,
                }
            } else if goal.is_some() {
                ControlBody::Goal
            } else {
                return Ok(None);
            }
        } else {
            return Ok(None);
        };
    Ok(Some(ControlEvent {
        seq: record.seq(),
        after_fact_seq: anchor,
        time: record.timestamp_ms(),
        body,
        goal,
    }))
}

fn command_origin(seq: u64, commit: &DomainStateCommit) -> Result<Option<CommandOrigin<'_>>> {
    if let DomainMutationSource::Command { invocation } = commit.source() {
        let receipt = SessionCommandReceipt::committed(seq, commit)
            .map_err(|error| Error::Remote(error.to_string()))?;
        return Ok(Some(CommandOrigin {
            invocation,
            receipt,
        }));
    }
    Ok(None)
}

fn mailbox_event(body: &Body) -> Option<ControlBody<'_>> {
    Some(match body {
        Body::MessageAccepted {
            message, target, ..
        } => ControlBody::Accepted {
            message_id: &message.message_id,
            message,
            target,
        },
        Body::MessageSuccessor {
            predecessor_id,
            successor_id,
            slot,
        } => ControlBody::Successor {
            predecessor_id,
            successor_id,
            slot,
        },
        Body::MessageClaimed {
            message_id,
            turn_id,
            ..
        } => ControlBody::Claimed {
            message_id,
            turn_id,
        },
        Body::MessagePromoted { message_id } => ControlBody::Promoted { message_id },
        Body::MessageDiscarded { message_id, .. } => ControlBody::Discarded { message_id },
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
