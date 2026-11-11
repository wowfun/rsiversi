use super::{
    Sessions,
    session_history::{resource_key, stored_history},
};
use futures_util::StreamExt as _;
use rsi_agent_session_protocol::{MessageId, QueueMutationRequest, QueueOperationId, SessionId};
use rsi_agent_store_protocol::StoreSessionInspection;
use rsi_extension::{Error, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    request: String,
    session_id: SessionId,
    operation: Operation,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Open,
    Mutate { mutation: QueueMutationRequest },
    DiscardDraft { message_ids: Vec<MessageId> },
    Status { operation_id: QueueOperationId },
    MessageStatus { message_id: MessageId },
    WaitDraft { revision: u64 },
    WaitIdle,
    RetainDraft { key: String },
    ReleaseDraft { key: String },
}

#[derive(Debug, Default)]
struct DraftOwners(Mutex<HashMap<SessionId, HeldDraft>>);
#[derive(Debug)]
struct HeldDraft {
    key: String,
    owner: Box<dyn rsi_session_protocol::DraftOwner>,
}
impl DraftOwners {
    fn retain(
        &self,
        session: SessionId,
        key: String,
        owner: Box<dyn rsi_session_protocol::DraftOwner>,
    ) {
        let mut owners = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        owners.retain(|_, held| held.owner.is_active());
        owners.insert(session, HeldDraft { key, owner });
    }
    fn release(&self, session: &SessionId, key: &str) {
        let mut owners = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if owners.get(session).is_some_and(|held| held.key == key) {
            owners.remove(session);
        }
    }
}
async fn capture(handle: &dyn rsi_session_protocol::SessionHandle) -> Result<Value> {
    match handle.draft_snapshot().await {
        Ok(snapshot) => Ok(draft(&snapshot)),
        Err(rsi_session_protocol::SessionError::NotFound(_)) => {
            let state = handle.inspect().await.map_err(remote)?;
            Ok(durable(&state))
        }
        Err(error) => Err(remote(error)),
    }
}

fn draft(draft: &rsi_session_protocol::SessionDraftView) -> Value {
    let session = draft.header.session_id();
    let pending: Vec<_> = draft
        .command_inputs
        .iter()
        .map(|input| {
            json!({
                    "message_id":input.message.message_id,"source_kind":input.message.source.kind(),
                    "delivery":input.delivery,"target":input.delivery,"message":input.message})
        })
        .collect();
    json!({"kind":"draft","session_id":session,
                    "revision":draft.revision,"pending":pending})
}

fn durable(state: &StoreSessionInspection) -> Value {
    let session = state.header.session_id();
    let history = stored_history(
        &state.header,
        state.durable_fact_seq,
        state.durable_control_seq,
    );
    json!({"kind":"durable","session_id":session,"history":history,
                    "active_turn_id":state.active_turn_id,"activation_phase":state.activation_phase,
                    "driver_active":state.tree.session.has_open_turn || state.tree.session.has_active_activation || state.tree.session.has_waking_message,
                    "cursor":{"fact_seq":state.durable_fact_seq,"control_seq":state.durable_control_seq},
                    "pending":state.pending})
}
fn remote(error: impl std::fmt::Display) -> Error {
    Error::Remote(error.to_string().chars().take(1000).collect())
}

