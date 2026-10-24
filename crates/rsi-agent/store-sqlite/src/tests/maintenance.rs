use super::*;
use rsi_agent_session_protocol::TurnOutcome;
use rsi_agent_store_protocol::{PurgeRequest, PurgeTarget, StoreMaintenance};

fn strict_policy(store: &SqliteStore) -> bool {
    store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .query_row(
            "SELECT COALESCE((SELECT strict FROM reference_policy WHERE id=1),0)",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn refused_maintenance_preserves_reference_policy() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let mut request = seed(&store, "refused-policy", 1).await;
    request.targets[0].fact_seq += 1;
    assert!(store.begin_purge(request).await.unwrap().is_err());
    assert!(!strict_policy(&store));
    drop(
        store
            .put_cas(Arc::from(b"unreferenced".as_slice()))
            .await
            .unwrap(),
    );
    let mut target = store.preview_cas_gc(1).await.unwrap().remove(0);
    target.generation += 1;
    assert!(matches!(
        store.collect_cas("stale-generation", target).await,
        Err(StoreError::Invalid(_))
    ));
    assert!(!strict_policy(&store));
    let target = store.preview_cas_gc(1).await.unwrap().remove(0);
    assert!(
        store
            .collect_cas("current-generation", target)
            .await
            .unwrap()
            .complete
    );
    assert!(strict_policy(&store));
}

#[tokio::test]
async fn completed_purge_retains_only_execution_locations_for_recovery() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let request = seed(&store, "minimal-binding", 1).await;
    store.begin_purge(request.clone()).await.unwrap().unwrap();
    assert!(
        store
            .purge_batch(request.operation_id.as_str())
            .await
            .unwrap()
            .complete
    );
    let (original, locations) = store
        .purge_binding(request.operation_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original, request);
    assert_eq!(
        serde_json::to_value(locations).unwrap(),
        serde_json::json!([{"kind":"local"}])
    );
    drop(store);
    let reopened = SqliteStore::open(root.path()).unwrap();
    assert_eq!(
        serde_json::to_value(
            reopened
                .purge_binding(request.operation_id.as_str())
                .await
                .unwrap()
                .unwrap()
                .1
        )
        .unwrap(),
        serde_json::json!([{"kind":"local"}])
    );
}

#[tokio::test]
async fn corrupted_retirement_location_binding_refuses_recovery() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let request = seed(&store, "corrupt-binding", 1).await;
    store.begin_purge(request.clone()).await.unwrap().unwrap();
    store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .execute(
            "UPDATE purge_operations SET locations_json='[]' WHERE operation_id=?1",
            [request.operation_id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        store.purge_binding(request.operation_id.as_str()).await,
        Err(StoreError::Corrupt(_))
    ));
    assert!(matches!(
        store.purge_batch(request.operation_id.as_str()).await,
        Err(StoreError::Corrupt(_))
    ));
}

#[tokio::test]
async fn reference_proof_survives_a_known_refusal_without_writes() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    seed(&store, "proof-refusal", 1).await;
    store.validate_references().await.unwrap();
    let changes = store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .total_changes();
    let rejected: Result<()> = store
        .with_writer(|_| Err(StoreError::NotFound("missing".into())))
        .await;
    assert!(matches!(rejected, Err(StoreError::NotFound(_))));
    store.validate_references().await.unwrap();
    assert_eq!(store.inner.reference_audits.load(Ordering::Relaxed), 1);
    assert_eq!(
        store
            .inner
            .connections
            .writer
            .lock()
            .unwrap()
            .total_changes(),
        changes
    );
}

#[tokio::test]
async fn complete_non_strict_writes_preserve_the_reference_audit() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    seed(&store, "first-proof", 1).await;
    store.validate_references().await.unwrap();
    drop(
        store
            .put_cas(Arc::from(b"complete-only".as_slice()))
            .await
            .unwrap(),
    );
    seed(&store, "next-proof", 1).await;
    store.validate_references().await.unwrap();
    assert!(!strict_policy(&store));
    assert_eq!(store.inner.reference_audits.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn warm_integrity_proof_does_not_dispatch_an_extra_reader_after_purge() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let retired = seed(&store, "retired-proof", 1).await;
    let live = seed(&store, "live-proof", 1).await;
    store
        .validate_session(&live.targets[0].session_id)
        .await
        .unwrap();
    store.begin_purge(retired).await.unwrap().unwrap();
    let _reader = store.inner.reader_admission.acquire().await.unwrap();
    tokio::time::timeout(
        Duration::from_millis(200),
        store.ensure_session_validated(&live.targets[0].session_id),
    )
    .await
    .expect("warm integrity proof must remain in-process")
    .unwrap();
}

#[tokio::test]
async fn cached_integrity_never_exposes_retiring_secondary_rows() {
    use rsi_agent_session_protocol::{
        DomainRequestId, ProgramRunId, QueueOperationId, QueueSlotId,
    };
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let request = seed(&store, "retiring-reads", 160).await;
    let id = &request.targets[0].session_id;
    store.validate_session(id).await.unwrap();
    store.begin_purge(request.clone()).await.unwrap().unwrap();
    assert!(
        !store
            .purge_batch(request.operation_id.as_str())
            .await
            .unwrap()
            .complete
    );
    let turn = TurnId::new("turn-0").unwrap();
    macro_rules! unavailable {
        ($read:expr) => {
            assert!(
                matches!($read.await, Err(StoreError::NotFound(_))),
                stringify!($read)
            );
        };
    }
    unavailable!(store.program_run_for_creator(id, &turn));
    unavailable!(store.read_domain_request(id, &DomainRequestId::new("request").unwrap()));
    unavailable!(store.read_queue_slot(id, &QueueSlotId::new("slot").unwrap()));
    unavailable!(store.read_queue_mutation(id, &QueueOperationId::new("mutation").unwrap()));
    unavailable!(store.queue_mutation_count(id));
    unavailable!(store.active_activation(id));
    unavailable!(store.resolve_fork_boundary(id, &turn, ForkTurnSelection::None));
    unavailable!(store.read_program_records_after(id, &ProgramRunId::new("run").unwrap(), 0));
    unavailable!(store.list_program_history(id, 100, None, 10));
}

