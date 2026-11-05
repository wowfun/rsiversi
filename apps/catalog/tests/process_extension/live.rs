use rsi_agent_composition_protocol::{AgentCompositionPin, PreparedFreshSession};
use rsi_agent_session_protocol::{
    DomainRequestId, ExtensionJournal, FrozenAgentSettings, SessionFactBody, SessionHeader,
    SessionId, TurnId, TurnOutcome,
};
use rsi_agent_store_protocol::SessionStoreContract;
use rsi_agent_turn_protocol::{SubmitSession, SubmitTurn, TurnServiceContract};
use rsi_credentials_protocol::SecretValue;
use rsi_host::{Profile, ProfileEntry};
use serde_json::json;
use std::{collections::BTreeMap, path::Path, time::Duration};

pub(super) fn configuration() -> (BTreeMap<String, SecretValue>, Profile) {
    let secret =
        SecretValue::new(std::env::var("DEEPSEEK_API_KEY").expect("explicit live credential"))
            .unwrap();
    (
        BTreeMap::from([("DEEPSEEK_API_KEY".into(), secret)]),
        Profile::new([ProfileEntry::new(
            "dsh-live-provider",
            "rsi.ai.provider.deepseek",
            json!({
                "deployment":"dsh-live", "endpoint":"https://api.deepseek.com", "protocol":"chat-completions",
                "credential":{"owner":"rsi.ai.provider.deepseek","slot":"default"},
                "language_models":{"deepseek-flash":{"context_window_tokens":100_000,"default_output_reserve_tokens":512,"max_output_reserve_tokens":1024}},
                "reasoning_efforts":{"deepseek-flash":{"supported":["off"],"default":"off"}}
            }),
        )]),
    )
}

fn prepared_output_default(body: &SessionFactBody) -> Option<u32> {
    let SessionFactBody::ModelIntent { snapshot, .. } = body else {
        return None;
    };
    let settings = snapshot
        .language_settings
        .as_ref()
        .expect("Language intent");
    Some(
        settings
            .profile
            .default_output_tokens()
            .expect("DeepSeek output default"),
    )
}

