use super::*;
use rsi_api_protocol::{ApiError, ApiOutput, ByteBudget, CallOrigin};
use rsi_history_api::ConversationIdentity;
use rsi_history_api::{QueryScope, Reply, Request, Scope};
async fn call(running: &RunningRsi, request: Request) -> Result<Reply, ApiError> {
    call_as(running, CallOrigin::Local, request).await
}
async fn call_as(
    running: &RunningRsi,
    origin: CallOrigin,
    request: Request,
) -> Result<Reply, ApiError> {
    let spec = request.spec();
    let input = ByteBudget::default().encode(&request, spec.maximum_request_bytes)?;
    let ApiOutput::Reply(output) = running
        .api_dispatch()
        .unwrap()
        .admit(&spec.id, origin)?
        .invoke(input)
        .await?
    else {
        panic!("finite history reply")
    };
    let reply = serde_json::from_slice(output.json.as_bytes()).unwrap();
    rsi_history_api::validate_reply(&request, &reply)?;
    Ok(reply)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn discovery_limits_each_metadata_step_and_fences_continuations_by_caller_and_range() {
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let service = running.session_service().unwrap();
    let workspace = running
        .workspace_registry()
        .unwrap()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap();
    for number in 0..65 {
        let session = service
            .create(CreateSession {
                workspace_id: workspace.id.clone(),
                session_id: SessionId::new(format!("discovery-{number:03}")).unwrap(),
                agent_preset_id: None,
            })
            .await
            .unwrap();
        run_message_to_terminal(&session, &format!("discovery-input-{number}")).await;
    }
    let range = QueryScope::Workspace {
        workspace: workspace.id.clone(),
    };
    let Reply::Progress { progress, .. } = call(
        &running,
        Request::Discover {
            scope: range.clone(),
            after: None,
        },
    )
    .await
    .unwrap() else {
        panic!("progress")
    };
    assert!(progress.visible_sources > 0 && progress.visible_sources <= 64);
    assert!(!progress.discovery_complete);
    let token = progress.continuation.unwrap();
    let foreign = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: rsi_api_protocol::DeviceId::from_bytes([7; 16]),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    assert!(
        matches!(call_as(&running, foreign, Request::Discover { scope: range.clone(), after: Some(token.clone()) }).await, Err(ApiError::Invalid(reason)) if reason.contains("Stale discovery"))
    );
    assert!(
        matches!(call(&running, Request::Discover { scope: QueryScope::AccessibleHost, after: Some(token.clone()) }).await, Err(ApiError::Invalid(reason)) if reason.contains("Stale discovery"))
    );
    let Reply::Progress { progress, .. } = call(
        &running,
        Request::Discover {
            scope: range.clone(),
            after: Some(token.clone()),
        },
    )
    .await
    .unwrap() else {
        panic!("progress")
    };
    assert_eq!(progress.visible_sources, 65);
    assert!(progress.discovery_complete && progress.pending_sources > 0);
    assert!(
        matches!(call(&running, Request::Discover { scope: range.clone(), after: Some(token) }).await, Err(ApiError::Invalid(reason)) if reason.contains("Stale discovery"))
    );
    assert!(!progress.capacity_limited);
    let Reply::Matches {
        matches, progress, ..
    } = call(
        &running,
        Request::Query {
            scope: range,
            query: "inspect".into(),
            after: None,
        },
    )
    .await
    .unwrap()
    else {
        panic!("query across every discovered source")
    };
    assert!(!matches.is_empty() && matches.len() <= 64);
    assert_eq!(progress.visible_sources, 65);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one public workflow retains exact source identities through mutation, retirement and cold recovery"
)]
async fn history_search_rereads_original_freezes_into_an_unpublished_draft_and_rebuilds_corrupt_cache()
 {
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let profile = host_profile(&fixture);
    let running = RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &profile)
        .await
        .unwrap();
    let service = running.session_service().unwrap();
    let registry = running.workspace_registry().unwrap();
    let workspace = registry.get_or_create(&fixture.workspace).await.unwrap();
    let source = service
        .create(CreateSession {
            workspace_id: workspace.id.clone(),
            session_id: SessionId::new("history-source").unwrap(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    let target = service
        .create(CreateSession {
            workspace_id: workspace.id.clone(),
            session_id: SessionId::new("history-target").unwrap(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    run_message_to_terminal(&source, "history-first").await;
    let scope = Scope {
        workspace: workspace.id.clone(),
        conversation: ConversationIdentity::Native(
            source.header().await.unwrap().session_id().clone(),
        ),
    };
    let search = || Request::Search {
        scope: scope.clone(),
        query: "inspect".into(),
        after: None,
    };
    let Reply::Hits { coverage, hits, .. } = call(&running, search()).await.unwrap() else {
        panic!("hits")
    };
    assert!(hits.is_empty());
    assert!(coverage.has_more);
    assert_eq!(coverage.indexed_through, "0");
    let Reply::Coverage { coverage } = call(
        &running,
        Request::Advance {
            scope: scope.clone(),
        },
    )
    .await
    .unwrap() else {
        panic!("coverage")
    };
    assert!(!coverage.has_more);
    assert_eq!(coverage.omissions, "0");
    let Reply::Hits { hits, .. } = call(&running, search()).await.unwrap() else {
        panic!("hits")
    };
    assert!(!hits.is_empty());
    let hit = hits[0].clone();
    let Reply::Original { text, .. } = call(
        &running,
        Request::Read {
            scope: scope.clone(),
            hit: hit.clone(),
            offset: 0,
        },
    )
    .await
    .unwrap() else {
        panic!("original")
    };
    assert_eq!(text, "inspect workspace context");
    let target_id = target.header().await.unwrap().session_id().clone();
    let freeze = || Request::Freeze {
        scope: scope.clone(),
        hit: hit.clone(),
        target: target_id.clone(),
        start: 0,
        end: 7,
    };
    let Reply::Frozen { reference } = call(&running, freeze()).await.unwrap() else {
        panic!("frozen")
    };
    assert_eq!(reference.preview, "inspect");
    target
        .preview_reference(reference.clone(), 0, 65536)
        .await
        .unwrap();
    let mut forged = hit.clone();
    forged.original.text_sha256 = "a".repeat(64);
    assert!(
        call(
            &running,
            Request::Read {
                scope: scope.clone(),
                hit: forged,
                offset: 0
            }
        )
        .await
        .is_err()
    );
    let foreign_dir = fixture.temporary.path().join("foreign-history");
    std::fs::create_dir(&foreign_dir).unwrap();
    let foreign = registry.get_or_create(&foreign_dir).await.unwrap();
    let foreign_target = service
        .create(CreateSession {
            workspace_id: foreign.id.clone(),
            session_id: SessionId::new("history-cross-project-target").unwrap(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    let Reply::Frozen {
        reference: cross_project,
    } = call(
        &running,
        Request::Freeze {
            scope: scope.clone(),
            hit: hit.clone(),
            target: foreign_target.header().await.unwrap().session_id().clone(),
            start: 0,
            end: 7,
        },
    )
    .await
    .unwrap()
    else {
        panic!("cross-project capture")
    };
    assert_eq!(
        foreign_target
            .preview_reference(cross_project, 0, 65536)
            .await
            .unwrap()
            .text,
        "inspect"
    );
    let mut continuation = None;
    let suffix = foreign_target
        .capture_reference(source.header().await.unwrap().session_id().clone())
        .await
        .unwrap();
    assert!(
        foreign_target
            .preview_reference(suffix, 0, 65536)
            .await
            .unwrap()
            .text
            .contains("inspect")
    );
    for _ in 0..16 {
        let Reply::Progress { progress, .. } = call(
            &running,
            Request::Discover {
                scope: QueryScope::AccessibleHost,
                after: continuation,
            },
        )
        .await
        .unwrap() else {
            panic!("source discovery")
        };
        continuation = progress.continuation;
        if continuation.is_none() {
            break;
        }
    }
    assert!(
        continuation.is_none(),
        "finite native discovery and indexing terminate"
    );
    let Reply::Matches { matches, .. } = call(
        &running,
        Request::Query {
            scope: QueryScope::AccessibleHost,
            query: "inspect".into(),
            after: None,
        },
    )
    .await
    .unwrap() else {
        panic!("global candidates")
    };
    assert!(matches.iter().any(|m| m.scope == scope && m.hit == hit));
    let Reply::Progress { sources, next, .. } = call(
        &running,
        Request::Reset {
            scope: QueryScope::Workspace {
                workspace: workspace.id.clone(),
            },
            after: None,
        },
    )
    .await
    .unwrap() else {
        panic!("workspace reset")
    };
    assert!(next.is_none());
    assert!(sources.iter().all(|s| s.coverage.indexed_through == "0"));
    let Reply::Matches { matches, .. } = call(
        &running,
        Request::Query {
            scope: QueryScope::Workspace {
                workspace: workspace.id.clone(),
            },
            query: "inspect".into(),
            after: None,
        },
    )
    .await
    .unwrap() else {
        panic!("reset search")
    };
    assert!(matches.is_empty());
    assert!(
        call(
            &running,
            Request::Reset {
                scope: QueryScope::AccessibleHost,
                after: None
            }
        )
        .await
        .is_err()
    );
    call(
        &running,
        Request::Advance {
            scope: scope.clone(),
        },
    )
    .await
    .unwrap();
    assert!(
        call(
            &running,
            Request::Search {
                scope: Scope {
                    workspace: foreign.id,
                    conversation: scope.conversation.clone()
                },
                query: "inspect".into(),
                after: None
            }
        )
        .await
        .is_err()
    );
    run_message_to_terminal(&source, "history-growth").await;
    let Reply::Frozen { reference: again } = call(&running, freeze()).await.unwrap() else {
        panic!("frozen")
    };
    assert_eq!(reference, again);
    let Reply::Hits { coverage, .. } = call(&running, search()).await.unwrap() else {
        panic!("hits")
    };
    assert!(coverage.has_more);
    // Exercise an actual FTS candidate beyond the old latest-1024-Facts boundary.
    for turn in 0..100 {
        run_message_to_terminal(&source, &format!("history-old-hit-{turn}")).await;
    }
    let mut indexed = false;
    for _ in 0..32 {
        let Reply::Coverage { coverage } = call(
            &running,
            Request::Advance {
                scope: scope.clone(),
            },
        )
        .await
        .unwrap() else {
            panic!("coverage")
        };
        if !coverage.has_more {
            indexed = true;
            break;
        }
    }
    assert!(
        indexed,
        "finite source should finish indexing within 8192 records"
    );
    let Reply::Hits {
        coverage,
        hits,
        next,
    } = call(&running, search()).await.unwrap()
    else {
        panic!("hits")
    };
    assert!(
        rsi_history_api::decimal(&coverage.observed_through).unwrap()
            > hit.original.record.sequence + 1024
    );
    assert_eq!(hits.first().unwrap().original.record, hit.original.record);
    assert!(
        next.is_some(),
        "more than 64 matching original messages need a bounded page"
    );
    let Reply::Frozen { reference: old } = call(
        &running,
        Request::Freeze {
            scope: scope.clone(),
            hit: hits[0].clone(),
            target: target_id,
            start: 0,
            end: 7,
        },
    )
    .await
    .unwrap() else {
        panic!("frozen")
    };
    assert_eq!(old.preview, "inspect");
    assert_eq!(
        target.preview_reference(old, 0, 65536).await.unwrap().text,
        "inspect"
    );
    drop((source, target, foreign_target, service, registry));
    assert!(running.shutdown().await.is_clean());
    // Rebuilding this cache must leave durable Agent data untouched.
    let agent_db = fixture.paths.state().join("agent/sessions.sqlite3");
    let before = std::fs::read(&agent_db).unwrap();
    std::fs::write(
        fixture.paths.cache().join("history/v1/history.sqlite3"),
        b"not a SQLite cache",
    )
    .unwrap();
    let running = RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &profile)
        .await
        .unwrap();
    let Reply::Hits { coverage, hits, .. } = call(&running, search()).await.unwrap() else {
        panic!("hits")
    };
    assert_eq!(coverage.indexed_through, "0");
    assert!(hits.is_empty());
    assert!(running.shutdown().await.is_clean());
    assert_eq!(std::fs::read(agent_db).unwrap(), before);
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one public workflow retains exact source identities through mutation, retirement and cold recovery"
)]
async fn history_observed_epoch_replacement_rejects_old_candidates_but_keeps_frozen_bytes() {
    use rsi_acp_protocol::{
        observation::{ConversationId, Status},
        service::{ExternalConversations as _, Setup},
    };
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/rsi/acp/agent.py")
        .canonicalize()
        .unwrap();
    let config = serde_json::json!({"directory":fixture.paths.state().join("acp"),"endpoints":[{"id":"history-peer","enabled":true,"cwd":fixture.workspace,"sandbox":"danger-full-access","launch":{"program":"/usr/bin/python3","arguments":["-u",script,fixture.workspace,"normal"],"environment":{"FIXTURE_SECRET":{"kind":"literal","value":"private-fixture-secret"}}},"mcp_servers":[{"name":"private","launch":{"program":"/usr/bin/python3","environment":{"MCP_SECRET":{"kind":"literal","value":"private-fixture-secret"}}}}]}]});
    let mut document: toml::Value =
        toml::from_str(&std::fs::read_to_string(&fixture.profile).unwrap()).unwrap();
    document["steps"].as_array_mut().unwrap().push(
        toml::Value::try_from(
            serde_json::json!({"kind":"patch","target":"rsi-acp","config":config}),
        )
        .unwrap(),
    );
    std::fs::write(&fixture.profile, toml::to_string(&document).unwrap()).unwrap();
    let daemon = DaemonFixture::new(&fixture).await;
    let direct = rsi_acp_api::Client::new(daemon.connection.api_client()).unwrap();
    let client = rsi_history_api::Client::new(daemon.connection.api_client()).unwrap();
    let id = ConversationId::new("history-observed").unwrap();
    direct.start(id.clone(), "history-peer").await.unwrap();
    direct.submit(&id, "human evidence").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while direct.view(&id).await.unwrap().snapshot.status != Status::Completed {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let workspace = daemon
        .running
        .workspace_registry()
        .unwrap()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap();
    let target = daemon
        .running
        .session_service()
        .unwrap()
        .create(CreateSession {
            workspace_id: workspace.id.clone(),
            session_id: SessionId::new("observed-target").unwrap(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    let scope = Scope {
        workspace: workspace.id,
        conversation: ConversationIdentity::External(id.clone()),
    };
    client
        .call(Request::Advance {
            scope: scope.clone(),
        })
        .await
        .unwrap();
    let Reply::Hits { hits, .. } = client
        .call(Request::Search {
            scope: scope.clone(),
            query: "fixture-output".into(),
            after: None,
        })
        .await
        .unwrap()
    else {
        panic!("hits")
    };
    assert_eq!(hits.len(), 1);
    let hit = hits[0].clone();
    let Reply::Frozen { reference } = client
        .call(Request::Freeze {
            scope: scope.clone(),
            hit: hit.clone(),
            target: target.header().await.unwrap().session_id().clone(),
            start: 0,
            end: 7,
        })
        .await
        .unwrap()
    else {
        panic!("frozen")
    };
    assert_eq!(reference.preview, "fixture");
    let old = direct.view(&id).await.unwrap().snapshot.epoch;
    direct.close(&id).await.unwrap();
    let loaded = direct.reconnect(&id, Setup::Load).await.unwrap();
    assert!(loaded.epoch > old);
    assert!(
        client
            .call(Request::Read {
                scope: scope.clone(),
                hit,
                offset: 0
            })
            .await
            .is_err()
    );
    assert_eq!(
        target
            .preview_reference(reference, 0, 65536)
            .await
            .unwrap()
            .text,
        "fixture"
    );
    let Reply::Hits { coverage, hits, .. } = client
        .call(Request::Search {
            scope: scope.clone(),
            query: "replayed".into(),
            after: None,
        })
        .await
        .unwrap()
    else {
        panic!("hits")
    };
    assert_eq!(coverage.indexed_through, "0");
    assert!(coverage.has_more);
    assert!(hits.is_empty());
    client
        .call(Request::Advance {
            scope: scope.clone(),
        })
        .await
        .unwrap();
    let Reply::Hits { hits, .. } = client
        .call(Request::Search {
            scope,
            query: "replayed".into(),
            after: None,
        })
        .await
        .unwrap()
    else {
        panic!("hits")
    };
    assert_eq!(hits.len(), 1);
    direct.close(&id).await.unwrap();
    drop((target, client, direct));
    daemon.shutdown().await;
    provider.abort();
}