#[test]
fn pending_recovery_uses_partial_indexes() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let connection = store.inner.connections.writer.lock().unwrap();
    for (table, predicate, index) in [
        ("purge_operations", "complete=0", "purge_operations_pending"),
        ("cas_deletions", "state='deleting'", "cas_deletions_pending"),
    ] {
        let plan = connection.prepare(&format!("EXPLAIN QUERY PLAN SELECT operation_id FROM {table} WHERE {predicate} AND operation_id>?1 ORDER BY operation_id LIMIT 256")).unwrap()
            .query_map([""], |row| row.get::<_, String>(3)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap().join("\n");
        assert!(plan.contains(index), "{plan}");
    }
}

#[tokio::test]
async fn garbage_collection_disposes_corrupt_unreferenced_bytes() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    for (index, damaged) in [b"different".as_slice(), b"x".as_slice()]
        .into_iter()
        .enumerate()
    {
        drop(
            store
                .put_cas(Arc::from(format!("original-{index}").into_bytes()))
                .await
                .unwrap(),
        );
        let target = store.preview_cas_gc(1).await.unwrap().remove(0);
        let path = root.path().join("cas").join(&target.object.sha256);
        std::fs::write(&path, damaged).unwrap();
        let operation = format!("damaged-{index}");
        assert!(
            store
                .collect_cas(&operation, target.clone())
                .await
                .unwrap()
                .complete
        );
        assert!(!path.exists());
        assert!(
            store
                .collect_cas(&operation, target)
                .await
                .unwrap()
                .complete
        );
    }
    assert!(
        store
            .pending_cas_collections(None, 256)
            .await
            .unwrap()
            .is_empty()
    );
    drop(store);
    SqliteStore::verify(root.path()).unwrap();
}

#[tokio::test]
async fn coverage_authenticates_stored_json_bytes_including_whitespace() {
    for (table, column) in [
        ("sessions", "header_json"),
        ("facts", "fact_json"),
        ("agent_controls", "control_json"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let request = seed(&store, "encoded-coverage", 1).await;
        let connection = store.inner.connections.writer.lock().unwrap();
        let changed = connection.execute(&format!("UPDATE {table} SET {column}={column} || ' ' WHERE session_id='encoded-coverage'"), []).unwrap();
        // This seed has no controls; Fact and Header bytes exercise the source contract.
        if table != "agent_controls" {
            assert!(changed > 0);
        }
        drop(connection);
        if changed > 0 {
            assert!(
                matches!(
                    store.preview_purge(&request).await,
                    Err(StoreError::Corrupt(_))
                ),
                "{table}"
            );
        }
    }
}

#[tokio::test]
async fn rolled_back_writes_preserve_coverage_but_committed_errors_do_not() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    seed(&store, "rollback-proof", 1).await;
    store.validate_references().await.unwrap();
    for _ in 0..3 {
        let result: Result<()> = store
            .with_writer(|connection| {
                let tx = connection.transaction().map_err(sql_error)?;
                tx.execute("UPDATE reference_records SET sha256='invalid'", [])
                    .map_err(sql_error)?;
                Err(StoreError::Invalid("rollback after statements".into()))
            })
            .await;
        assert!(matches!(result, Err(StoreError::Invalid(_))));
        store.validate_references().await.unwrap();
    }
    assert_eq!(store.inner.reference_audits.load(Ordering::Relaxed), 1);
    let result: Result<()> = store
        .with_writer(|connection| {
            connection
                .execute("UPDATE reference_records SET sha256='invalid'", [])
                .map_err(sql_error)?;
            Err(StoreError::Io("acknowledgement lost".into()))
        })
        .await;
    assert!(result.is_err());
    assert!(matches!(
        store.validate_references().await,
        Err(StoreError::Corrupt(_))
    ));
}

#[tokio::test]
async fn proof_stamp_cannot_skip_an_external_commit_between_entry_observations() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    seed(&store, "stamp-gap", 1).await;
    store.validate_references().await.unwrap();
    {
        let connection = store.inner.connections.writer.lock().unwrap();
        reference_validation::check_cached(&store.inner, &connection).unwrap();
        let external = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
        external
            .execute("UPDATE reference_records SET sha256='invalid'", [])
            .unwrap();
        let _newer_observation =
            reference_validation::Stamp::read(&store.inner, &connection).unwrap();
        reference_validation::after_write(&store.inner, &connection, true).unwrap();
    }
    assert!(matches!(
        store.preview_cas_gc(1).await,
        Err(StoreError::Corrupt(_))
    ));
}

