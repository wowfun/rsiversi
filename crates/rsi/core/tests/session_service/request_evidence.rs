use super::*;
use rsi_agent_session_protocol::{EvidencePart, EvidenceSection, RequestEvidence};
use rsi_session_protocol::{EvidencePageContent, EvidenceRead};

async fn section(
    handle: &Arc<dyn rsi_session_protocol::SessionHandle>,
    seq: u64,
    section: EvidenceSection,
) -> String {
    let mut offset = 0;
    let mut text = String::new();
    loop {
        let request = EvidenceRead {
            intent_seq: seq,
            section,
            offset,
            maximum_bytes: 8192,
        };
        let page = handle.evidence(request.clone()).await.unwrap();
        page.validate(&request).unwrap();
        let EvidencePageContent::Available {
            start,
            text: part,
            more,
            ..
        } = page.content
        else {
            panic!("actual request evidence missing")
        };
        assert_eq!(start, offset);
        offset += u32::try_from(part.len()).unwrap();
        text.push_str(&part);
        if !more {
            break;
        }
        assert!(text.len() < 16 * 1024 * 1024);
    }
    text
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // One actual request is checked through paging, deduplication and cold restart.
async fn evidence_matches_actual_request_reuses_original_sections_and_survives_restart() {
    let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let captured = requests.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<serde_json::Value>| {
            captured.lock().unwrap().push(body);
            async { chat().await }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let session_id = SessionId::new("request-evidence").unwrap();
    let handle = running
        .session_service()
        .unwrap()
        .create(CreateSession {
            workspace_id: running
                .workspace_registry()
                .unwrap()
                .get_or_create(&fixture.workspace)
                .await
                .unwrap()
                .id,
            session_id: session_id.clone(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    run_message_to_terminal(&handle, "first-request").await;
    run_message_to_terminal(&handle, "second-request").await;
    let facts = handle.history_before(None, 128).await.unwrap();
    let intents = facts
        .facts
        .iter()
        .filter(|fact| matches!(fact.body(), SessionFactBody::ModelIntent { .. }))
        .collect::<Vec<_>>();
    assert_eq!(intents.len(), 2);
    assert_eq!(requests.lock().unwrap().len(), 2);
    let original_seq = intents[0].seq();
    let second_seq = intents[1].seq();
    let SessionFactBody::ModelIntent { evidence, .. } = intents[1].body() else {
        unreachable!()
    };
    let SessionFactBody::ModelIntent {
        evidence: original, ..
    } = intents[0].body()
    else {
        unreachable!()
    };
    for (kind, part) in evidence.parts() {
        let first = original.part(kind).unwrap();
        if part.sha256() == first.sha256() {
            assert!(matches!(part,EvidencePart::Reference {seq,..} if *seq == original_seq));
        } else {
            assert!(matches!(part, EvidencePart::Inline { .. }));
            if let (EvidencePart::Inline { text: a, .. }, EvidencePart::Inline { text: b, .. }) =
                (first, part)
            {
                let prefix = a.chars().zip(b.chars()).take_while(|(a, b)| a == b).count();
                eprintln!(
                    "changed {kind:?}: {:?} -> {:?}",
                    a.chars().skip(prefix).take(160).collect::<String>(),
                    b.chars().skip(prefix).take(160).collect::<String>()
                );
            }
        }
    }
    assert!(
        matches!(evidence.part(EvidenceSection::Tools),Some(EvidencePart::Reference {seq,..}) if *seq == original_seq)
    );
    let system = section(&handle, second_seq, EvidenceSection::System).await;
    let decoded: Vec<rsi_ai_protocol::Message> = serde_json::from_str(&system).unwrap();
    let actual = requests.lock().unwrap()[1].clone();
    let actual_system = actual["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| matches!(message["role"].as_str(), Some("system" | "developer")))
        .map(|message| {
            message["content"].as_str().map_or_else(
                || {
                    message["content"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|part| part["text"].as_str().unwrap())
                        .collect::<String>()
                },
                str::to_owned,
            )
        })
        .collect::<Vec<_>>();
    let captured_system = decoded
        .iter()
        .map(|message| {
            message
                .content()
                .iter()
                .map(|content| match content {
                    rsi_ai_protocol::MessageContent::Text { text } => text.as_str(),
                    _ => panic!("system should contain only text"),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(captured_system, actual_system);
    let tools = section(&handle, second_seq, EvidenceSection::Tools).await;
    assert!(tools.contains("todo_write"));
    assert!(tools.contains("directory_list"));
    let config = section(&handle, second_seq, EvidenceSection::Configuration).await;
    assert!(!config.contains(KEY));
    assert!(!system.contains("inspect workspace context"));
    let RequestEvidence::Available { manifest, .. } = evidence else {
        unreachable!()
    };
    assert!(!manifest.is_empty());
    assert!(
        handle
            .evidence(EvidenceRead {
                intent_seq: 0,
                section: EvidenceSection::System,
                offset: 0,
                maximum_bytes: 8192
            })
            .await
            .is_err()
    );
    assert!(running.shutdown().await.is_clean());
    drop(handle);
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = running
        .session_service()
        .unwrap()
        .attach(&session_id)
        .await
        .unwrap();
    assert_eq!(
        section(&handle, second_seq, EvidenceSection::System).await,
        system
    );
    run_message_to_terminal(&handle, "after-restart").await;
    let facts = handle.history_before(None, 128).await.unwrap();
    let last = facts
        .facts
        .iter()
        .rev()
        .find(|fact| matches!(fact.body(), SessionFactBody::ModelIntent { .. }))
        .unwrap();
    let SessionFactBody::ModelIntent { evidence, .. } = last.body() else {
        unreachable!()
    };
    assert!(
        matches!(evidence.part(EvidenceSection::Tools),Some(EvidencePart::Reference {seq,..}) if *seq == original_seq)
    );
    assert_eq!(requests.lock().unwrap().len(), 3);
    assert!(running.shutdown().await.is_clean());
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn optional_evidence_budget_fallback_still_dispatches_exactly_one_request() {
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { chat().await }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let settings = running.settings_access().unwrap();
    let current = settings.read("rsi.agent").await.unwrap();
    let mut value = current.value.clone();
    value["turn_budget"]["maximum_generated_record_bytes"] = serde_json::json!(16_384);
    value["system_prompt"] = serde_json::json!("fixture ".repeat(4096));
    settings
        .replace("rsi.agent", &current.version(), value)
        .await
        .unwrap();
    let handle = running
        .session_service()
        .unwrap()
        .create(CreateSession {
            workspace_id: running
                .workspace_registry()
                .unwrap()
                .get_or_create(&fixture.workspace)
                .await
                .unwrap()
                .id,
            session_id: SessionId::new("evidence-fallback").unwrap(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    run_message_to_terminal(&handle, "optional-evidence").await;
    let facts = handle.history_before(None, 128).await.unwrap();
    let intents = facts
        .facts
        .iter()
        .filter(|fact| matches!(fact.body(), SessionFactBody::ModelIntent { .. }))
        .collect::<Vec<_>>();
    assert_eq!(intents.len(), 1);
    assert!(matches!(
        intents[0].body(),
        SessionFactBody::ModelIntent {
            evidence: RequestEvidence::Unavailable {
                reason: rsi_agent_session_protocol::EvidenceUnavailable::Budget
            },
            ..
        }
    ));
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(facts.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::TurnTerminal {
            outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
            ..
        }
    )));
    let response = handle
        .evidence(EvidenceRead {
            intent_seq: intents[0].seq(),
            section: EvidenceSection::System,
            offset: 0,
            maximum_bytes: 8192,
        })
        .await
        .unwrap();
    assert!(matches!(
        response.content,
        EvidencePageContent::Unavailable {
            reason: rsi_agent_session_protocol::EvidenceUnavailable::Budget
        }
    ));
    assert!(running.shutdown().await.is_clean());
    server.abort();
    let _ = server.await;
}
