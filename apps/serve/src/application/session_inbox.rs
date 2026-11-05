use super::{
    Sessions,
    session_history::{fragment_text, resource_key},
};
use futures_util::StreamExt as _;
use rsi_agent_session_protocol::{
    AgentMessage, CommandMessage, MessageId, QueueMutationRequest, QueueOperationId, SessionId,
};
use rsi_agent_store_protocol::StorePendingMessage;
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
    Open {
        key: String,
    },
    Message {
        key: String,
        message_id: MessageId,
        offset: usize,
    },
    Close {
        key: String,
    },
    Mutate {
        mutation: QueueMutationRequest,
    },
    DiscardDraft {
        message_ids: Vec<MessageId>,
    },
    Status {
        operation_id: QueueOperationId,
    },
    MessageStatus {
        message_id: MessageId,
    },
    WaitDraft {
        revision: u64,
    },
    WaitIdle,
    RetainDraft {
        key: String,
    },
    ReleaseDraft {
        key: String,
    },
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
#[derive(Debug)]
struct Snapshot {
    session: SessionId,
    inputs: Inputs,
    message: tokio::sync::Mutex<Option<(MessageId, String)>>,
}
#[derive(Debug)]
enum Inputs {
    Draft(Vec<CommandMessage>),
    Durable(Vec<StorePendingMessage>),
}
enum Acceptance<'a> {
    Draft(&'a AgentMessage),
    Durable(u64),
}
impl Snapshot {
    fn publish(
        self,
        routes: &rsi_api_http::ApplicationUpstream,
        request: &str,
        key: String,
    ) -> Result<()> {
        let pending = match &self.inputs {
            Inputs::Draft(inputs) => !inputs.is_empty(),
            Inputs::Durable(inputs) => !inputs.is_empty(),
        };
        if pending {
            routes
                .retain_resource(request, key, Arc::new(self))
                .map_err(|_| Error::Cancelled)?;
        } else {
            routes.caller(request).map_err(|_| Error::Cancelled)?;
        }
        Ok(())
    }

    async fn message_fragment<F>(
        &self,
        id: &MessageId,
        offset: usize,
        load: impl FnOnce(u64) -> F,
    ) -> Result<Value>
    where
        F: std::future::Future<Output = Result<AgentMessage>>,
    {
        let mut cached = self.message.lock().await;
        if cached.as_ref().is_none_or(|(key, _)| key != id) {
            *cached = None;
            let encoded = match self.acceptance(id)? {
                Acceptance::Draft(message) => serde_json::to_string(message),
                Acceptance::Durable(accepted) => serde_json::to_string(&load(accepted).await?),
            }
            .map_err(|_| Error::Protocol)?;
            *cached = Some((id.clone(), encoded));
        }
        let page = fragment(&cached.as_ref().expect("captured message body").1, offset)?;
        if page["next_offset"] == page["total_bytes"] {
            *cached = None;
        }
        Ok(page)
    }