#[tokio::test]
async fn reference_audit_is_not_reused_after_reopen_or_external_database_changes() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    seed(&store, "external-change", 1).await;
    assert!(store.preview_cas_gc(1).await.unwrap().is_empty());
    assert_eq!(store.inner.reference_audits.load(Ordering::Relaxed), 1);
    drop(store);

    let store = SqliteStore::open(root.path()).unwrap();
    assert_eq!(store.inner.reference_audits.load(Ordering::Relaxed), 0);
    assert!(store.preview_cas_gc(1).await.unwrap().is_empty());
    assert_eq!(store.inner.reference_audits.load(Ordering::Relaxed), 1);
    let external = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    assert_eq!(
        external
            .execute(
                "UPDATE reference_records SET sha256=?1 WHERE session_id='external-change' AND stream='header'",
                ["a".repeat(64)],
            )
            .unwrap(),
        1
    );
    assert!(matches!(
        store.preview_cas_gc(1).await,
        Err(StoreError::Corrupt(_))
    ));
    assert_eq!(store.inner.reference_audits.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn reference_audit_is_shared_across_gc_and_strict_appends_but_not_untracked_changes() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    seed(&store, "surviving-history", 30).await;
    for i in 0..16 {
        drop(
            store
                .put_cas(Arc::from(format!("unreferenced-{i}").into_bytes()))
                .await
                .unwrap(),
        );
    }
    // Holding the cold-validation lane must not block a maintenance audit.
    let cold = store.inner.validation_admission.acquire().await.unwrap();
    let targets = tokio::time::timeout(Duration::from_secs(3), store.preview_cas_gc(256))
        .await
        .unwrap()
        .unwrap();
    drop(cold);
    assert_eq!(targets.len(), 16);
    for (i, target) in targets.into_iter().enumerate() {
        assert!(
            store
                .collect_cas(&format!("collect-{i}"), target)
                .await
                .unwrap()
                .complete
        );
    }
    seed(&store, "typed-append", 1).await;
    assert!(store.preview_cas_gc(256).await.unwrap().is_empty());
    assert_eq!(store.inner.reference_audits.load(Ordering::Relaxed), 1);
    store.inner.connections.writer.lock().unwrap().execute("UPDATE reference_records SET sha256=?1 WHERE session_id='typed-append' AND stream='header'", ["a".repeat(64)]).unwrap();
    assert!(matches!(
        store.preview_cas_gc(256).await,
        Err(StoreError::Corrupt(_))
    ));
    assert_eq!(store.inner.reference_audits.load(Ordering::Relaxed), 2);
}
#[test]
fn purge_selection_uses_session_and_operation_indexes_without_a_global_sort() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let connection = store.inner.connections.writer.lock().unwrap();
    let sql = crate::maintenance::deletion_query("facts", "seq", "length(fact_json)");
    let mut statement = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap();
    let plan = statement
        .query_map(params!["operation", 512], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join("\n");
    assert!(plan.contains("SEARCH facts USING INDEX"), "{plan}");
    assert!(plan.contains("operation_tombstones"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
}

#[tokio::test]
async fn maximum_checkpoint_purges_in_bounded_batches_and_offline_maintenance_recovers() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let mut request = seed(&store, &"a".repeat(256), 1).await;
    request.operation_id =
        rsi_agent_store_protocol::MaintenanceOperationId::new("checkpoint-max").unwrap();
    request
        .targets
        .extend(seed(&store, "checkpoint-b", 1).await.targets);
    for (index, target) in request.targets.iter().enumerate() {
        let facts = store.read_facts(&target.session_id, 0, 10).await.unwrap();
        let length = if index == 0 {
            MAXIMUM_CONTEXT_CHECKPOINT_BYTES
        } else {
            128 * 1024
        };
        store
            .write_context_checkpoint(WriteContextCheckpoint {
                session_id: target.session_id.clone(),
                expected_durable_seq: 2,
                checkpoint: StoredContextCheckpoint {
                    header_fingerprint: target.header_sha256.clone(),
                    through_seq: 2,
                    fact_prefix_sha256: rsi_agent_session_protocol::fact_prefix_sha256(
                        &facts.facts,
                    )
                    .unwrap(),
                    bytes: rsi_api_protocol::ByteBudget::new(length)
                        .unwrap()
                        .copy(&vec![7; length])
                        .unwrap(),
                },
            })
            .await
            .unwrap();
    }
    let mut receipt = store.begin_purge(request.clone()).await.unwrap().unwrap();
    let mut batches = 0;
    while !receipt.complete {
        let next = store
            .purge_batch(request.operation_id.as_str())
            .await
            .unwrap();
        assert!(next.deleted_records > receipt.deleted_records);
        assert!(next.deleted_records - receipt.deleted_records <= 512);
        assert!(
            next.deleted_payload_bytes - receipt.deleted_payload_bytes <= (64 * 1024 * 1024 + 384)
        );
        receipt = next;
        batches += 1;
        assert!(batches < 5);
    }
    assert!(
        batches >= 2,
        "checkpoint payloads must split across transactions"
    );
    drop(store);
    SqliteStore::verify(root.path()).unwrap();
    SqliteStore::compact(root.path()).unwrap();
}

async fn seed(store: &dyn SessionStore, id: &str, turns: u64) -> PurgeRequest {
    seed_header(store, test_header(id), turns).await
}
async fn seed_header(store: &dyn SessionStore, header: SessionHeader, turns: u64) -> PurgeRequest {
    for index in 0..turns {
        let turn = TurnId::new(format!("turn-{index}")).unwrap();
        let accepted = SessionFact::new(
            index * 2 + 1,
            index * 2 + 1,
            SessionFactBody::TurnAccepted {
                turn_id: turn.clone(),
                text: "maintenance fixture".into(),
                model: None,
                reasoning_effort: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap();
        let terminal = SessionFact::new(
            index * 2 + 2,
            index * 2 + 2,
            SessionFactBody::TurnTerminal {
                turn_id: turn.clone(),
                outcome: TurnOutcome::Completed,
                result: None,
            },
        )
        .unwrap();
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: header.session_id().clone(),
                    expected_fact_seq: index * 2,
                    expected_control_seq: index,
                    header: (index == 0).then(|| header.clone()),
                    facts: vec![Arc::new(accepted), Arc::new(terminal)],
                    controls: vec![
                        AgentControlRecord::new(
                            index + 1,
                            index * 2 + 2,
                            AgentControlRecordBody::TurnBoundaryRecorded {
                                turn_id: turn,
                                terminal_fact_seq: index * 2 + 2,
                            },
                        )
                        .unwrap(),
                    ],
                }],
                required_active_activations: vec![],
                quiescent_descendants_of: None,
            })
            .await
            .unwrap();
    }
    PurgeRequest {
        operation_id: rsi_agent_store_protocol::MaintenanceOperationId::new(format!(
            "purge-{:x}",
            Sha256::digest(header.session_id().as_str())
        ))
        .unwrap(),
        targets: vec![PurgeTarget {
            session_id: header.session_id().clone(),
            header_sha256: header.fingerprint().unwrap(),
            fact_seq: turns * 2,
            control_seq: turns,
        }],
    }
}
#[tokio::test]
async fn purge_restarts_between_bounded_batches_and_rejects_old_proofs_and_resurrection() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let request = seed(&store, "bounded-purge", 160).await;
    let id = request.targets[0].session_id.clone();
    let pin = store.prepare_session(&id).await.unwrap();
    assert!(
        store
            .preview_purge(&request)
            .await
            .unwrap()
            .blockers
            .is_empty()
    );
    assert!(
        !store
            .begin_purge(request.clone())
            .await
            .unwrap()
            .unwrap()
            .complete
    );
    assert!(matches!(
        store.header(&id).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.prepare_session(&id).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store
            .contains_model_intent(
                &id,
                &TurnId::new("turn-0").unwrap(),
                &rsi_agent_session_protocol::EffectId::new("model").unwrap()
            )
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        store
            .session_activity_summaries(std::slice::from_ref(&id))
            .await
            .unwrap(),
        vec![None]
    );
    let mut reused = request.clone();
    reused.targets[0].fact_seq += 1;
    assert!(matches!(
        store.begin_purge(reused).await,
        Err(StoreError::Invalid(_))
    ));
    let first = store
        .purge_batch(request.operation_id.as_str())
        .await
        .unwrap();
    assert_eq!(first.deleted_records, 512);
    assert!(!first.complete);
    drop(pin);
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    assert_eq!(
        store.begin_purge(request.clone()).await.unwrap().unwrap(),
        first
    );
    let final_receipt = store
        .purge_batch(request.operation_id.as_str())
        .await
        .unwrap();
    assert!(final_receipt.complete);
    assert_eq!(
        store
            .purge_batch(request.operation_id.as_str())
            .await
            .unwrap(),
        final_receipt
    );
    let restored = store
        .append(AppendBatch {
            session_id: id.clone(),
            expected_seq: 0,
            header: Some(test_header(id.as_str())),
            facts: vec![Arc::new(test_fact(1))],
        })
        .await;
    assert!(matches!(restored, Err(StoreError::NotFound(_))));
    assert!(matches!(
        store.header(&id).await,
        Err(StoreError::NotFound(_))
    ));
}
#[tokio::test]
async fn purge_refuses_changed_versions_and_extra_reference_edges_before_retirement() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let request = seed(&store, "changed-purge", 1).await;
    assert!(
        store
            .preview_purge(&request)
            .await
            .unwrap()
            .blockers
            .is_empty()
    );
    let strict: bool = store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .query_row(
            "SELECT COALESCE((SELECT strict FROM reference_policy WHERE id=1),0)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!strict, "preview must not change durable reference policy");
    let mut changed = request.clone();
    changed.targets[0].fact_seq += 1;
    assert!(
        !store
            .preview_purge(&changed)
            .await
            .unwrap()
            .blockers
            .is_empty()
    );
    store.inner.connections.writer.lock().unwrap().execute("INSERT INTO reference_edges(session_id,stream,seq,target_kind,target_id,target_bytes) VALUES('changed-purge','header',0,'session','invented',0)",[]).unwrap();
    assert!(matches!(
        store.begin_purge(request.clone()).await,
        Err(StoreError::Corrupt(_))
    ));
    assert!(
        store
            .purge_status(request.operation_id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.header(&request.targets[0].session_id).await.is_ok());
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One fixture checks independent blockers and absence of retirement side effects."
)]
async fn purge_blockers_are_exact_and_refusals_do_not_latch_cached_proof_checks() {
    use rsi_agent_session_protocol::{
        AgentPath, ForkOrigin, ForkTurnSelection, SessionProtectionScope,
    };
    use rsi_agent_store_protocol::PurgeBlocker;
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let parent = seed(&store, "parent", 1).await;
    let child_header = test_header("child")
        .with_fork_origin(ForkOrigin {
            parent_session_id: parent.targets[0].session_id.clone(),
            root_session_id: parent.targets[0].session_id.clone(),
            path: AgentPath::new(vec![1]).unwrap(),
            task_name: "child".into(),
            parent_header_fingerprint: parent.targets[0].header_sha256.clone(),
            invoking_turn_id: TurnId::new("turn-0").unwrap(),
            resolved_after_seq: 0,
            resolved_terminal_seq: 0,
            terminal_prefix_sha256: "0".repeat(64),
            resolved_terminal_control_seq: 0,
            terminal_control_prefix_sha256: "0".repeat(64),
            requested_turns: ForkTurnSelection::None,
            effective_turns: 0,
        })
        .unwrap();
    let child = seed_header(&store, child_header, 1).await;
    let referenced = vec![PurgeBlocker::Referenced {
        source: Some(child.targets[0].session_id.clone()),
        target: parent.targets[0].session_id.clone(),
    }];
    assert_eq!(
        store.preview_purge(&parent).await.unwrap().blockers,
        referenced
    );
    assert_eq!(
        store
            .begin_purge(parent.clone())
            .await
            .unwrap()
            .unwrap_err()
            .blockers,
        referenced
    );
    let protected = seed_header(
        &store,
        test_header("protected")
            .with_protection(SessionProtectionScope::new("fixture", "owner").unwrap())
            .unwrap(),
        1,
    )
    .await;
    assert_eq!(
        store
            .begin_purge(protected.clone())
            .await
            .unwrap()
            .unwrap_err()
            .blockers,
        vec![PurgeBlocker::ProductOwned {
            session_id: protected.targets[0].session_id.clone()
        }]
    );
    let header = test_header("busy");
    store
        .append(AppendBatch {
            session_id: header.session_id().clone(),
            expected_seq: 0,
            header: Some(header.clone()),
            facts: vec![Arc::new(test_fact(1))],
        })
        .await
        .unwrap();
    let busy = PurgeRequest {
        operation_id: rsi_agent_store_protocol::MaintenanceOperationId::new("busy-purge").unwrap(),
        targets: vec![PurgeTarget {
            session_id: header.session_id().clone(),
            header_sha256: header.fingerprint().unwrap(),
            fact_seq: 1,
            control_seq: 0,
        }],
    };
    assert_eq!(
        store.begin_purge(busy).await.unwrap().unwrap_err().blockers,
        vec![PurgeBlocker::Busy {
            session_id: header.session_id().clone()
        }]
    );
    assert!(
        !store
            .inner
            .retirement_started
            .load(std::sync::atomic::Ordering::Acquire)
    );
    assert!(
        store
            .purge_status(parent.operation_id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    let mut together = parent;
    together.targets.extend(child.targets);
    together
        .targets
        .sort_by(|a, b| a.session_id.cmp(&b.session_id));
    assert!(store.begin_purge(together).await.unwrap().is_ok());
    assert!(
        store
            .inner
            .retirement_started
            .load(std::sync::atomic::Ordering::Acquire)
    );
}

#[tokio::test]
async fn cas_collection_respects_publication_ownership_and_old_receipts_cannot_delete_republication()
 {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let bytes: Arc<[u8]> = Arc::from(b"retained publication".as_slice());
    let prepared = store.put_cas(bytes.clone()).await.unwrap();
    let target = store.preview_cas_gc(8).await.unwrap().remove(0);
    assert_eq!(target.object, *prepared.reference());
    assert!(matches!(
        store.collect_cas("collect-first", target.clone()).await,
        Err(StoreError::ReadCapacity)
    ));
    let read = store.retain_cas(&target.object).await.unwrap();
    drop(prepared);
    assert!(matches!(
        store.collect_cas("collect-first", target.clone()).await,
        Err(StoreError::ReadCapacity)
    ));
    drop(read);
    let receipt = store
        .collect_cas("collect-first", target.clone())
        .await
        .unwrap();
    assert!(receipt.complete);
    assert!(!root.path().join("cas").join(&target.object.sha256).exists());
    assert!(matches!(
        store.retain_cas(&target.object).await,
        Err(StoreError::NotFound(_))
    ));
    let next = store.put_cas(bytes).await.unwrap();
    let next_target = store.preview_cas_gc(8).await.unwrap().remove(0);
    assert!(next_target.generation > target.generation);
    assert_eq!(
        store.collect_cas("collect-first", target).await.unwrap(),
        receipt
    );
    assert!(store.retain_cas(&next).await.is_ok());
}
#[tokio::test]
async fn interrupted_cas_deletion_rejects_readers_and_publishers_and_resumes_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let bytes: Arc<[u8]> = Arc::from(b"deleting".as_slice());
    let prepared = store.put_cas(bytes.clone()).await.unwrap();
    drop(prepared);
    let target = store.preview_cas_gc(8).await.unwrap().remove(0);
    store.inner.connections.writer.lock().unwrap().execute("INSERT INTO cas_deletions(operation_id,sha256,byte_len,generation,state) VALUES('interrupted',?1,?2,?3,'deleting')",params![target.object.sha256,i64::try_from(target.object.byte_len).unwrap(),i64::try_from(target.generation).unwrap()]).unwrap();
    seed(&store, "unrelated-gc", 1).await;
    store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .execute(
            "UPDATE reference_records SET complete=0 WHERE session_id='unrelated-gc'",
            [],
        )
        .unwrap();
    assert!(matches!(
        store.preview_cas_gc(8).await,
        Err(StoreError::Unsupported(_))
    ));
    assert!(matches!(
        store.retain_cas(&target.object).await,
        Err(StoreError::ReadCapacity)
    ));
    assert!(matches!(
        store.put_cas(bytes).await,
        Err(StoreError::ReadCapacity)
    ));
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    assert!(
        store
            .collect_cas("interrupted", target)
            .await
            .unwrap()
            .complete
    );
}
#[tokio::test]
async fn interrupted_purge_revalidates_surviving_canonical_reference_records() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let request = seed(&store, "retiring-corruption", 160).await;
    store.begin_purge(request.clone()).await.unwrap().unwrap();
    let receipt = store
        .purge_batch(request.operation_id.as_str())
        .await
        .unwrap();
    assert!(!receipt.complete);
    store.inner.connections.writer.lock().unwrap().execute("UPDATE reference_records SET sha256=?1 WHERE session_id='retiring-corruption' AND stream='header'",["a".repeat(64)]).unwrap();
    assert!(matches!(
        store.purge_batch(request.operation_id.as_str()).await,
        Err(StoreError::Corrupt(_))
    ));
    assert_eq!(
        store
            .purge_status(request.operation_id.as_str())
            .await
            .unwrap()
            .unwrap(),
        receipt
    );
}

