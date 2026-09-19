use super::*;
use rsi_agent_session_protocol::{
    ActivationOutcome, AgentResultLocator, OutputContract, ToolConclusion,
};

#[tokio::test]
async fn foreign_tool_cannot_forge_a_structured_conclusion() {
    structured_result("echo").await;
}
#[tokio::test]
async fn structured_result_is_exact_final_and_initial_only() {
    structured_result("report_result").await;
}
#[allow(clippy::too_many_lines)] // One ordered lifecycle proves provisional, final and later-activation result identity.
async fn structured_result(tool_name: &str) {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_workers();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header("structured-root")),
            message: mailbox_message("structured-root-input"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _root_lease = kernel.register("structured-root-worker".into()).unwrap();
    let root = kernel
        .claim("structured-root-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = control_tool_caller(&kernel, &root).await;
    let contract =
        OutputContract::new(serde_json::json!({"type":"object","additionalProperties":false}))
            .unwrap();
    let child = SessionId::new("structured-child").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            output_contract: Some(contract.clone()),
            role: None,
            model: None,
            reasoning_effort: None,
            cancellation: CancellationToken::new(),
            caller,
            child_session_id: child.clone(),
            task_name: "structured".into(),
            message_id: MessageId::new("structured-initial").unwrap(),
            message: "return an object".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    tool_origin::finish_control_tool(&kernel, &root).await;
    let caller = kernel.agent_caller(&root).unwrap();
    let _child_lease = kernel.register("structured-child-worker".into()).unwrap();
    let claim = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        kernel.claim("structured-child-worker", CancellationToken::new()),
    )
    .await
    .expect("child claim deadline")
    .unwrap()
    .unwrap();
    let composition = kernel.composition(&claim).unwrap();
    assert_eq!(composition.output_contract(), Some(&contract));
    assert!(
        composition
            .tools()
            .definitions()
            .iter()
            .any(|tool| tool.name() == "report_result")
    );
    tool_origin::publish_model_source(&kernel, &claim, "report", tool_name, &snapshot()).await;
    let effect = EffectId::new("report-effect").unwrap();
    let identity =
        ToolResultIdentity::new("report-owner", "report-effect", "report", "a".repeat(64)).unwrap();
    let publish = |body| async {
        let facts = kernel
            .publish(&claim, vec![body])
            .await
            .unwrap()
            .published();
        kernel
            .flush(&claim, facts.last().unwrap().seq())
            .await
            .unwrap();
        facts
    };
    publish(SessionFactBody::ToolIntent {
        turn_id: claim.turn_id().clone(),
        effect_id: effect.clone(),
        source_model_effect_id: EffectId::new("source-model").unwrap(),
        identity: identity.clone(),
        name: tool_name.into(),
        arguments: serde_json::json!({}),
        approval: None,
        parallel_safe: false,
    })
    .await;
    publish(SessionFactBody::ToolStarted {
        turn_id: claim.turn_id().clone(),
        effect_id: effect.clone(),
        identity: identity.clone(),
    })
    .await;
    let summary = contract.summarize(&serde_json::json!({})).unwrap();
    let body = SessionFactBody::ToolResult {
        turn_id: claim.turn_id().clone(),
        effect_id: effect.clone(),
        identity,
        result: rsi_tools_protocol::ToolResult::new(serde_json::json!({}), vec![], false).unwrap(),
        conclusion: Some(ToolConclusion {
            structured: Some(summary.clone()),
        }),
    };
    if tool_name != "report_result" {
        assert!(
            matches!(
                kernel.publish(&claim, vec![body.clone()]).await,
                Err(TurnError::Invalid(_))
            ),
            "an ordinary Tool must not conclude the structured activation"
        );
        let mut ordinary = body;
        if let SessionFactBody::ToolResult { conclusion, .. } = &mut ordinary {
            *conclusion = None;
        }
        publish(ordinary).await;
        assert!(
            matches!(kernel.finish_turn(&claim, &TurnOutcome::Completed).await.unwrap().body(),
            SessionFactBody::TurnTerminal { outcome: TurnOutcome::Failed { code, .. }, result: None, .. } if code == "structured_output.missing")
        );
        kernel
            .finish_turn(&root, &TurnOutcome::Completed)
            .await
            .unwrap();
        kernel.shutdown(worker).await.unwrap();
        return;
    }
    let result = publish(body).await;
    let activation = store
        .active_activation(&child)
        .await
        .unwrap()
        .unwrap()
        .activation_id;
    let locator = AgentResultLocator {
        child_session_id: child.clone(),
        activation_id: activation,
        turn_id: claim.turn_id().clone(),
        fact_seq: result[0].seq(),
    };
    assert!(
        kernel.read_agent_result(&caller, &locator).await.is_err(),
        "published result alone is not final activation success"
    );
    assert!(
        kernel
            .publish(
                &claim,
                vec![SessionFactBody::StepStarted {
                    turn_id: claim.turn_id().clone(),
                    step_id: StepId::new("forbidden-next").unwrap()
                }]
            )
            .await
            .is_err()
    );
    let terminal = kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    assert!(
        matches!(terminal.body(), SessionFactBody::TurnTerminal { result: Some(reference), .. } if reference.locator() == locator)
    );
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            kernel.read_agent_result(&caller, &locator)
        )
        .await
        .expect("exact Completion read must release Store admission before reading its result Fact")
        .unwrap(),
        serde_json::json!({})
    );
    let mut wrong = locator.clone();
    wrong.fact_seq += 1;
    assert!(kernel.read_agent_result(&caller, &wrong).await.is_err());
    wrong = locator.clone();
    wrong.activation_id = ActivationId::new("other-activation").unwrap();
    assert!(kernel.read_agent_result(&caller, &wrong).await.is_err());
    let completion = store
        .read_agent_mailbox(root.session_id(), None)
        .await
        .unwrap()
        .pending
        .into_iter()
        .find(|entry| matches!(entry.message.source, AgentMessageSource::Completion { .. }))
        .unwrap();
    assert!(matches!(
        completion.message.source,
        AgentMessageSource::Completion {
            outcome: ActivationOutcome::Completed { result: Some(_) },
            ..
        }
    ));
    kernel
        .send_agent_message(SendAgentMessage {
            caller: caller.clone(),
            cancellation: CancellationToken::new(),
            target_session_id: child.clone(),
            message_id: MessageId::new("structured-followup").unwrap(),
            message: "continue without a schema".into(),
            start_new_turn: true,
        })
        .await
        .unwrap();
    let followup = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        kernel.claim("structured-child-worker", CancellationToken::new()),
    )
    .await
    .expect("child claim deadline")
    .unwrap()
    .unwrap();
    assert!(
        kernel
            .composition(&followup)
            .unwrap()
            .output_contract()
            .is_none()
    );
    assert!(
        !kernel
            .composition(&followup)
            .unwrap()
            .tools()
            .definitions()
            .iter()
            .any(|tool| tool.name() == "report_result")
    );
    assert!(matches!(
        kernel
            .finish_turn(&followup, &TurnOutcome::Completed)
            .await
            .unwrap()
            .body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::Completed,
            result: None,
            ..
        }
    ));
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            kernel.read_agent_result(&caller, &locator)
        )
        .await
        .expect("exact Completion read must release Store admission before reading its result Fact")
        .unwrap(),
        serde_json::json!({}),
        "a later activation cannot replace the exact old result"
    );
    kernel
        .finish_turn(&root, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel.shutdown(worker).await.unwrap();
}