    fn acceptance(&self, id: &MessageId) -> Result<Acceptance<'_>> {
        match &self.inputs {
            Inputs::Draft(inputs) => inputs
                .iter()
                .find(|row| &row.message.message_id == id)
                .map(|row| Acceptance::Draft(&row.message)),
            Inputs::Durable(inputs) => inputs
                .iter()
                .find(|row| &row.message_id == id)
                .map(|row| Acceptance::Durable(row.accepted_control_seq)),
        }
        .ok_or(Error::Protocol)
    }
    async fn capture(handle: &dyn rsi_session_protocol::SessionHandle) -> Result<(Self, Value)> {
        match handle.draft_snapshot().await {
            Ok(draft) => {
                let session = draft.header.session_id().clone();
                let pending: Vec<_> = draft
                    .command_inputs
                    .iter()
                    .map(|input| {
                        json!({
                    "message_id":input.message.message_id,"source_kind":input.message.source.kind(),
                    "delivery":input.delivery,"target":input.delivery})
                    })
                    .collect();
                let metadata = json!({"kind":"draft","session_id":session,
                    "revision":draft.revision,"pending":pending});
                Ok((
                    Self {
                        session,
                        inputs: Inputs::Draft(draft.command_inputs),
                        message: tokio::sync::Mutex::new(None),
                    },
                    metadata,
                ))
            }
            Err(rsi_session_protocol::SessionError::NotFound(_)) => {
                let state = handle.inspect().await.map_err(remote)?;
                let session = state.header.session_id().clone();
                let metadata = json!({"kind":"durable","session_id":session,
                    "active_turn_id":state.active_turn_id,"activation_phase":state.activation_phase,
                    "driver_active":state.tree.session.has_open_turn || state.tree.session.has_active_activation || state.tree.session.has_waking_message,
                    "cursor":{"fact_seq":state.durable_fact_seq,"control_seq":state.durable_control_seq},
                    "pending":state.pending});
                Ok((
                    Self {
                        session,
                        inputs: Inputs::Durable(state.pending),
                        message: tokio::sync::Mutex::new(None),
                    },
                    metadata,
                ))
            }
            Err(error) => Err(remote(error)),
        }
    }
}
fn fragment(encoded: &str, offset: usize) -> Result<Value> {
    let (end, part) = fragment_text(encoded, offset)?;
    Ok(json!({"json":part,"offset":offset,"next_offset":end,"total_bytes":encoded.len()}))
}
fn remote(error: impl std::fmt::Display) -> Error {
    Error::Remote(error.to_string().chars().take(1000).collect())
}