#[tokio::test]
async fn reference_rebuild_and_compact_are_explicit_and_exclusive() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let request = seed(&store, "rebuild", 12).await;
    assert!(matches!(
        SqliteStore::compact(root.path()),
        Err(StoreError::WriterLocked)
    ));
    store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .execute("UPDATE reference_records SET complete=0", [])
        .unwrap();
    assert!(matches!(
        store.preview_purge(&request).await,
        Err(StoreError::Unsupported(_))
    ));
    drop(store);
    assert_eq!(
        SqliteStore::rebuild_reference_index(
            root.path(),
            &rsi_agent_store_protocol::ReferenceCodecs::default()
        )
        .unwrap(),
        37
    );
    let store = SqliteStore::open(root.path()).unwrap();
    assert!(
        store
            .preview_purge(&request)
            .await
            .unwrap()
            .blockers
            .is_empty()
    );
    store.begin_purge(request.clone()).await.unwrap().unwrap();
    assert!(
        store
            .purge_batch(request.operation_id.as_str())
            .await
            .unwrap()
            .complete
    );
    drop(store);
    let compact = SqliteStore::compact(root.path()).unwrap();
    assert!(compact.after_bytes <= compact.before_bytes);
    SqliteStore::verify(root.path()).unwrap();
}

#[tokio::test]
async fn generated_memory_sqlite_paging_and_reopen_agree() {
    let mut random = 0x7465_726d_696e_616c_u64;
    for case in 0..32 {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let turns = 1 + (random >> 32) % 16;
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let memory = rsi_agent_testkit::MemoryStore::default();
        let id = format!("generated-{case}");
        let request = seed(&store, &id, turns).await;
        seed(&memory, &id, turns).await;
        drop(store);
        let store = SqliteStore::open(root.path()).unwrap();
        let id = &request.targets[0].session_id;
        assert_eq!(
            store.header(id).await.unwrap(),
            memory.header(id).await.unwrap()
        );
        assert_eq!(
            store.read_watermarks(id).await.unwrap(),
            memory.read_watermarks(id).await.unwrap()
        );
        for after in [0, turns / 2, turns, turns * 2] {
            let size = 1 + usize::try_from(random % 7).unwrap();
            assert_eq!(
                store.read_facts(id, after, size).await.unwrap(),
                memory.read_facts(id, after, size).await.unwrap()
            );
            assert_eq!(
                store
                    .read_controls(id, after.min(turns), size)
                    .await
                    .unwrap(),
                memory
                    .read_controls(id, after.min(turns), size)
                    .await
                    .unwrap()
            );
        }
    }
}
#[tokio::test]
async fn cas_unlink_failure_keeps_deleting_and_missing_namespace_resume_is_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let prepared = store
        .put_cas(Arc::from(b"gc-fault".as_slice()))
        .await
        .unwrap();
    drop(prepared);
    let target = store.preview_cas_gc(1).await.unwrap().remove(0);
    let path = root.path().join("cas").join(&target.object.sha256);
    // A non-file entry cannot be turned into a successful deletion/publication.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(store.collect_cas("fault", target.clone()).await.is_err());
    assert!(
        !store
            .cas_gc_status("fault")
            .await
            .unwrap()
            .unwrap()
            .complete
    );
    assert!(store.retain_cas(&target.object).await.is_err());
    // Crash after unlink but before metadata acknowledgement is also resumable.
    std::fs::remove_dir(path).unwrap();
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    assert!(store.collect_cas("fault", target).await.unwrap().complete);
}

