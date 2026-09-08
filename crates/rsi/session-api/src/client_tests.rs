use super::*;

#[path = "client_tests/commands.rs"]
mod commands;

#[path = "client_tests/draft.rs"]
mod draft;
use futures_util::{StreamExt as _, stream};
use rsi_agent_session_protocol::{
    AgentControlRecord, AgentControlRecordBody, AgentMessageContent, AgentMessageSource,
    AgentPresetId, FrozenAgentSettings, MessageDelivery, MessageOptions, SessionFact,
    SessionFactBody, TurnId,
};
use rsi_agent_turn_protocol::{MessageState, SessionObservation, TurnError};
use rsi_api_protocol::{
    ApiMessage, ApiOutput, ByteBudget, ConnectionDescription, EndpointId, HostEpoch,
    OperationClass, OperationSpec, RetainedBytes,
};
use rsi_session_protocol::SessionInput;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug)]
struct Remote {
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    input: ByteBudget,
    output: ByteBudget,
    replies: Mutex<VecDeque<rsi_api_protocol::Result<ApiOutput>>>,
    calls: AtomicUsize,
    requests: Mutex<Vec<(OperationSpec, Value)>>,
    gates: Mutex<VecDeque<Arc<Gate>>>,
}

#[derive(Debug)]
struct Gate {
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
        })
    }
}
impl Remote {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            description: ConnectionDescription {
                wire_version: 1,
                endpoint_id: EndpointId::from_bytes([1; 16]),
                host_epoch: HostEpoch::from_bytes([2; 16]),
            },
            operations: Operation::ALL.map(Operation::spec).into(),
            input: ByteBudget::default(),
            output: ByteBudget::default(),
            replies: Mutex::default(),
            calls: AtomicUsize::new(0),
            requests: Mutex::default(),
            gates: Mutex::default(),
        })
    }
    fn message(&self, value: &impl Serialize) -> ApiMessage {
        ApiMessage {
            json: self.output.encode(value, self.output.limit()).unwrap(),
            binary: None,
        }
    }
    fn reply(&self, value: &impl Serialize) {
        self.replies
            .lock()
            .unwrap()
            .push_back(Ok(ApiOutput::Reply(self.message(value))));
    }
    fn domain(&self, value: &impl Serialize) {
        self.replies.lock().unwrap().push_back(Err(ApiError::Domain(
            self.output.encode(value, 16 * 1024).unwrap(),
        )));
    }
    fn stream(&self, values: &[Value]) {
        self.replies
            .lock()
            .unwrap()
            .push_back(Ok(ApiOutput::Stream(Box::pin(stream::iter(
                values
                    .iter()
                    .map(|value| Ok(self.message(value)))
                    .collect::<Vec<_>>(),
            )))));
    }
}
#[async_trait]
impl ApiClient for Remote {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        self.input.clone()
    }
    async fn call(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        assert!(self.operations.contains(operation));
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.requests.lock().unwrap().push((
            operation.clone(),
            serde_json::from_slice(input.as_bytes()).unwrap(),
        ));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("one scripted response per explicit request");
        let gate = self.gates.lock().unwrap().pop_front();
        if let Some(gate) = gate {
            gate.entered.add_permits(1);
            gate.release.acquire().await.unwrap().forget();
        }
        reply
    }
}
fn header() -> SessionHeader {
    SessionHeader::new(
        SessionId::new("session").unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("standard").unwrap(),
        FrozenAgentSettings::new(
            "settings",
            "prompt",
            rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
            rsi_sandbox::SandboxMode::ReadOnly,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}
async fn fixture() -> (Arc<Remote>, SessionClient, Arc<dyn SessionHandle>) {
    let remote = Remote::new();
    let client = SessionClient::new(remote.clone()).unwrap();
    remote.reply(&header());
    let handle = client.attach(header().session_id()).await.unwrap();
    (remote, client, handle)
}
fn envelope(body: Value) -> Value {
    let mut reply =
        json!({"target":{"session_id":"session","header_key":header().fingerprint().unwrap()}});
    reply["body"] = body;
    reply
}
fn submit() -> SubmitInput {
    SubmitInput {
        delivery: MessageDelivery::NextTurn,
        message_id: MessageId::new("message").unwrap(),
        content: vec![SessionInput::Text {
            text: "hello".into(),
        }],
        model: None,
        sandbox: None,
    }
}
fn receipt() -> MessageReceipt {
    MessageReceipt {
        session_id: SessionId::new("session").unwrap(),
        message_id: MessageId::new("message").unwrap(),
        accepted_control_seq: 1,
        observed_fact_seq: 0,
        state: MessageState::Pending,
    }
}
fn fact(seq: u64) -> SessionFact {
    SessionFact::new(
        seq,
        1,
        SessionFactBody::CancelRequested {
            turn_id: TurnId::new("turn").unwrap(),
            reason: None,
        },
    )
    .unwrap()
}
fn observed_fact(seq: u64, durable: u64) -> Value {
    envelope(json!({"kind":"fact","fact":fact(seq),"durable_fact_seq":durable}))
}

#[tokio::test]
async fn submit_never_replays_and_preserves_caller_identity_when_reply_is_untrustworthy() {
    for change in 0..7 {
        let (remote, _, handle) = fixture().await;
        let mut reply = envelope(serde_json::to_value(receipt()).unwrap());
        match change {
            0 => reply["target"]["session_id"] = json!("foreign"),
            1 => reply["target"]["header_key"] = json!("b".repeat(64)),
            2 => reply["body"]["message_id"] = json!("foreign"),
            3 => reply["body"]["accepted_control_seq"] = json!(0),
            4 => reply["body"]["extra"] = json!(true),
            5 => reply["body"]["state"]["extra"] = json!(true),
            6 => reply["extra"] = json!(true),
            _ => unreachable!(),
        }
        remote.reply(&reply);
        assert!(
            matches!(handle.submit(submit()).await, Err(SessionError::MessageOutcomeUnknown { session, message }) if session == "session" && message == "message"),
            "case {change}"
        );
        assert_eq!(remote.calls.load(Ordering::Acquire), 2);
        assert_eq!(remote.input.used() + remote.output.used(), 0);
    }
    let (remote, _, handle) = fixture().await;
    remote.domain(&json!({"code":"message_conflict","session":"foreign","message":"foreign"}));
    assert!(
        matches!(handle.submit(submit()).await, Err(SessionError::MessageOutcomeUnknown { session, message }) if session == "session" && message == "message")
    );
    remote
        .replies
        .lock()
        .unwrap()
        .push_back(Err(ApiError::OutcomeUnknown));
    assert!(matches!(
        handle.submit(submit()).await,
        Err(SessionError::MessageOutcomeUnknown { .. })
    ));
    assert_eq!(remote.calls.load(Ordering::Acquire), 3);
}

#[tokio::test]
async fn finite_reads_reject_foreign_cursors_discontinuous_history_and_wrong_error_classes() {
    let (remote, _, handle) = fixture().await;
    let message = AgentMessage {
        message_id: MessageId::new("message").unwrap(),
        source: AgentMessageSource::Human,
        content: vec![AgentMessageContent::Text {
            text: "hello".into(),
        }],
        options: MessageOptions::default(),
    };
    remote.reply(&envelope(
        json!({"message":message,"accepted_control_seq":2}),
    ));
    assert!(matches!(
        handle.read_message(&message.message_id, 1).await,
        Err(SessionError::Api(ApiError::Invalid(_)))
    ));
    remote.reply(&envelope(
        json!({"before_seq":6,"durable_seq":5,"has_more":true,"facts":[fact(3),fact(5)]}),
    ));
    assert!(matches!(
        handle.history_before(Some(6), 3).await,
        Err(SessionError::Api(ApiError::Invalid(_)))
    ));
    remote.domain(&json!({"code":"message_conflict","session":"session","message":"message"}));
    assert!(matches!(
        handle.message_status(&message.message_id).await,
        Err(SessionError::Api(ApiError::Invalid(_)))
    ));
    remote.reply(&envelope(json!({"accepted":true,"already_terminal":true})));
    assert!(matches!(
        handle
            .cancel(CancelTarget::Turn(TurnId::new("turn").unwrap()), None)
            .await,
        Err(SessionError::Api(ApiError::OutcomeUnknown))
    ));
    assert_eq!(remote.input.used() + remote.output.used(), 0);
}

#[tokio::test]
async fn observation_checks_each_cursor_and_retains_decoded_payload_after_wire_release() {
    let (remote, client, handle) = fixture().await;
    let control = AgentControlRecord::new(
        1,
        1,
        AgentControlRecordBody::MessagePromoted {
            message_id: MessageId::new("message").unwrap(),
        },
    )
    .unwrap();
    remote.stream(&[
        observed_fact(1, 4),
        envelope(json!({"kind":"control","record":control,"durable_control_seq":1})),
        observed_fact(2, 3),
    ]);
    let mut stream = handle.observe(ObservationCursor::default()).await.unwrap();
    let first = stream.next().await.unwrap().unwrap();
    let SessionObservation::Fact { fact: held, .. } = first else {
        panic!("Fact")
    };
    let held_clone = held.clone();
    assert_eq!(
        client.state.observations.retained_bytes(),
        held.encoded_len()
    );
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        SessionObservation::Control { .. }
    ));
    assert!(matches!(
        stream.next().await.unwrap(),
        Err(TurnError::Invariant(_))
    ));
    drop(stream);
    assert_eq!(remote.output.used(), 0);
    drop(held);
    assert_eq!(
        client.state.observations.retained_bytes(),
        held_clone.encoded_len()
    );
    drop(held_clone);
    assert_eq!(client.state.observations.retained_bytes(), 0);
    for invalid in [observed_fact(2, 2), observed_fact(1, 0), {
        let mut value = observed_fact(1, 1);
        value["target"]["session_id"] = json!("foreign");
        value
    }] {
        remote.stream(&[invalid]);
        let mut stream = handle.observe(ObservationCursor::default()).await.unwrap();
        assert!(matches!(
            stream.next().await.unwrap(),
            Err(TurnError::Invariant(_))
        ));
    }
    assert_eq!(
        remote.output.used() + client.state.observations.retained_bytes(),
        0
    );
}