fn session_header(pin: &AgentCompositionPin, workspace: &Path) -> SessionHeader {
    SessionHeader::new_local(
        SessionId::new("dsh-live-todo").unwrap(),
        1,
        workspace.to_str().unwrap(),
        pin.preset_id().clone(),
        FrozenAgentSettings::new(
            "dsh-live",
            "deepseek",
            rsi_ai_protocol::ModelRef::new("dsh-live", "deepseek-flash").unwrap(),
            rsi_sandbox::SandboxMode::ReadOnly,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}

pub(super) async fn run(
    host: &rsi_host::RunningHost,
    pin: AgentCompositionPin,
    workspace: &Path,
    scenario: super::Scenario,
) {
    let began = std::time::Instant::now();
    let turns = host.lookup_local::<TurnServiceContract>().unwrap();
    let store = host.lookup_local::<SessionStoreContract>().unwrap();
    let header = session_header(&pin, workspace);
    let prompt = match scenario {
        super::Scenario::LargeEdit => "Rewrite this large standalone input",
        super::Scenario::Reject => "Reject this standalone input",
        super::Scenario::LiveTodo => {
            "Call todo_write exactly once with todos [{\"content\":\"Verify original DSH Todo through RSI\",\"status\":\"completed\"}]. Then reply with DSH_LIVE_OK. Do not call other tools."
        }
        super::Scenario::Activate => unreachable!(),
    };
    let receipt = turns
        .submit(SubmitTurn {
            session: SubmitSession::Fresh(PreparedFreshSession::new(header, pin).unwrap()),
            turn_id: TurnId::new("live-one").unwrap(),
            text: prompt.into(),
            model: None,
            reasoning_effort: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let outcome = await_outcome(turns.as_ref(), &receipt).await;
    let facts = store
        .read_facts(
            &receipt.session_id,
            0,
            128,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    if scenario == super::Scenario::LargeEdit {
        assert!(
            matches!(outcome, TurnOutcome::Failed { .. }),
            "no provider configured: {outcome:?}"
        );
        verify_large_edit(store.as_ref(), &receipt.session_id, &facts).await;
        return;
    }
    if scenario == super::Scenario::Reject {
        assert_eq!(outcome, TurnOutcome::Blocked);
        verify_rejection(store.as_ref(), &receipt.session_id, &facts).await;
        return;
    }
    assert_eq!(outcome, TurnOutcome::Completed);
    let output_defaults: Vec<_> = facts
        .iter()
        .filter_map(|fact| prepared_output_default(fact.body()))
        .collect();
    assert!(!output_defaults.is_empty());
    assert!(output_defaults.iter().all(|cap| *cap == 1024));
    let results: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ToolResult {
                effect_id, result, ..
            } => Some((fact, effect_id, result)),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1);
    let (fact, effect, result) = results[0];
    assert!(!result.is_error);
    let claimed_contexts = verify_claimed_contexts(&facts, effect, &result.additional_contexts);
    let request = DomainRequestId::new(effect.as_str()).unwrap();
    let control = store
        .read_domain_request(&receipt.session_id, &request)
        .await
        .unwrap()
        .expect("canonical Tool settlement");
    let rsi_agent_session_protocol::AgentControlRecordBody::DomainStateCommitted { commit } =
        control.body()
    else {
        panic!("domain receipt");
    };
    assert_eq!(commit.fact_span().unwrap().first_seq(), fact.seq());
    assert_eq!(commit.fact_span().unwrap().count(), 1);
    let journal = commit
        .updates()
        .iter()
        .find(|update| update.snapshot().identity().id() == "rsi.extension-journal")
        .expect("atomic original journal");
    let state: ExtensionJournal =
        serde_json::from_value(journal.snapshot().state().value().clone()).unwrap();
    state.validate().unwrap();
    assert!(!state.segments.is_empty());
    let report = json!({"model":"deepseek-flash","protocol":"chat-completions","elapsed_ms":began.elapsed().as_millis(),
        "outcome":"completed","tool_results":results.len(),"tool_result_seq":fact.seq(),"control_seq":control.seq(),
        "journal_segments":state.segments.len(),"claimed_tool_contexts":claimed_contexts,"request_output_defaults":output_defaults,"result":result});
    if let Ok(path) = std::env::var("RSI_DSH_LIVE_RECEIPT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    eprintln!("{report}");
}

async fn await_outcome(
    turns: &dyn rsi_agent_turn_protocol::TurnService,
    receipt: &rsi_agent_turn_protocol::SubmittedTurn,
) -> TurnOutcome {
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if let Some(outcome) = turns
                .outcome(&receipt.session_id, &receipt.turn_id)
                .await
                .unwrap()
            {
                break outcome;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("live Turn deadline")
}

fn verify_claimed_contexts<'a>(
    facts: &'a [rsi_agent_session_protocol::SessionFact],
    effect: &rsi_agent_session_protocol::EffectId,
    expected: &[rsi_tools_protocol::ToolContextMessage],
) -> Vec<(u64, &'a str)> {
    let mut ids = Vec::new();
    for fact in facts {
        if let SessionFactBody::ToolContextsClaimed { messages, .. } = fact.body() {
            ids.extend(
                messages
                    .iter()
                    .filter(|m| &m.effect == effect)
                    .map(|m| (fact.seq(), m.message.id.as_str())),
            );
        }
    }
    assert!(
        ids.iter()
            .map(|(_, id)| *id)
            .eq(expected.iter().map(|m| m.id.as_str())),
        "claimed tool contexts differ: {ids:?}"
    );
    ids
}

async fn verify_rejection(
    store: &dyn rsi_agent_store_protocol::SessionStore,
    session: &SessionId,
    facts: &[rsi_agent_session_protocol::SessionFact],
) {
    let mut decisions = facts.iter().filter_map(|fact| match fact.body() {
        SessionFactBody::StepInputsPrepared { decision, .. } => Some((fact.seq(), decision)),
        _ => None,
    });
    let (seq, decision) = decisions.next().expect("durable preset decision");
    assert!(decision.messages().is_none() && decisions.next().is_none());
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::ModelIntent { .. }))
    );
    let (control, journal) = decision_journal(store, session, seq).await;
    let views = &journal.checkpoint.value()["views"];
    assert_eq!(
        views["values"]["todos"][0]["content"],
        "dsh-live/deepseek-flash"
    );
    let report = json!({"outcome": "blocked", "model_intents": 0, "decision_seq": seq, "control_seq": control, "views": views});
    if let Ok(path) = std::env::var("RSI_DSH_REJECT_RECEIPT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    eprintln!("{report}");
}

async fn decision_journal(
    store: &dyn rsi_agent_store_protocol::SessionStore,
    session: &SessionId,
    seq: u64,
) -> (u64, ExtensionJournal) {
    let controls = store
        .read_controls(
            session,
            0,
            64,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap();
    controls
            .records
            .iter()
            .find_map(|row| {
                let rsi_agent_session_protocol::AgentControlRecordBody::DomainStateCommitted {
                    commit,
                } = row.body()
                else {
                    return None;
                };
                if commit.fact_span()?.last_seq() != seq {
                    return None;
                }
                let state = commit
                    .updates()
                    .iter()
                    .find(|update| update.snapshot().identity().id() == "rsi.extension-journal")?;
                Some((
                    row.seq(),
                    serde_json::from_value::<ExtensionJournal>(
                        state.snapshot().state().value().clone(),
                    )
                    .unwrap(),
                ))
            })
            .expect("input decision and staged plugin state must share a commit")
}

async fn verify_large_edit(
    store: &dyn rsi_agent_store_protocol::SessionStore,
    session: &SessionId,
    facts: &[rsi_agent_session_protocol::SessionFact],
) {
    use rsi_agent_session_protocol::{AgentMessageContent, PreparedInput};
    let (seq, entries) = facts
        .iter()
        .find_map(|fact| match fact.body() {
            SessionFactBody::StepInputsPrepared { decision, .. } => {
                Some((fact.seq(), decision.messages().unwrap()))
            }
            _ => None,
        })
        .expect("large input decision was durably published");
    let [PreparedInput::Replacement { claim, message }] = entries else {
        panic!("original input must be replaced")
    };
    let [AgentMessageContent::Text { text }] = message.content() else {
        panic!("original text block")
    };
    assert_eq!(text.as_str(), "界🚀".repeat(110_000));
    assert!(facts.iter().any(|f| f.seq() == claim.fact_seq(seq)
        && matches!(f.body(), SessionFactBody::TurnAccepted { .. })));
    let (control, journal) = decision_journal(store, session, seq).await;
    assert_eq!(
        journal.checkpoint.value()["views"]["values"]["todos"][0]["content"],
        "large edit observed"
    );
    eprintln!(
        "large edit bytes={} decision={} control={}",
        text.len(),
        seq,
        control
    );
}