#[tokio::test]
async fn mixed_program_references_validate_rebuild_and_preserve_cas_roots() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let header = test_header("mixed-program");
    let (id, _) = Box::pin(rsi_agent_testkit::assert_program_store_contract(
        &store,
        &header,
        &test_fact(1),
    ))
    .await;
    let header = store.header(&id).await.unwrap();
    let mixed: bool = store.inner.connections.writer.lock().unwrap().query_row(
        "SELECT EXISTS(SELECT 1 FROM reference_edges GROUP BY session_id,stream,seq HAVING COUNT(DISTINCT target_kind)=2)",
        [], |row| row.get(0),
    ).unwrap();
    assert!(
        mixed,
        "fixture must contain both Session and CAS edges on one record"
    );
    assert!(store.preview_cas_gc(256).await.unwrap().is_empty());
    let watermarks = store.read_watermarks(header.session_id()).await.unwrap();
    let request = PurgeRequest {
        operation_id: rsi_agent_store_protocol::MaintenanceOperationId::new("mixed-preview")
            .unwrap(),
        targets: vec![PurgeTarget {
            session_id: header.session_id().clone(),
            header_sha256: header.fingerprint().unwrap(),
            fact_seq: watermarks.durable_fact_seq,
            control_seq: watermarks.durable_control_seq,
        }],
    };
    assert!(store.preview_purge(&request).await.unwrap().blockers.iter().any(|blocker| matches!(blocker, rsi_agent_store_protocol::PurgeBlocker::Busy { session_id } if session_id == &id)));
    drop(store);
    SqliteStore::verify(root.path()).unwrap();
    assert!(
        SqliteStore::rebuild_reference_index(
            root.path(),
            &rsi_agent_store_protocol::ReferenceCodecs::default()
        )
        .unwrap()
            > 0
    );
    let store = SqliteStore::open(root.path()).unwrap();
    assert!(store.preview_cas_gc(256).await.unwrap().is_empty());
}