#[tokio::test]
async fn interactions_reject_foreign_questions_and_release_bytes_only_after_last_snapshot() {
    let (remote, client, handle) = fixture().await;
    let question = rsi_user_questions_protocol::QuestionRequest {
        id: "question".into(),
        session_id: "session".into(),
        turn_id: "turn".into(),
        questions: vec![rsi_user_questions_protocol::Question {
            id: "one".into(),
            prompt: "Continue?".into(),
            options: vec![],
        }],
    };
    remote.stream(&[envelope(json!({"approvals":[],"questions":[question]}))]);
    let mut stream = handle.observe_interactions().await.unwrap();
    let snapshot = stream.next().await.unwrap().unwrap();
    let clone = snapshot.clone();
    assert_eq!(remote.output.used(), 0);
    assert!(client.state.interactions.retained_bytes() > 0);
    drop(stream);
    drop(snapshot);
    assert!(client.state.interactions.retained_bytes() > 0);
    drop(clone);
    assert_eq!(client.state.interactions.retained_bytes(), 0);
    let mut foreign = question;
    foreign.session_id = "foreign".into();
    remote.stream(&[envelope(json!({"approvals":[],"questions":[foreign]}))]);
    let mut stream = handle.observe_interactions().await.unwrap();
    assert!(stream.next().await.unwrap().is_err());
    drop(stream);
    assert_eq!(
        remote.output.used() + client.state.interactions.retained_bytes(),
        0
    );
}

