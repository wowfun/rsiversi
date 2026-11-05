use super::*;
use rsi_agent_composition_protocol::{
    AgentSessionDraft, DraftCommandPreparation, SessionCommandRegistration,
};
use rsi_agent_session_protocol::{
    AgentMessage, AgentMessageContent, AgentMessageSource, CommandArguments, DomainRequestId,
    MessageDelivery, MessageId, MessageOptions, SessionCommandDescriptor, SessionCommandInvocation,
};
use rsi_agent_turn_protocol::{SessionCommandsContract, SubmitMessage};

pub(super) fn registration(
    adapter: &Arc<rsi_extension_agent::AgentAdapter>,
) -> ContributionRegistration {
    let id = ContributionId::new("rsi.extension.command.plan").unwrap();
    let descriptor =
        SessionCommandDescriptor::new(id.clone(), "plan", "Original Plan", true).unwrap();
    ContributionRegistration::new(
        id.clone(),
        0,
        ContributionKind::Command(SessionCommandRegistration::new(
            descriptor,
            adapter.command(id, "plan".into()),
        )),
    )
}

pub(super) async fn durable_command(
    stack: &BaseStack,
    session: &SessionId,
    request: &str,
    input: &str,
) {
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let commands = stack
        .runtime
        .root()
        .lookup_local::<SessionCommandsContract>()
        .unwrap();
    let view = commands
        .list(turns.prepare_resume(session).await.unwrap())
        .await
        .unwrap();
    commands
        .execute(
            turns.prepare_resume(session).await.unwrap(),
            SessionCommandInvocation {
                command: ContributionId::new("rsi.extension.command.plan").unwrap(),
                request_id: DomainRequestId::new(request).unwrap(),
                expected_revision: view.revision(),
                arguments: CommandArguments::new(json!(input)).unwrap(),
            },
        )
        .await
        .unwrap();
}
fn invocation(draft: &AgentSessionDraft, request: &str, input: &str) -> SessionCommandInvocation {
    SessionCommandInvocation {
        command: draft.command_descriptors()[0].id().clone(),
        request_id: DomainRequestId::new(request).unwrap(),
        expected_revision: draft.revision(),
        arguments: CommandArguments::new(json!(input)).unwrap(),
    }
}
async fn draft_command(
    draft: &mut AgentSessionDraft,
    invocation: SessionCommandInvocation,
) -> Result<(), String> {
    let DraftCommandPreparation::Run(prepared) = draft.prepare_command(invocation).unwrap() else {
        panic!("new invocation")
    };
    let mutation = prepared
        .execute(CancellationToken::new())
        .await
        .map_err(|error| error.to_string())?;
    draft
        .apply_command(mutation)
        .finish()
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tokio::test]
#[ignore = "requires RSI_EXTENSION_NODE and RSI_DSH_INSTALLED_COHORT built from original upstream"]
#[expect(
    clippy::too_many_lines,
    reason = "One native scenario crosses lost replies, draft swaps, first mailbox publication and durable receipt recovery."
)]
async fn original_plan_commands_reconcile_draft_and_durable_receipts_without_handler_replay() {
    let cohort = std::path::PathBuf::from(
        std::env::var("RSI_DSH_INSTALLED_COHORT").expect("independent cohort"),
    );
    for mode in ["ack", "stage"] {
        let stack = BaseStack::activate().await;
        let entry = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/extension-lost-ack.mjs");
        let (process, extension, connection) = connect(&stack, &cohort, &entry).await;
        connection
            .request(
                "fixture.arm",
                json!({"mode":mode}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let definition = rsi_extension_agent::definition("a".repeat(64)).unwrap();
        let domains = DomainCatalog::new([definition.registration()]).unwrap();
        let handle = domains.bind(&definition).unwrap();
        *stack.composition.domains.lock().unwrap() = domains;
        let adapter = rsi_extension_agent::AgentAdapter::new(
            connection.clone(),
            stack.store.clone(),
            stack
                .runtime
                .root()
                .lookup_local::<rsi_agent_turn_protocol::TurnExecutionContract>()
                .unwrap(),
            handle.clone(),
            "a".repeat(64),
        )
        .unwrap();
        let callbacks = contributions::install(&stack, vec![registration(&adapter)]).await;
        let header = header_for_session("native-command");
        let mut draft = AgentSessionDraft::new(header, stack.composition.clone())
            .await
            .unwrap();
        let first = invocation(&draft, "lost-draft", "Inspect source before editing");
        assert!(draft_command(&mut draft, first.clone()).await.is_err());
        let receipt = draft.command_receipt(&first.request_id);
        assert_eq!(receipt.is_some(), mode == "ack");
        assert_eq!(
            draft.freeze().command_inputs().len(),
            usize::from(mode == "ack")
        );
        assert!(
            stack
                .store
                .header(draft.header().session_id())
                .await
                .is_err()
        );
        let next = invocation(&draft, "next-draft", "");
        draft_command(&mut draft, next).await.unwrap();
        if let Some(receipt) = receipt {
            assert!(
                matches!(draft.prepare_command(first).unwrap(), DraftCommandPreparation::Completed(found) if found == receipt)
            );
        }
        let turns = stack
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        let execution = stack
            .runtime
            .root()
            .lookup_local::<TurnExecutionContract>()
            .unwrap();
        let commands = stack
            .runtime
            .root()
            .lookup_local::<SessionCommandsContract>()
            .unwrap();
        let session = draft.header().session_id().clone();
        turns
            .submit_message(SubmitMessage {
                session: SubmitSession::Fresh(draft.freeze()),
                delivery: MessageDelivery::NextTurn,
                message: AgentMessage {
                    message_id: MessageId::new("wake").unwrap(),
                    source: AgentMessageSource::Human { attribution: None },
                    content: vec![AgentMessageContent::Text {
                        text: "Start".into(),
                    }],
                    options: MessageOptions::default(),
                },
            })
            .await
            .unwrap();
        let lease = execution.register("command-fixture".into()).unwrap();
        let claim = execution
            .claim("command-fixture", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        execution
            .finish_turn(&claim, &TurnOutcome::Completed)
            .await
            .unwrap();
        drop(lease);
        let list = commands
            .list(turns.prepare_resume(&session).await.unwrap())
            .await
            .unwrap();
        let off = SessionCommandInvocation {
            command: ContributionId::new("rsi.extension.command.plan").unwrap(),
            request_id: DomainRequestId::new("durable-off").unwrap(),
            expected_revision: list.revision(),
            arguments: CommandArguments::new(json!("off")).unwrap(),
        };
        let receipt = commands
            .execute(turns.prepare_resume(&session).await.unwrap(), off.clone())
            .await
            .unwrap();
        assert_eq!(
            commands
                .execute(turns.prepare_resume(&session).await.unwrap(), off)
                .await
                .unwrap(),
            receipt
        );
        let states = stack
            .store
            .read_domain_states(&session, None, None)
            .await
            .unwrap();
        let journal = handle.decode(&states.states[0].snapshot).unwrap();
        assert_eq!(
            journal.checkpoint.value()["views"]["values"]["plan"]["active"],
            false
        );
        assert!(journal.checkpoint.value()["pendingIntent"].is_null());
        assert!(journal.checkpoint.value()["mapping"]["turnId"].is_null());
        assert!(
            journal.checkpoint.value()["mapping"]["factCursor"]
                .as_u64()
                .unwrap()
                > 0
        );
        let counts = connection
            .request("fixture.counts", Value::Null, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(counts["executions"], 3);
        assert_eq!(
            counts["acknowledgements"],
            if mode == "ack" { 4 } else { 2 }
        );
        drop(draft);
        assert!(callbacks.dispose().await.is_clean());
        drop(adapter);
        drop(connection);
        assert!(extension.dispose().await.is_clean());
        assert!(process.dispose().await.is_clean());
        stack.dispose_services().await;
    }
}