#[tokio::test]
async fn offline_verify_explicitly_refuses_pending_retirement() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let request = seed(&store, "pending-verify", 1).await;
    store.begin_purge(request).await.unwrap().unwrap();
    drop(store);
    assert!(
        matches!(SqliteStore::verify(root.path()), Err(StoreError::Unsupported(message)) if message.contains("pending retirements"))
    );
    let inspected = SqliteStore::inspect_cas(
        root.path(),
        CasInspectionMode::Metadata,
        &tokio_util::sync::CancellationToken::new(),
        |_| std::ops::ControlFlow::Continue(()),
    );
    assert!(
        matches!(inspected.unwrap_err().source, StoreError::Unsupported(message) if message.contains("pending retirements"))
    );
}

#[tokio::test]
async fn purge_batches_do_not_replay_unrelated_history_and_reject_extra_retiring_coverage() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    assert!(matches!(
        store.purge_batch("unknown").await,
        Err(StoreError::NotFound(_))
    ));
    let request = seed(&store, "selected", 160).await;
    seed(&store, "unrelated", 32).await;
    store.begin_purge(request.clone()).await.unwrap().unwrap();
    let before = store
        .inner
        .control_decodes
        .load(std::sync::atomic::Ordering::Relaxed);
    let first = store
        .purge_batch(request.operation_id.as_str())
        .await
        .unwrap();
    assert!(!first.complete);
    assert_eq!(
        store
            .inner
            .control_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        before,
        "no live-history replay on a retiring batch"
    );
    store.inner.connections.writer.lock().unwrap().execute("INSERT INTO reference_records(session_id,stream,seq,sha256,complete) VALUES('selected','fact',999,?1,1)", ["a".repeat(64)]).unwrap();
    assert!(matches!(
        store.purge_batch(request.operation_id.as_str()).await,
        Err(StoreError::Corrupt(_))
    ));
    assert_eq!(
        store
            .purge_status(request.operation_id.as_str())
            .await
            .unwrap()
            .unwrap(),
        first
    );
}

