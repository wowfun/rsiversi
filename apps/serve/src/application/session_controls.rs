use super::Sessions;
use rsi_agent_session_protocol::{
    AgentControlRecord, AgentControlRecordBody as Body, DomainMutationSource, DomainStateCommit,
    SessionCommandReceipt, SessionHeader,
};
use rsi_extension::{Error, Result};
use serde_json::{Value, json};

impl Sessions {
    pub(super) async fn history_controls(
        &self,
        header: &SessionHeader,
        after: u64,
        through: u64,
    ) -> Result<Value> {
        if after > through {
            return Err(Error::Protocol);
        }
        let page = self
            .store
            .read_controls(
                header.session_id(),
                after,
                64,
                rsi_agent_store_protocol::MAXIMUM_STORE_CONTROL_PAGE_BYTES,
            )
            .await
            .map_err(|error| Error::Remote(error.to_string()))?;
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
        Ok(json!({"through":cursor,"events":events}))
    }
}

fn project_control(
    header: &SessionHeader,
    record: &AgentControlRecord,
    anchor: u64,
) -> Result<Option<Value>> {
    let mut value =
        if let Some(value) = mailbox_event(record.body()) {
            value
        } else if let Body::DomainStateCommitted { commit } = record.body() {
            let Some(update) = commit.updates().iter().find(|update| {
                update.snapshot().identity().id() == rsi_agent_model_selection::DOMAIN
            }) else {
                return Ok(None);
            };
            let domain = rsi_agent_session_protocol::DomainStateView {
                revision: update.revision(),
                snapshot: update.snapshot().clone(),
            };
            let selection =
                rsi_agent_model_selection::resolve_selection(header, Some(&domain.snapshot), None)
                    .map_err(|error| Error::Remote(error.to_string()))?;
            let mut value = json!({"kind":"model","selection":selection});
            command_origin(&mut value, record.seq(), commit)?;
            value
        } else {
            return Ok(None);
        };
    value["seq"] = json!(record.seq());
    value["after_fact_seq"] = json!(anchor);
    value["time"] = json!(record.timestamp_ms());
    Ok(Some(value))
}

fn command_origin(value: &mut Value, seq: u64, commit: &DomainStateCommit) -> Result<()> {
    if let DomainMutationSource::Command { invocation } = commit.source() {
        let receipt = SessionCommandReceipt::committed(seq, commit)
            .map_err(|error| Error::Remote(error.to_string()))?;
        value["invocation"] = json!(invocation);
        value["receipt"] = json!(receipt);
    }
    Ok(())
}

fn mailbox_event(body: &Body) -> Option<Value> {
    Some(match body {
        Body::MessageAccepted {
            message, target, ..
        } => json!({"kind":"accepted", "message_id":message.message_id,"target":target}),
        Body::MessageSuccessor {
            predecessor_id,
            successor_id,
            slot,
        } => {
            json!({"kind":"successor","predecessor_id":predecessor_id,"successor_id":successor_id,"slot":slot})
        }
        Body::MessageClaimed {
            message_id,
            turn_id,
            ..
        } => json!({"kind":"claimed","message_id":message_id,"turn_id":turn_id}),
        Body::MessagePromoted { message_id } => json!({"kind":"promoted","message_id":message_id}),
        Body::MessageDiscarded { message_id, .. } => {
            json!({"kind":"discarded","message_id":message_id})
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_command_origin_distinguishes_identical_values_and_aggregate_baselines() {
        use rsi_agent_session_protocol::{
            CommandArguments, CommandRevision, ContributionId, DomainIdentity, DomainRequestId,
            DomainRevision, DomainSnapshot, DomainStateUpdate, DomainStateValue,
            SessionCommandInvocation,
        };
        let update = DomainStateUpdate::new(
            DomainRevision::new(1),
            DomainSnapshot::new(
                DomainIdentity::new(rsi_agent_model_selection::DOMAIN, 1).unwrap(),
                DomainStateValue::new(json!({"model":{"deployment":"native","model":"chosen"}}))
                    .unwrap(),
            ),
        )
        .unwrap();
        let mut results = Vec::new();
        for id in ["first", "second"] {
            let invocation = SessionCommandInvocation {
                command: ContributionId::new(rsi_agent_model_selection::COMMAND).unwrap(),
                request_id: DomainRequestId::new(id).unwrap(),
                expected_revision: CommandRevision::Durable { control_seq: 3 },
                arguments: CommandArguments::new(update.snapshot().state().value().clone())
                    .unwrap(),
            };
            let commit = DomainStateCommit::new(
                Some(invocation.request_id.clone()),
                DomainMutationSource::Command {
                    invocation: invocation.clone(),
                },
                vec![update.clone()],
            )
            .unwrap();
            let mut value = json!({});
            command_origin(&mut value, 4, &commit).unwrap();
            assert_eq!(value["invocation"], json!(invocation));
            assert_eq!(
                value["receipt"],
                json!(SessionCommandReceipt::committed(4, &commit).unwrap())
            );
            assert!(command_origin(&mut json!({}), 5, &commit).is_err());
            results.push(value);
        }
        assert_ne!(results[0]["receipt"], results[1]["receipt"]);
        let initial =
            DomainStateUpdate::new(DomainRevision::new(0), update.snapshot().clone()).unwrap();
        let baseline =
            DomainStateCommit::new(None, DomainMutationSource::Baseline, vec![initial]).unwrap();
        let mut value = json!({});
        command_origin(&mut value, 1, &baseline).unwrap();
        assert_eq!(value, json!({}));
    }

    #[test]
    fn acceptance_metadata_stays_small_and_body_reads_preserve_exact_multibyte_input() {
        use rsi_agent_session_protocol::{
            AgentMessage, AgentMessageContent, AgentMessageSource, MessageDelivery, MessageId,
            MessageOptions, MessageTarget, SessionId,
        };
        let message = AgentMessage {
            message_id: MessageId::new("large-context").unwrap(),
            source: AgentMessageSource::Human { attribution: None },
            content: vec![AgentMessageContent::Text {
                text: "界".repeat(100_000),
            }],
            options: MessageOptions::default(),
        };
        let body = Body::MessageAccepted {
            message: message.clone(),
            delivery: MessageDelivery::NextTurn,
            bound_turn_id: None,
            root_session_id: SessionId::new("root").unwrap(),
            target: MessageTarget::NextTurn,
            wake_required: true,
        };
        let metadata = mailbox_event(&body).unwrap();
        assert_eq!(metadata["message_id"], "large-context");
        assert!(serde_json::to_vec(&metadata).unwrap().len() < 256);
        let encoded = serde_json::to_string(&message).unwrap();
        let mut offset = 0;
        let mut restored = String::new();
        while offset < encoded.len() {
            let fragment = super::super::session_history::fragment(&encoded, offset, 42).unwrap();
            assert_eq!(fragment["through_seq"], 42);
            restored.push_str(fragment["json"].as_str().unwrap());
            offset = usize::try_from(fragment["next_offset"].as_u64().unwrap()).unwrap();
        }
        assert_eq!(
            serde_json::from_str::<AgentMessage>(&restored).unwrap(),
            message
        );
    }
}