impl Sessions {
    #[expect(
        clippy::too_many_lines,
        reason = "One typed wire dispatcher shares caller and Session admission across inbox operations"
    )]
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
            Operation::Close { key } => {
                self.routes
                    .release_resource(&request, &resource_key("inbox", &key)?);
                Ok(Value::Null)
            }
            Operation::Message {
                key,
                message_id,
                offset,
            } => {
                let snapshot = self.inbox_snapshot(&request, &session_id, &key)?;
                let handle = self.read_handle(&request, &session_id, None).await?;
                let page = snapshot
                    .message_fragment(&message_id, offset, async |accepted| {
                        handle
                            .read_message(&message_id, accepted)
                            .await
                            .map_err(remote)
                    })
                    .await?;
                self.routes.caller(&request).map_err(|_| Error::Cancelled)?;
                Ok(page)
            }
            Operation::Open { key } => {
                let key = resource_key("inbox", &key)?;
                let handle = self.read_handle(&request, &session_id, None).await?;
                let (snapshot, metadata) = Snapshot::capture(handle.as_ref()).await?;
                snapshot.publish(&self.routes, &request, key)?;
                Ok(metadata)
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
    fn inbox_snapshot(
        &self,
        request: &str,
        session: &SessionId,
        key: &str,
    ) -> Result<Arc<Snapshot>> {
        let (snapshot, _) = self
            .routes
            .shared_resource::<Snapshot>(request, &resource_key("inbox", key)?)
            .map_err(|_| Error::Cancelled)?;
        if &snapshot.session != session {
            return Err(Error::Protocol);
        }
        Ok(snapshot)
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn snapshot(inputs: Inputs) -> Snapshot {
        Snapshot {
            session: SessionId::new("one").unwrap(),
            inputs,
            message: tokio::sync::Mutex::new(None),
        }
    }

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
            "driver_active":true,"cursor":{"fact_seq":u64::MAX,"control_seq":u64::MAX},"pending":rows})).unwrap();
        assert!(
            bytes.len() < 128 * 1024,
            "bounded metadata leaves room for the process envelope"
        );
    }

    #[test]
    fn empty_inbox_captures_do_not_consume_callback_resources() {
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
        for index in 0..128 {
            let inputs = if index % 2 == 0 {
                Inputs::Draft(vec![])
            } else {
                Inputs::Durable(vec![])
            };
            let key = format!("inbox-{index}");
            snapshot(inputs)
                .publish(&upstream, callback.id(), key.clone())
                .unwrap();
            assert!(
                upstream
                    .shared_resource::<Snapshot>(callback.id(), &key)
                    .is_err()
            );
        }
        let id = MessageId::new("queued").unwrap();
        snapshot(Inputs::Durable(vec![pending(&id)]))
            .publish(&upstream, callback.id(), "pending".into())
            .unwrap();
        assert!(
            upstream
                .shared_resource::<Snapshot>(callback.id(), "pending")
                .is_ok()
        );
        device.revoked.cancel();
        assert!(
            snapshot(Inputs::Draft(vec![]))
                .publish(&upstream, callback.id(), "late".into())
                .is_err()
        );
        assert!(
            upstream
                .shared_resource::<Snapshot>(callback.id(), "pending")
                .is_err()
        );
    }

    #[tokio::test]
    async fn fragmented_acceptance_reads_once_and_releases_the_encoded_body() {
        use rsi_agent_session_protocol::{AgentMessageContent, AgentMessageSource, MessageOptions};
        let id = MessageId::new("queued").unwrap();
        let captured = snapshot(Inputs::Durable(vec![pending(&id)]));
        let reads = AtomicUsize::new(0);
        let mut offset = 0;
        let mut restored = String::new();
        loop {
            let page = captured
                .message_fragment(&id, offset, async |accepted| {
                    assert_eq!(accepted, 4);
                    reads.fetch_add(1, Ordering::SeqCst);
                    Ok(AgentMessage {
                        message_id: id.clone(),
                        source: AgentMessageSource::Human { attribution: None },
                        content: vec![AgentMessageContent::Text {
                            text: "排队".repeat(100_000),
                        }],
                        options: MessageOptions::default(),
                    })
                })
                .await
                .unwrap();
            restored.push_str(page["json"].as_str().unwrap());
            offset = page["next_offset"].as_u64().unwrap().try_into().unwrap();
            if page["next_offset"] == page["total_bytes"] {
                break;
            }
            assert!(captured.message.lock().await.is_some());
        }
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert!(captured.message.lock().await.is_none());
        let message: AgentMessage = serde_json::from_str(&restored).unwrap();
        assert_eq!(message.message_id, id);
        assert_eq!(
            message.content,
            vec![AgentMessageContent::Text {
                text: "排队".repeat(100_000)
            }]
        );
        assert!(
            captured
                .message_fragment(&MessageId::new("other").unwrap(), 0, |_| async {
                    panic!("an acceptance outside the snapshot must not reach Store");
                })
                .await
                .is_err()
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
        // Draft owners must leave snapshot capacity available on this callback.
        upstream
            .retain_resource(callback.id(), "inbox".into(), Arc::new(0u64))
            .unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(callback);
        assert_eq!(dropped.load(Ordering::SeqCst), 64);
    }

    #[test]
    fn inbox_fragments_preserve_multibyte_acceptance_content() {
        let encoded = serde_json::to_string(&json!({"text":"界".repeat(100_000)})).unwrap();
        let mut offset = 0;
        let mut restored = String::new();
        while offset < encoded.len() {
            let value = fragment(&encoded, offset).unwrap();
            let part = value["json"].as_str().unwrap();
            assert!(part.len() <= 128 * 1024);
            assert_eq!(value["offset"], offset);
            assert_eq!(value["total_bytes"], encoded.len());
            offset = usize::try_from(value["next_offset"].as_u64().unwrap()).unwrap();
            restored.push_str(part);
        }
        assert_eq!(restored, encoded);
        assert!(fragment(&encoded, encoded.len()).is_err());
        let character = encoded.find('界').unwrap();
        assert!(fragment(&encoded, character + 1).is_err());
    }
}