#[tokio::test]
async fn batches_defer_unselected_corruption_and_failed_barriers_preserve_policy() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let request = seed(&store, "bounded-verification", 600).await;
    store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .execute(
            "UPDATE reference_records SET complete=0 WHERE stream='header'",
            [],
        )
        .unwrap();
    assert!(matches!(
        store.begin_purge(request.clone()).await,
        Err(StoreError::Unsupported(_))
    ));
    let strict: bool = store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .query_row(
            "SELECT COALESCE((SELECT strict FROM reference_policy WHERE id=1),0)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!strict, "failed maintenance cannot latch strict admission");
    store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .execute(
            "UPDATE reference_records SET complete=1 WHERE stream='header'",
            [],
        )
        .unwrap();
    store.begin_purge(request.clone()).await.unwrap().unwrap();
    store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .execute(
            "UPDATE reference_records SET sha256=?1 WHERE stream='fact' AND seq=1200",
            ["a".repeat(64)],
        )
        .unwrap();
    // Only the selected rows may be decoded; later corruption fails its own batch.
    let mut previous = store
        .purge_batch(request.operation_id.as_str())
        .await
        .unwrap();
    assert_eq!(previous.deleted_records, 512);
    loop {
        match store.purge_batch(request.operation_id.as_str()).await {
            Ok(receipt) => {
                assert!(!receipt.complete);
                previous = receipt;
            }
            Err(StoreError::Corrupt(_)) => break,
            Err(error) => panic!("unexpected batch error: {error}"),
        }
    }
    assert_eq!(
        store
            .purge_status(request.operation_id.as_str())
            .await
            .unwrap()
            .unwrap(),
        previous
    );
}

#[tokio::test]
async fn cas_lease_checks_metadata_without_payload_io_and_read_still_authenticates() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let prepared = store
        .put_cas(Arc::from(b"verified".as_slice()))
        .await
        .unwrap();
    let object = prepared.reference().clone();
    drop(prepared);
    std::fs::write(root.path().join("cas").join(&object.sha256), b"tampered").unwrap();
    let lease = store.retain_cas(&object).await.unwrap();
    rsi_agent_testkit::assert_metadata_only_cas_retention(&store, &object).await;
    let target = store.preview_cas_gc(1).await.unwrap().remove(0);
    assert!(matches!(
        store.collect_cas("leased", target).await,
        Err(StoreError::ReadCapacity)
    ));
    drop(lease);
}

#[test]
fn tombstone_not_in_uses_the_existing_primary_key_without_materialization() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let connection = store.inner.connections.writer.lock().unwrap();
    let mut statement = connection.prepare("EXPLAIN QUERY PLAN SELECT durable_seq FROM sessions WHERE session_id NOT IN (SELECT session_id FROM session_tombstones) AND session_id=?1").unwrap();
    let plan = statement
        .query_map(["selected"], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join("\n");
    assert!(plan.contains("FOR IN-OPERATOR"), "{plan}");
    assert!(
        !plan.contains("SCAN session_tombstones") && !plan.contains("LIST SUBQUERY"),
        "{plan}"
    );
    println!("{plan}");
}