impl Sessions {
    pub(super) async fn inbox(&self, payload: Value) -> Result<Value> {
        let Input {
            request,
            session_id,
            operation,
        } = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        match operation {
            Operation::WaitIdle => {
                let handle = self.read_handle(&request, &session_id, None).await?;
                handle.wait_idle().await.map_err(remote)?;
                self.routes.caller(&request).map_err(|_| Error::Cancelled)?;
                Ok(Value::Null)
            }
            Operation::RetainDraft { key } => {
                let key = resource_key("draft", &key)?;
                let handle = self.interactive_handle(&request, &session_id).await?;
                let Some(owner) = handle.retain_draft().map_err(remote)? else {
                    return Ok(Value::Bool(false));
                };
                self.draft_owners(&request)?.retain(session_id, key, owner);
                Ok(Value::Bool(true))
            }
            Operation::ReleaseDraft { key } => {
                let key = resource_key("draft", &key)?;
                if let Ok((owners, _)) = self
                    .routes
                    .shared_resource::<DraftOwners>(&request, "draft-owners")
                {
                    owners.release(&session_id, &key);
                }
                Ok(Value::Null)
            }
            Operation::WaitDraft { revision } => {
                let handle = self.read_handle(&request, &session_id, None).await?;
                let mut changes = handle.observe_draft().await.map_err(remote)?;
                while let Some(current) = changes.next().await {
                    if current.map_err(remote)? != Some(revision) {
                        self.routes.caller(&request).map_err(|_| Error::Cancelled)?;
                        return Ok(Value::Null);
                    }
                }
                Err(Error::Protocol)
            }
            Operation::Open => {
                let handle = self.read_handle(&request, &session_id, None).await?;
                let captured = capture(handle.as_ref()).await?;
                self.routes.caller(&request).map_err(|_| Error::Cancelled)?;
                Ok(captured)
            }
            Operation::Mutate { mutation } => {
                let handle = self.interactive_handle(&request, &session_id).await?;
                serde_json::to_value(handle.mutate_queue(mutation).await.map_err(remote)?)
                    .map_err(|_| Error::Protocol)
            }
            Operation::DiscardDraft { message_ids } => {
                let handle = self.interactive_handle(&request, &session_id).await?;
                let snapshot = handle
                    .discard_draft_inputs(rsi_session_protocol::DiscardDraftInputs { message_ids })
                    .await
                    .map_err(remote)?;
                Ok(json!({"revision":snapshot.revision}))
            }
            Operation::Status { operation_id } => {
                let handle = self.read_handle(&request, &session_id, None).await?;
                serde_json::to_value(
                    handle
                        .queue_mutation_status(&operation_id)
                        .await
                        .map_err(remote)?,
                )
                .map_err(|_| Error::Protocol)
            }
            Operation::MessageStatus { message_id } => {
                let handle = self.read_handle(&request, &session_id, None).await?;
                match handle.message_status(&message_id).await {
                    Ok(receipt) => serde_json::to_value(receipt).map_err(|_| Error::Protocol),
                    Err(rsi_session_protocol::SessionError::NotFound(_)) => Ok(Value::Null),
                    Err(error) => Err(remote(error)),
                }
            }
        }
    }
    fn draft_owners(&self, request: &str) -> Result<Arc<DraftOwners>> {
        if let Ok((owners, _)) = self
            .routes
            .shared_resource::<DraftOwners>(request, "draft-owners")
        {
            return Ok(owners);
        }
        let owners = Arc::new(DraftOwners::default());
        if let Err(error) =
            self.routes
                .retain_resource(request, "draft-owners".into(), owners.clone())
        {
            // Another request may have installed the callback's shared owner table.
            return self
                .routes
                .shared_resource::<DraftOwners>(request, "draft-owners")
                .map(|(owners, _)| owners)
                .map_err(|_| remote(error));
        }
        Ok(owners)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_store_protocol::StorePendingMessage;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn pending(id: &MessageId) -> StorePendingMessage {
        use rsi_agent_session_protocol::{
            AgentMessageSourceKind, MessageDelivery, MessageTarget, QueueSlot,
        };
        StorePendingMessage {
            queue_slot: QueueSlot::initial(id, 1, 4),
            source_kind: AgentMessageSourceKind::Human,
            has_turn_options: false,
            message_id: id.clone(),
            delivery: MessageDelivery::NextTurn,
            target: MessageTarget::NextTurn,
            permits_promotion: false,
            bound_turn_id: None,
            accepted_control_seq: 4,
        }
    }

    fn header() -> rsi_agent_session_protocol::SessionHeader {
        use rsi_agent_session_protocol::{AgentPresetId, FrozenAgentSettings, SessionHeader};
        SessionHeader::new_local(
            SessionId::new("one").unwrap(),
            1,
            "/workspace",
            AgentPresetId::new("test").unwrap(),
            FrozenAgentSettings::new(
                "default",
                "system",
                rsi_ai_protocol::ModelRef::new("test", "baseline").unwrap(),
                rsi_sandbox::SandboxMode::WorkspaceWrite,
                false,
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn durable_inbox_supplies_replay_metadata_from_its_inspection() {
        use rsi_agent_session_protocol::{SessionFact, SessionFactBody, TurnId};
        use rsi_agent_store_protocol::{AppendBatch, SessionStore};
        let store = rsi_agent_testkit::MemoryStore::new();
        let header = header();
        let session = header.session_id();
        store
            .append(AppendBatch {
                session_id: session.clone(),
                expected_seq: 0,
                header: Some(header.clone()),
                facts: vec![Arc::new(
                    SessionFact::new(
                        1,
                        2,
                        SessionFactBody::TurnAccepted {
                            turn_id: TurnId::new("turn").unwrap(),
                            text: "input".into(),
                            model: None,
                            reasoning_effort: None,
                            sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                            require_approval: false,
                        },
                    )
                    .unwrap(),
                )],
            })
            .await
            .unwrap();
        let inspection = store.inspect_session(session).await.unwrap();
        let inbox = durable(&inspection);
        let history = &inbox["history"];
        assert_eq!(inbox["active_turn_id"], "turn");
        assert_eq!(history["id"], inbox["session_id"]);
        assert_eq!(history["through"], 1);
        assert_eq!(history["through"], inbox["cursor"]["fact_seq"]);
        assert_eq!(
            history["selection_cursor"],
            json!({"kind":"durable", "fact_seq":1, "control_seq":0})
        );
        assert_eq!(
            history["selection_cursor"]["control_seq"],
            inbox["cursor"]["control_seq"]
        );
        assert_eq!(history["preset"], "test");
        assert_eq!(
            history["model_baseline"]["model"],
            json!(header.settings().default_model())
        );
        assert!(
            history.get("model_selection").is_none(),
            "current model comes from replay at the captured cut"
        );
    }

    #[test]
    fn maximum_pending_metadata_fits_one_process_reply() {
        use rsi_agent_session_protocol::{
            MAXIMUM_AGENT_IDENTIFIER_BYTES, MAXIMUM_PENDING_AGENT_MESSAGES, TurnId,
        };
        let rows: Vec<_> = (0..MAXIMUM_PENDING_AGENT_MESSAGES)
            .map(|index| {
                let id = MessageId::new(format!(
                    "{}{:02}",
                    "x".repeat(MAXIMUM_AGENT_IDENTIFIER_BYTES - 2),
                    index
                ))
                .unwrap();
                let mut row = pending(&id);
                row.queue_slot.timestamp_ms = u64::MAX;
                row.queue_slot.control_seq = u64::MAX;
                row.accepted_control_seq = u64::MAX;
                row.bound_turn_id =
                    Some(TurnId::new("y".repeat(MAXIMUM_AGENT_IDENTIFIER_BYTES)).unwrap());
                row
            })
            .collect();
        let bytes = serde_json::to_vec(&json!({"kind":"durable","session_id":"s".repeat(MAXIMUM_AGENT_IDENTIFIER_BYTES),
            "active_turn_id":"t".repeat(MAXIMUM_AGENT_IDENTIFIER_BYTES),"activation_phase":rsi_agent_store_protocol::StoreActivationPhase::WaitingForDescendants,
            "driver_active":true,"cursor":{"fact_seq":u64::MAX,"control_seq":u64::MAX},"pending":rows,
            "history":stored_history(&header(), u64::MAX, u64::MAX)})).unwrap();
        assert!(
            bytes.len() < 128 * 1024,
            "bounded metadata leaves room for the process envelope"
        );
    }

    #[test]
    fn maximum_draft_inputs_round_trip_in_one_process_reply() {
        use rsi_agent_session_protocol::{
            AgentMessage, AgentMessageContent, AgentMessageSource, CommandMessage,
            CommandMessageSource, ContributionId, DomainRequestId, MAXIMUM_COMMAND_INPUT_BYTES,
            MAXIMUM_COMMAND_INPUTS, MessageDelivery, MessageOptions,
        };
        let mut inputs: Vec<_> = (0..MAXIMUM_COMMAND_INPUTS)
            .map(|index| {
                let source = CommandMessageSource {
                    command: ContributionId::new("fixture.inject").unwrap(),
                    request_id: DomainRequestId::new("held").unwrap(),
                    invocation_sha256: "a".repeat(64),
                    ordinal: index.try_into().unwrap(),
                    attribution: None,
                };
                CommandMessage {
                    message: AgentMessage {
                        message_id: source.message_id().unwrap(),
                        source: AgentMessageSource::Command { source },
                        content: vec![AgentMessageContent::Text { text: "x".into() }],
                        options: MessageOptions::default(),
                    },
                    delivery: MessageDelivery::NextStep,
                }
            })
            .collect();
        // Escaped text fills the admitted byte budget, not just character count.
        let mut remaining =
            MAXIMUM_COMMAND_INPUT_BYTES - serde_json::to_vec(&inputs).unwrap().len();
        for input in &mut inputs {
            let added = (remaining / 2).min(60 * 1024);
            let AgentMessageContent::Text { text } = &mut input.message.content[0] else {
                unreachable!()
            };
            text.push_str(&"\\".repeat(added));
            remaining -= added * 2;
        }
        CommandMessage::validate_batch(&inputs, None, true).unwrap();
        assert!(MAXIMUM_COMMAND_INPUT_BYTES - serde_json::to_vec(&inputs).unwrap().len() < 2);
        let view = rsi_session_protocol::SessionDraftView {
            header: header(),
            revision: 7,
            command_inputs: inputs,
        };
        let captured = draft(&view);
        assert_eq!(captured["kind"], "draft");
        assert_eq!(captured["revision"], 7);
        assert_eq!(
            captured["pending"].as_array().unwrap().len(),
            MAXIMUM_COMMAND_INPUTS
        );
        for (row, input) in captured["pending"]
            .as_array()
            .unwrap()
            .iter()
            .zip(&view.command_inputs)
        {
            assert_eq!(row["message_id"], input.message.message_id.as_str());
            assert_eq!(row["target"], "next_step");
            assert_eq!(
                serde_json::from_value::<AgentMessage>(row["message"].clone()).unwrap(),
                input.message
            );
        }
        assert!(
            serde_json::to_vec(&captured).unwrap().len() < 512 * 1024,
            "bounded draft bodies and routing metadata leave room for the 1 MiB process envelope"
        );
    }

    #[derive(Debug)]
    struct Owner {
        active: Arc<AtomicBool>,
        dropped: Arc<AtomicUsize>,
    }
    impl rsi_session_protocol::DraftOwner for Owner {
        fn is_active(&self) -> bool {
            self.active.load(Ordering::SeqCst)
        }
    }
    impl Drop for Owner {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn draft_owners_release_exact_generations_and_prune_published_entries() {
        let owners = DraftOwners::default();
        let dropped = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicBool::new(true));
        let owner = || {
            Box::new(Owner {
                active: active.clone(),
                dropped: dropped.clone(),
            })
        };
        let id = SessionId::new("one").unwrap();
        owners.retain(id.clone(), "old".into(), owner());
        owners.retain(id.clone(), "new".into(), owner());
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        owners.release(&id, "old");
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            1,
            "stale release cannot remove replacement"
        );
        active.store(false, Ordering::SeqCst);
        owners.retain(
            SessionId::new("two").unwrap(),
            "other".into(),
            Box::new(Owner {
                active: Arc::new(AtomicBool::new(true)),
                dropped: dropped.clone(),
            }),
        );
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            2,
            "publication is pruned on acquisition"
        );
        owners.release(&SessionId::new("two").unwrap(), "other");
        owners.release(&SessionId::new("two").unwrap(), "other");
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            3,
            "matching release is idempotent"
        );
    }

    #[test]
    fn draft_owners_use_one_callback_slot_and_retire_with_the_callback() {
        let upstream = rsi_api_http::ApplicationUpstream::preparing(
            rsi_credentials_protocol::SecretValue::new("a".repeat(64)).unwrap(),
            vec![],
            tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        let device = rsi_api_protocol::AuthenticatedDevice {
            id: rsi_api_protocol::DeviceId::generate().unwrap(),
            revoked: tokio_util::sync::CancellationToken::new(),
        };
        let callback = upstream.open_callback(&device).unwrap();
        let owners = Arc::new(DraftOwners::default());
        let dropped = Arc::new(AtomicUsize::new(0));
        for index in 0..64 {
            owners.retain(
                SessionId::new(format!("draft-{index}")).unwrap(),
                "key".into(),
                Box::new(Owner {
                    active: Arc::new(AtomicBool::new(true)),
                    dropped: dropped.clone(),
                }),
            );
        }
        upstream
            .retain_resource(callback.id(), "draft-owners".into(), owners)
            .unwrap();
        // Draft owners must leave history reader capacity available on this callback.
        upstream
            .retain_resource(callback.id(), "history".into(), Arc::new(0u64))
            .unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(callback);
        assert_eq!(dropped.load(Ordering::SeqCst), 64);
    }
}