#[tokio::test]
async fn descendant_approvals_use_one_validated_tree_and_reuse_its_membership() {
    use rsi_agent_store_protocol::{
        StoreAgentDescendantStatus, StoreAgentSessionStatus, StoreAgentSubtreeSnapshot,
    };
    for foreign in [false, true] {
        let (remote, client, handle) = fixture().await;
        let status = |session_id| StoreAgentSessionStatus {
            session_id,
            durable_control_seq: 0,
            has_open_turn: false,
            has_active_activation: false,
            has_waking_message: false,
        };
        let descendants = (1..=255)
            .map(|index| {
                let task_name = format!("child-{index:03}");
                StoreAgentDescendantStatus {
                    status: status(SessionId::new(&task_name).unwrap()),
                    parent_session_id: header().session_id().clone(),
                    path: rsi_agent_session_protocol::AgentPath::new(vec![index]).unwrap(),
                    task_name,
                }
            })
            .collect::<Vec<_>>();
        let approvals = descendants
            .iter()
            .map(|child| ApprovalRequest {
                review: None,
                id: "approval".into(),
                subject: rsi_approval_protocol::ApprovalSubject::new(
                    child.status.session_id.as_str(),
                    "turn",
                    "effect",
                )
                .unwrap(),
                action: "write".into(),
                reason: "verify membership".into(),
            })
            .collect::<Vec<_>>();
        let inspection = StoreSessionInspection {
            header: header(),
            durable_fact_seq: 0,
            durable_control_seq: 0,
            pending: vec![],
            active_turn_id: None,
            activation_phase: None,
            tree: StoreAgentSubtreeSnapshot {
                session: status(header().session_id().clone()),
                descendants: if foreign { vec![] } else { descendants },
            },
        };
        inspection.validate().unwrap();
        let snapshot = envelope(json!({"approvals":approvals,"questions":[]}));
        remote.stream(&[snapshot.clone(), snapshot]);
        remote.reply(&envelope(serde_json::to_value(inspection).unwrap()));
        let mut stream = handle.observe_interactions().await.unwrap();
        if foreign {
            assert!(stream.next().await.unwrap().is_err());
        } else {
            for _ in 0..2 {
                assert_eq!(stream.next().await.unwrap().unwrap().approvals().len(), 255);
            }
            assert!(stream.next().await.is_none());
        }
        // One attach, one subscription, one inspection for all 255 owners and both snapshots.
        assert_eq!(remote.calls.load(Ordering::Acquire), 3);
        drop(stream);
        assert_eq!(
            remote.output.used() + client.state.interactions.retained_bytes(),
            0
        );
    }
}