#[tokio::test]
async fn pending_operations_can_be_discovered_paged_and_resumed_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    for id in ["a", "b"] {
        let request = seed(&store, id, 1).await;
        store.begin_purge(request).await.unwrap().unwrap();
    }
    let object = store
        .put_cas(Arc::from(b"lost-operation".as_slice()))
        .await
        .unwrap()
        .reference()
        .clone();
    store.inner.connections.writer.lock().unwrap().execute("INSERT INTO cas_deletions(operation_id,sha256,byte_len,generation,state) VALUES('lost-gc',?1,?2,1,'deleting')", params![object.sha256, i64::try_from(object.byte_len).unwrap()]).unwrap();
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    for limit in [0, 257] {
        assert!(matches!(
            store.pending_purges(None, limit).await,
            Err(StoreError::Invalid(_))
        ));
    }
    assert!(matches!(
        store.pending_cas_collections(Some(""), 1).await,
        Err(StoreError::Invalid(_))
    ));
    let first = store.pending_purges(None, 1).await.unwrap();
    assert_eq!(first.len(), 1);
    let second = store.pending_purges(Some(&first[0]), 1).await.unwrap();
    assert_eq!(second.len(), 1);
    assert!(first[0] < second[0]);
    assert!(
        store
            .pending_purges(Some(&second[0]), 1)
            .await
            .unwrap()
            .is_empty()
    );
    for id in first.into_iter().chain(second) {
        assert!(store.purge_binding(&id).await.unwrap().is_some());
        assert!(store.purge_batch(&id).await.unwrap().complete);
    }
    assert!(store.pending_purges(None, 256).await.unwrap().is_empty());
    let ids = store.pending_cas_collections(None, 256).await.unwrap();
    assert_eq!(ids, vec!["lost-gc"]);
    let receipt = store.cas_gc_status(&ids[0]).await.unwrap().unwrap();
    assert!(
        store
            .collect_cas(&ids[0], receipt.target)
            .await
            .unwrap()
            .complete
    );
    assert!(
        store
            .pending_cas_collections(None, 256)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn batch_rejects_tombstone_receipt_disagreement_before_mutation() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let request = seed(&store, "state-mismatch", 1).await;
    let original = store.begin_purge(request.clone()).await.unwrap().unwrap();
    store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .execute("UPDATE session_tombstones SET state='purged'", [])
        .unwrap();
    assert!(matches!(
        store.purge_batch(request.operation_id.as_str()).await,
        Err(StoreError::Corrupt(_))
    ));
    assert_eq!(
        store
            .purge_status(request.operation_id.as_str())
            .await
            .unwrap()
            .unwrap(),
        original
    );
}

#[tokio::test]
async fn append_coverage_hashes_exact_persisted_header_fact_and_control_bytes() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    seed(&store, "exact-coverage", 3).await;
    let connection = store.inner.connections.writer.lock().unwrap();
    for (table, stream, column, seq) in [
        ("sessions", "header", "header_json", "0"),
        ("facts", "fact", "fact_json", "c.seq"),
        ("agent_controls", "control", "control_json", "c.seq"),
    ] {
        let mut statement = connection.prepare(&format!("SELECT c.{column},r.sha256 FROM {table} c JOIN reference_records r ON c.session_id=r.session_id AND r.stream='{stream}' AND r.seq={seq}")).unwrap();
        let mut rows = statement.query([]).unwrap();
        let mut count = 0;
        while let Some(row) = rows.next().unwrap() {
            let encoded: String = row.get(0).unwrap();
            let digest: String = row.get(1).unwrap();
            assert_eq!(digest, hex::encode(Sha256::digest(encoded.as_bytes())));
            count += 1;
        }
        assert!(count > 0);
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_audit_queues_a_concurrent_writer_and_publishes_before_releasing_it() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(root.path()).unwrap());
    seed(store.as_ref(), "busy-audit", 2).await;
    let (started, entered) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let mut started = Some(started);
    let observed = Arc::downgrade(&store.inner);
    store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .progress_handler(
            1,
            Some(move || {
                if let Some(owner) = observed.upgrade()
                    && owner.reference_audits.load(Ordering::Relaxed) == 1
                    && let Some(started) = started.take()
                {
                    started.send(()).unwrap();
                    wait.recv().unwrap();
                }
                false
            }),
        )
        .unwrap();
    let audit = tokio::spawn({
        let store = store.clone();
        async move { store.validate_references().await }
    });
    entered.await.unwrap();
    let writer = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .with_writer(|connection| {
                    connection
                        .execute("UPDATE reference_policy SET strict=0", [])
                        .map_err(sql_error)?;
                    Ok(())
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!writer.is_finished());
    release.send(()).unwrap();
    audit.await.unwrap().unwrap();
    writer.await.unwrap().unwrap();
    store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .progress_handler(0, None::<fn() -> bool>)
        .unwrap();
    assert_eq!(store.inner.reference_audits.load(Ordering::Relaxed), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_purge_admission_does_not_release_the_writer_after_cold_audit() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(root.path()).unwrap());
    let request = seed(store.as_ref(), "atomic-audit", 2).await;
    let (started, entered) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let mut started = Some(started);
    let observed = Arc::downgrade(&store.inner);
    store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .progress_handler(
            1,
            Some(move || {
                if let Some(owner) = observed.upgrade()
                    && owner.reference_audits.load(Ordering::Relaxed) == 1
                    && let Some(started) = started.take()
                {
                    started.send(()).unwrap();
                    wait.recv().unwrap();
                }
                false
            }),
        )
        .unwrap();
    let audit = tokio::spawn({
        let store = store.clone();
        async move { store.begin_purge(request).await }
    });
    entered.await.unwrap();
    let writer = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .with_writer(|connection| {
                    connection
                        .execute("UPDATE reference_records SET sha256=sha256", [])
                        .map_err(sql_error)?;
                    Ok(())
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!writer.is_finished());
    release.send(()).unwrap();
    audit.await.unwrap().unwrap().unwrap();
    writer.await.unwrap().unwrap();
    store
        .inner
        .connections
        .writer
        .lock()
        .unwrap()
        .progress_handler(0, None::<fn() -> bool>)
        .unwrap();
    assert_eq!(store.inner.reference_audits.load(Ordering::Relaxed), 1);
}
