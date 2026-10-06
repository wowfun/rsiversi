use super::*;
use rsi_browser::{Assertion, AssertionResult, CheckOutcome, CheckResult, CheckSpec};
const NOW: u64 = 1_800_000_000_000;

#[tokio::test]
async fn metadata_pressure_stops_new_dispatch_but_cancellation_uses_reserved_headroom() {
    let root = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&root.path().join("ledger"), NOW).unwrap();
    let running = admit(&ledger, 1, NOW).await.unwrap().attempt_id;
    ledger.claim(NOW).unwrap();
    let queued = admit(&ledger, 2, NOW).await.unwrap().attempt_id;
    ledger
        .connection
        .lock()
        .unwrap()
        .execute(
            "UPDATE settings SET value=?1 WHERE key='metadata_bytes'",
            [MAX_METADATA - 1024 * 1024 + 1],
        )
        .unwrap();
    assert!(matches!(ledger.claim(NOW), Err(AdmissionError::Capacity)));
    assert_eq!(ledger.get(queued).unwrap().state, AttemptState::Queued);
    assert_eq!(
        ledger.cancel(running, "stop-running").unwrap().state,
        AttemptState::Cancelled
    );
    assert_eq!(
        ledger.cancel(running, "stop-running").unwrap().state,
        AttemptState::Cancelled
    );
    exhaust_metadata(&ledger);
    assert!(matches!(
        ledger.cancel(queued, "stop-queued"),
        Err(AdmissionError::Capacity)
    ));
    assert_eq!(ledger.get(queued).unwrap().state, AttemptState::Queued);
}

#[tokio::test]
async fn cancelled_exploration_cannot_be_overwritten_by_late_start_or_completion() {
    for state in [
        crate::ExplorationState::Starting,
        crate::ExplorationState::Running,
    ] {
        let root = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&root.path().join("ledger"), NOW).unwrap();
        let id = admit(&ledger, 1, NOW).await.unwrap().attempt_id;
        ledger.claim(NOW).unwrap();
        ledger.settle(id, failed(), vec![], NOW).unwrap();
        ledger.exploration_state(id, state, None).unwrap();
        ledger.cancel(id, "operator-cancel").unwrap();
        for report in [None, Some("late completion".into())] {
            assert!(matches!(
                ledger.record_exploration(id, "late-session".into(), report),
                Err(AdmissionError::Conflict)
            ));
        }
        for late in [
            crate::ExplorationState::Running,
            crate::ExplorationState::Complete,
            crate::ExplorationState::Failed,
        ] {
            assert!(matches!(
                ledger.exploration_state(id, late, None),
                Err(AdmissionError::Conflict)
            ));
        }
        let attempt = ledger.get(id).unwrap();
        assert_eq!(attempt.state, AttemptState::Failed);
        assert_eq!(attempt.exploration, crate::ExplorationState::Cancelled);
        assert_eq!(
            attempt.result.unwrap().outcome,
            CheckOutcome::AssertionFailed
        );
    }
}

#[tokio::test]
async fn retention_expires_queued_work_without_claiming_or_touching_running_work() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    let running = admit(&ledger, 1, NOW).await.unwrap();
    ledger.claim(NOW).unwrap().unwrap();
    let queued = admit(&ledger, 2, NOW).await.unwrap();
    ledger.retain(NOW + 30 * 60 * 1000).unwrap();
    assert_eq!(
        ledger.get(queued.attempt_id).unwrap().state,
        AttemptState::Queued
    );
    ledger.retain(NOW + 30 * 60 * 1000 + 1).unwrap();
    assert_eq!(
        ledger.get(queued.attempt_id).unwrap().state,
        AttemptState::Interrupted
    );
    assert_eq!(
        ledger.get(running.attempt_id).unwrap().state,
        AttemptState::Running
    );
    assert!(ledger.available());
}

#[tokio::test]
async fn cancelling_failed_checks_preserves_verdict_and_only_stops_pending_exploration() {
    for (explore, state, expected) in [
        (
            false,
            crate::ExplorationState::NotStarted,
            crate::ExplorationState::NotStarted,
        ),
        (
            true,
            crate::ExplorationState::Complete,
            crate::ExplorationState::Complete,
        ),
        (
            true,
            crate::ExplorationState::Running,
            crate::ExplorationState::Cancelled,
        ),
        (
            true,
            crate::ExplorationState::NotStarted,
            crate::ExplorationState::Cancelled,
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
        let mut selected = rule();
        selected.explore_on_failure = explore;
        let receipt = ledger
            .admit(
                "source".into(),
                "cancel-failed".into(),
                "a".repeat(64),
                selected,
                deployment(1, NOW),
                NOW,
            )
            .await
            .unwrap();
        ledger.claim(NOW).unwrap().unwrap();
        ledger
            .settle(
                receipt.attempt_id,
                CheckResult {
                    outcome: CheckOutcome::AssertionFailed,
                    final_url: deployment(1, NOW).url,
                    assertions: vec![AssertionResult {
                        assertion: Assertion::TextVisible {
                            text: "Ready".into(),
                        },
                        passed: false,
                        detail: "absent".into(),
                    }],
                    snapshot: String::new(),
                    dialogs_dismissed: 0,
                    evidence_error: None,
                },
                vec![],
                NOW,
            )
            .unwrap();
        ledger
            .exploration_state(receipt.attempt_id, state, None)
            .unwrap();
        if explore && expected == crate::ExplorationState::Cancelled {
            assert!(matches!(
                ledger.resume(receipt.attempt_id, "resume-before-cancel", rule()),
                Err(AdmissionError::Conflict)
            ));
        }
        let cancelled = ledger.cancel(receipt.attempt_id, "cancel-once").unwrap();
        assert_eq!(cancelled.state, AttemptState::Failed);
        assert_eq!(cancelled.exploration, expected);
        assert_eq!(
            cancelled.result.unwrap().outcome,
            CheckOutcome::AssertionFailed
        );
        assert_eq!(
            ledger
                .cancel(receipt.attempt_id, "cancel-once")
                .unwrap()
                .state,
            AttemptState::Failed
        );
    }
}

#[tokio::test]
async fn abandoned_waiter_keeps_dispatch_bounded_and_queued_admission_can_be_cancelled() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    let (entered, entering) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let worker = ledger.clone();
    let first = tokio::spawn(async move {
        worker
            .run(move |ledger| {
                let _connection = ledger.connection.lock().unwrap();
                entered.send(()).unwrap();
                released.recv().unwrap();
                Ok(())
            })
            .await
    });
    entering.await.unwrap();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let mut second = Box::pin(admit(&ledger, 1, NOW));
    assert!(futures_util::poll!(&mut second).is_pending());
    let dispatched = ledger.work.len();
    drop(second);
    release.send(()).unwrap();
    ledger.drain().await;
    assert_eq!(
        dispatched, 1,
        "a dropped waiter must not release its running worker's admission"
    );
    assert!(
        ledger.list(0, None, 50).unwrap().0.is_empty(),
        "a cancelled caller waiting for dispatch must not commit a receipt"
    );
}

#[tokio::test]
async fn retirement_refuses_queued_work_and_waits_for_dispatched_settlement() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    let (entered, entering) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let worker = ledger.clone();
    let first = tokio::spawn(async move {
        worker
            .run(move |_| {
                entered.send(()).unwrap();
                released.recv().unwrap();
                Ok(())
            })
            .await
    });
    entering.await.unwrap();
    let mut queued = Box::pin(ledger.run(|_| Ok(())));
    assert!(futures_util::poll!(&mut queued).is_pending());
    let mut drain = Box::pin(ledger.drain());
    assert!(futures_util::poll!(&mut drain).is_pending());
    let refused = futures_util::poll!(&mut queued);
    release.send(()).unwrap();
    first.await.unwrap().unwrap();
    drain.await;
    assert!(matches!(
        refused,
        std::task::Poll::Ready(Err(AdmissionError::Unavailable))
    ));
    assert!(matches!(
        ledger.run(|_| Ok(())).await,
        Err(AdmissionError::Unavailable)
    ));
}

#[tokio::test]
async fn restart_validates_terminal_rows_without_republishing_them() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger");
    let ledger = Ledger::open(&path, NOW).unwrap();
    for id in 1..=33 {
        let receipt = admit(&ledger, id, NOW).await.unwrap();
        ledger
            .cancel(receipt.attempt_id, &format!("cancel-{id}"))
            .unwrap();
    }
    drop(ledger);
    let reopened = Ledger::open(&path, NOW).unwrap();
    assert!(
        reopened.connection.lock().unwrap().total_changes() <= 4,
        "restart must not UPDATE terminal attempts"
    );
    assert_eq!(reopened.list(0, None, 50).unwrap().0.len(), 33);
}

#[tokio::test]
async fn metadata_accounting_tracks_mutations_rollback_restart_and_retention() {
    fn check(ledger: &Ledger, expected_rows: i64) {
        let c = ledger.connection.lock().unwrap();
        let rows: i64 = c
            .query_row(
                "SELECT value FROM settings WHERE key='metadata_rows'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let bytes: i64 = c
            .query_row(
                "SELECT value FROM settings WHERE key='metadata_bytes'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let measured: i64 = c.query_row("SELECT (SELECT coalesce(sum(length(CAST(data AS BLOB))),0) FROM attempts)+(SELECT coalesce(sum(length(source)+length(delivery)+length(digest)+length(rule)+48),0) FROM receipts)+(SELECT coalesce(sum(length(request)+length(digest)+16),0) FROM mutations)+(SELECT coalesce(sum(length(source)+length(rule)+length(CAST(environment AS BLOB))+length(CAST(url AS BLOB))+64),0) FROM tasks)",[],|r|r.get(0)).unwrap();
        assert_eq!(rows, expected_rows);
        assert_eq!(bytes, measured);
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger");
    let ledger = Ledger::open(&path, NOW).unwrap();
    check(&ledger, 0);
    let receipt = admit(&ledger, 1, NOW).await.unwrap();
    check(&ledger, 3);
    let cancelled = ledger.cancel(receipt.attempt_id, "cancel-once").unwrap();
    assert_eq!(cancelled.state, AttemptState::Cancelled);
    assert_eq!(
        ledger.cancel(receipt.attempt_id, "cancel-once").unwrap().id,
        cancelled.id
    );
    check(&ledger, 4);
    let mut current = rule();
    current.revision = 2;
    let resumed = ledger.resume(cancelled.id, "resume-once", current).unwrap();
    check(&ledger, 6);
    assert_eq!(
        ledger.claim(resumed.created_ms).unwrap().unwrap().id,
        resumed.id
    );
    ledger.settle(resumed.id, failed(), vec![], NOW).unwrap();
    ledger
        .record_exploration(
            resumed.id,
            "session-accounting".into(),
            Some("中\\\"".repeat(1024)),
        )
        .unwrap();
    check(&ledger, 6);
    assert!(matches!(
        ledger.transact(|tx| {
            let mut a = read(tx, resumed.id)?;
            a.report = Some("rolled back".into());
            save(tx, &a)?;
            Err::<(), _>(AdmissionError::Conflict)
        }),
        Err(AdmissionError::Conflict)
    ));
    check(&ledger, 6);
    drop(ledger);
    let ledger = Ledger::open(&path, NOW).unwrap();
    check(&ledger, 6);
    ledger.retain(NOW + 40 * DAY).unwrap();
    check(&ledger, 0);
}

#[tokio::test]
async fn admission_ack_loss_retains_the_same_interrupted_identity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger");
    let ledger = Ledger::open(&path, NOW).unwrap();
    ledger.lose_commit_reply.store(true, Ordering::Release);
    assert!(matches!(
        admit(&ledger, 1, NOW).await,
        Err(AdmissionError::OutcomeUnknown)
    ));
    drop(ledger);
    let ledger = Ledger::open(&path, NOW).unwrap();
    let rows = ledger.list(0, None, 50).unwrap().0;
    assert_eq!(
        rows.len(),
        1,
        "admission uses one commit for floor and receipt"
    );
    assert_eq!(rows[0].state, AttemptState::Interrupted);
    let duplicate = admit(&ledger, 1, NOW).await.unwrap();
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.attempt_id, rows[0].id);
    assert!(ledger.claim(NOW).unwrap().is_none());
}

#[tokio::test]
async fn rejected_admission_still_persists_the_floor_before_clock_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger");
    let ledger = Ledger::open(&path, NOW).unwrap();
    assert!(matches!(
        ledger
            .admit(
                "source".into(),
                "stale-delivery".into(),
                "b".repeat(64),
                rule(),
                deployment(1, NOW),
                NOW + 8 * DAY
            )
            .await,
        Err(AdmissionError::Invalid(_))
    ));
    drop(ledger);
    let ledger = Ledger::open(&path, NOW).unwrap();
    assert!(matches!(
        admit(&ledger, 1, NOW).await,
        Err(AdmissionError::Invalid(_))
    ));
    assert!(ledger.list(0, None, 50).unwrap().0.is_empty());
}

#[tokio::test]
async fn api_read_does_not_block_the_async_executor_behind_the_sqlite_writer() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    let receipt = admit(&ledger, 1, NOW).await.unwrap();
    let policy = Arc::new(crate::PolicyOwner::open(directory.path().join("policy")).unwrap());
    policy
        .update(
            0,
            crate::Policy {
                revision: 0,
                rules: std::collections::BTreeMap::from([("source".into(), vec![rule()])]),
                grants: vec![],
                retired: vec![],
            },
        )
        .unwrap();
    let owner = crate::AutomationService::new(
        ledger.clone(),
        policy,
        None,
        std::collections::BTreeMap::default(),
        None,
    );
    let blocked = ledger.clone();
    let (entered, entering) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let writer = tokio::task::spawn_blocking(move || {
        let _connection = blocked.connection.lock().unwrap();
        entered.send(()).unwrap();
        released
            .recv_timeout(std::time::Duration::from_secs(2))
            .is_err()
    });
    entering.await.unwrap();
    let (started, starting) = tokio::sync::oneshot::channel();
    let reader = owner.clone();
    let read = tokio::spawn(async move {
        started.send(()).unwrap();
        reader
            .api(
                rsi_api_protocol::CallOrigin::Local,
                rsi_automation_api::Request::Get {
                    id: receipt.attempt_id.to_string(),
                },
                None,
            )
            .await
    });
    starting.await.unwrap();
    let _ = release.send(());
    let stalled = writer.await.unwrap();
    read.await.unwrap().unwrap();
    owner.close().await;
    assert!(
        !stalled,
        "the Tokio executor must run the writer release while a read waits"
    );
}
pub(crate) fn rule() -> AutomationRule {
    AutomationRule {
        id: "preview".into(),
        revision: 1,
        enabled: true,
        repository_id: 7,
        environment: "preview".into(),
        preview_host_suffix: "example.invalid".into(),
        path_prefix: "/".into(),
        dependency_hosts: std::collections::BTreeSet::default(),
        checks: CheckSpec {
            entry_identity: "Preview".into(),
            assertions: vec![Assertion::TextVisible {
                text: "Ready".into(),
            }],
        },
        explore_on_failure: true,
        authorized_catalog_digest: "0".repeat(64),
        model: rsi_ai_protocol::ModelRef::new("fixture", "fixture-model").unwrap(),
        turn_budget: rsi_agent_session_protocol::TurnBudget::new(120_000, 8, 16, 256, 1_048_576)
            .unwrap(),
        max_rounds: 2,
    }
}
pub(crate) fn deployment(id: u64, created: u64) -> Deployment {
    Deployment {
        repository_id: 7,
        deployment_id: id,
        status_id: id,
        deployment_created_ms: created,
        status_created_ms: created,
        environment: "preview".into(),
        sha: "a".repeat(40),
        url: format!("https://deployment-{id}.example.invalid/"),
    }
}
async fn admit(ledger: &Arc<Ledger>, id: u64, created: u64) -> Result<Receipt> {
    ledger
        .admit(
            "source".into(),
            format!("delivery-{id}"),
            "b".repeat(64),
            rule(),
            deployment(id, created),
            NOW,
        )
        .await
}
pub(crate) fn failed() -> CheckResult {
    CheckResult {
        outcome: CheckOutcome::AssertionFailed,
        final_url: "https://deployment-1.example.invalid/".into(),
        assertions: vec![AssertionResult {
            assertion: Assertion::TextVisible {
                text: "Ready".into(),
            },
            passed: false,
            detail: "Predicate not satisfied".into(),
        }],
        snapshot: "Preview".into(),
        dialogs_dismissed: 0,
        evidence_error: None,
    }
}
#[tokio::test]
async fn concurrent_receipts_and_success_statuses_have_one_logical_task() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    let mut workers = vec![];
    for _ in 0..8 {
        let owner = ledger.clone();
        workers.push(tokio::spawn(
            async move { admit(&owner, 1, NOW).await.unwrap() },
        ));
    }
    let rows = futures_util::future::join_all(workers).await;
    let id = rows[0].as_ref().unwrap().attempt_id;
    assert!(rows.iter().all(|r| r.as_ref().unwrap().attempt_id == id));
    assert_eq!(ledger.list(0, None, 50).unwrap().0.len(), 1);
    let mut status = deployment(1, NOW);
    status.status_id = 2;
    status.status_created_ms = NOW + 1;
    let next = ledger
        .admit(
            "source".into(),
            "new-status".into(),
            "c".repeat(64),
            rule(),
            status,
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(next.attempt_id, id);
    assert!(next.duplicate);
    assert!(matches!(
        ledger
            .admit(
                "source".into(),
                "delivery-1".into(),
                "d".repeat(64),
                rule(),
                deployment(1, NOW),
                NOW
            )
            .await,
        Err(AdmissionError::Conflict)
    ));
}
#[tokio::test]
async fn ordering_identity_reuse_floor_and_ambiguous_time_are_explicit() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    let first = admit(&ledger, 1, NOW - 10).await.unwrap();
    let newest = admit(&ledger, 2, NOW).await.unwrap();
    assert_eq!(
        ledger.get(first.attempt_id).unwrap().state,
        AttemptState::Superseded
    );
    let old = admit(&ledger, 3, NOW - 5).await.unwrap();
    assert_eq!(
        ledger.get(old.attempt_id).unwrap().state,
        AttemptState::Superseded
    );
    let equal = admit(&ledger, 4, NOW).await.unwrap();
    assert_eq!(
        ledger.get(equal.attempt_id).unwrap().state,
        AttemptState::Queued
    );
    assert_eq!(
        ledger.get(newest.attempt_id).unwrap().state,
        AttemptState::Queued
    );
    let mut reused = deployment(5, NOW);
    reused.url = deployment(2, NOW).url;
    assert!(matches!(
        ledger
            .admit(
                "source".into(),
                "reused".into(),
                "e".repeat(64),
                rule(),
                reused,
                NOW
            )
            .await,
        Err(AdmissionError::Invalid(_))
    ));
    assert!(matches!(
        admit(&ledger, 6, NOW - 8 * DAY).await,
        Err(AdmissionError::Invalid(_))
    ));
    ledger.retain(NOW + 8 * DAY).unwrap();
    assert!(matches!(
        admit(&ledger, 1, NOW - 10).await,
        Err(AdmissionError::Invalid(_))
    ));
}
#[tokio::test]
async fn restart_disarms_and_resume_preserves_original_and_new_rule() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    let receipt = admit(&ledger, 1, NOW).await.unwrap();
    let claim = ledger.claim(NOW).unwrap().unwrap();
    assert_eq!(claim.id, receipt.attempt_id);
    drop(ledger);
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    assert_eq!(
        ledger.get(claim.id).unwrap().state,
        AttemptState::Interrupted
    );
    assert!(ledger.claim(NOW).unwrap().is_none());
    let mut current = rule();
    current.revision = 2;
    let resumed = ledger
        .resume(claim.id, "resume-once", current.clone())
        .unwrap();
    assert_ne!(resumed.id, claim.id);
    assert_eq!(resumed.rule.revision, 2);
    assert_eq!(
        ledger.resume(claim.id, "resume-once", current).unwrap().id,
        resumed.id
    );
    assert_eq!(admit(&ledger, 1, NOW).await.unwrap().attempt_id, claim.id);
}
#[tokio::test]
async fn failed_check_exploration_identity_survives_crash_and_retention() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    let receipt = admit(&ledger, 1, NOW).await.unwrap();
    ledger.claim(NOW).unwrap();
    ledger
        .settle(receipt.attempt_id, failed(), vec![], NOW)
        .unwrap();
    assert!(ledger.eligible(receipt.attempt_id).unwrap());
    ledger
        .record_exploration(receipt.attempt_id, "protected-session".into(), None)
        .unwrap();
    assert!(!ledger.eligible(receipt.attempt_id).unwrap());
    drop(ledger);
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    assert_eq!(
        ledger.get(receipt.attempt_id).unwrap().exploration,
        crate::ExplorationState::Interrupted
    );
    assert!(ledger.claim(NOW).unwrap().is_none());
    ledger.retain(NOW + 40 * DAY).unwrap();
    assert!(matches!(
        ledger.get(receipt.attempt_id),
        Err(AdmissionError::NotFound)
    ));
}
#[tokio::test]
async fn capacity_rejection_is_durable_and_unknown_commit_fences() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    for id in 1..=33 {
        let mut r = rule();
        r.id = format!("rule-{id}");
        r.environment = format!("env-{id}");
        let mut d = deployment(id, NOW);
        d.environment = r.environment.clone();
        let receipt = ledger
            .admit(
                "source".into(),
                format!("delivery-{id}"),
                "a".repeat(64),
                r,
                d,
                NOW,
            )
            .await
            .unwrap();
        if id == 33 {
            assert_eq!(
                ledger.get(receipt.attempt_id).unwrap().state,
                AttemptState::CapacityRejected
            );
        }
    }
    let claim = ledger.claim(NOW).unwrap().unwrap();
    ledger.lose_commit_reply.store(true, Ordering::Release);
    assert!(matches!(
        ledger.settle(claim.id, failed(), vec![], NOW),
        Err(AdmissionError::OutcomeUnknown)
    ));
    assert!(!ledger.available());
    assert!(matches!(
        ledger.get(claim.id),
        Err(AdmissionError::Unavailable)
    ));
    drop(ledger);
    let reopened = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    assert_eq!(reopened.get(claim.id).unwrap().state, AttemptState::Failed);
    assert!(reopened.claim(NOW).unwrap().is_none());
}

#[tokio::test]
async fn one_signed_delivery_admits_distinct_rules_without_hiding_changed_bodies_or_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&tmp.path().join("ledger"), NOW).unwrap();
    let first = admit(&ledger, 1, NOW).await.unwrap();
    let mut second_rule = rule();
    second_rule.id = "second".into();
    let second = ledger
        .admit(
            "source".into(),
            "delivery-1".into(),
            "b".repeat(64),
            second_rule.clone(),
            deployment(1, NOW),
            NOW,
        )
        .await
        .unwrap();
    assert_ne!(first.task_id, second.task_id);
    assert_eq!(
        ledger
            .admit(
                "source".into(),
                "delivery-1".into(),
                "b".repeat(64),
                second_rule.clone(),
                deployment(1, NOW),
                NOW
            )
            .await
            .unwrap()
            .attempt_id,
        second.attempt_id
    );
    assert!(matches!(
        ledger
            .admit(
                "source".into(),
                "delivery-1".into(),
                "a".repeat(64),
                second_rule,
                deployment(1, NOW),
                NOW
            )
            .await,
        Err(AdmissionError::Conflict)
    ));
    let mut changed = deployment(1, NOW);
    changed.sha = "c".repeat(40);
    assert!(matches!(
        ledger
            .admit(
                "source".into(),
                "changed-status".into(),
                "b".repeat(64),
                rule(),
                changed,
                NOW
            )
            .await,
        Err(AdmissionError::Conflict)
    ));
}
#[tokio::test]
async fn maximum_escaped_snapshot_and_report_remain_readable_after_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&tmp.path().join("ledger"), NOW).unwrap();
    let id = admit(&ledger, 1, NOW).await.unwrap().attempt_id;
    ledger.claim(NOW).unwrap();
    let mut result = failed();
    result.snapshot = "\n".repeat(65_536);
    ledger.settle(id, result, vec![], NOW).unwrap();
    ledger
        .record_exploration(id, "session".into(), Some("\n".repeat(65_536)))
        .unwrap();
    drop(ledger);
    let ledger = Ledger::open(&tmp.path().join("ledger"), NOW).unwrap();
    let a = ledger.get(id).unwrap();
    assert_eq!(a.report.unwrap().len(), 65_536);
    assert_eq!(a.result.unwrap().snapshot.len(), 65_536);
}
#[test]
fn policy_revisions_revocation_retirement_and_intake_log_are_independent() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("policy");
    let owner = crate::PolicyOwner::open(dir.clone()).unwrap();
    let p = crate::Policy {
        rules: std::collections::BTreeMap::from([("source".into(), vec![rule()])]),
        ..Default::default()
    };
    owner.update(0, p).unwrap();
    let lease = owner
        .permits(
            &rsi_api_protocol::CallOrigin::Local,
            "source",
            "preview",
            "view",
        )
        .unwrap();
    let mut p = (*owner.snapshot().unwrap()).clone();
    p.rules.get_mut("source").unwrap()[0].enabled = false;
    assert!(matches!(
        owner.update(1, p.clone()),
        Err(AdmissionError::Conflict)
    ));
    p.rules.get_mut("source").unwrap()[0].revision = 2;
    owner.update(1, p).unwrap();
    assert!(lease.is_cancelled());
    for _ in 0..258 {
        owner.reject(
            Some("source"),
            Some("delivery"),
            crate::intake::Reason::StorageUnavailable,
        );
    }
    assert_eq!(
        owner.intake_diagnostics()["rejections"]
            .as_array()
            .unwrap()
            .len(),
        256
    );
    assert!(
        !dir.join("intake-log.json").exists(),
        "garbage traffic must not write per rejection"
    );
    owner.flush_intake();
    let first = std::fs::metadata(dir.join("intake-log.json"))
        .unwrap()
        .modified()
        .unwrap();
    owner.flush_intake();
    assert_eq!(
        std::fs::metadata(dir.join("intake-log.json"))
            .unwrap()
            .modified()
            .unwrap(),
        first
    );
    drop(owner);
    let owner = crate::PolicyOwner::open(dir).unwrap();
    assert_eq!(
        owner.intake_diagnostics()["rejections"]
            .as_array()
            .unwrap()
            .len(),
        256
    );
}

#[tokio::test]
async fn corrupt_durable_payload_fences_the_owner_before_dispatch() {
    let tmp = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&tmp.path().join("ledger"), NOW).unwrap();
    let id = admit(&ledger, 1, NOW).await.unwrap().attempt_id;
    ledger
        .connection
        .lock()
        .unwrap()
        .execute("UPDATE attempts SET data='{' WHERE id=?1", [SqlU64(id)])
        .unwrap();
    assert!(ledger.get(id).is_err());
    assert!(!ledger.available());
    assert!(matches!(
        ledger.claim(NOW),
        Err(AdmissionError::Unavailable)
    ));
}

#[test]
fn retiring_a_rule_keeps_historical_view_authority_without_execution_authority() {
    let tmp = tempfile::tempdir().unwrap();
    let owner = crate::PolicyOwner::open(tmp.path().join("policy")).unwrap();
    let device = rsi_api_protocol::DeviceId::from_bytes([3; 16]);
    let origin = rsi_api_protocol::CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: device.clone(),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    let p = crate::Policy {
        rules: std::collections::BTreeMap::from([("source".into(), vec![rule()])]),
        grants: vec![crate::AutomationGrant {
            device: device.as_str().into(),
            source: "source".into(),
            rule: "preview".into(),
            view: true,
            cancel: true,
            resume: true,
        }],
        ..Default::default()
    };
    owner.update(0, p).unwrap();
    let mut p = (*owner.snapshot().unwrap()).clone();
    p.rules.clear();
    owner.update(1, p).unwrap();
    assert!(owner.permits(&origin, "source", "preview", "view").is_ok());
    assert!(
        owner
            .permits(&origin, "source", "preview", "resume")
            .is_err()
    );
    let mut p = (*owner.snapshot().unwrap()).clone();
    p.rules.insert("source".into(), vec![rule()]);
    assert!(matches!(owner.update(2, p), Err(AdmissionError::Conflict)));
}

#[tokio::test]
async fn oversized_or_invalid_durable_png_fences_without_unbounded_blob_read() {
    for png in [vec![0; 512 * 1024 + 1], b"\x89PNG\r\n\x1a\n".to_vec()] {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&tmp.path().join("ledger"), NOW).unwrap();
        let id = admit(&ledger, 1, NOW).await.unwrap().attempt_id;
        ledger
            .connection
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO artifacts VALUES(?1,0,?2,?3)",
                params![SqlU64(id), SqlU64(NOW), png],
            )
            .unwrap();
        assert!(matches!(
            ledger.artifact(id, 0),
            Err(AdmissionError::Corrupt)
        ));
        assert!(!ledger.available());
        assert!(matches!(
            ledger.claim(NOW),
            Err(AdmissionError::Unavailable)
        ));
    }
}

#[tokio::test]
async fn idle_and_full_claims_do_not_enter_commit_or_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&tmp.path().join("ledger"), NOW).unwrap();
    let changes = ledger.connection.lock().unwrap().total_changes();
    ledger.lose_commit_reply.store(true, Ordering::Release);
    for _ in 0..10 {
        assert!(ledger.claim(NOW).unwrap().is_none());
    }
    assert!(ledger.lose_commit_reply.load(Ordering::Acquire));
    assert_eq!(ledger.connection.lock().unwrap().total_changes(), changes);
    ledger.lose_commit_reply.store(false, Ordering::Release);
    for id in 1..=3 {
        admit(&ledger, id, NOW).await.unwrap();
    }
    ledger.claim(NOW).unwrap().unwrap();
    ledger.claim(NOW).unwrap().unwrap();
    ledger.lose_commit_reply.store(true, Ordering::Release);
    assert!(ledger.claim(NOW).unwrap().is_none());
    assert!(ledger.lose_commit_reply.load(Ordering::Acquire));
    ledger.lose_commit_reply.store(false, Ordering::Release);
    assert!(ledger.available());
}
#[tokio::test]
async fn claim_retires_expired_heads_before_selecting_fresh_work() {
    let tmp = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&tmp.path().join("ledger"), NOW).unwrap();
    let old = admit(&ledger, 1, NOW).await.unwrap().attempt_id;
    ledger
        .transact(|tx| {
            let mut a = read(tx, old)?;
            a.created_ms = NOW - 31 * 60 * 1000;
            save(tx, &a)
        })
        .unwrap();
    let fresh = admit(&ledger, 2, NOW).await.unwrap().attempt_id;
    assert_eq!(ledger.claim(NOW).unwrap().unwrap().id, fresh);
    assert_eq!(ledger.get(old).unwrap().state, AttemptState::Interrupted);
}

#[tokio::test]
async fn invalid_png_is_rejected_before_settlement_without_fencing_reads() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    let receipt = admit(&ledger, 1, NOW).await.unwrap();
    ledger.claim(NOW).unwrap().unwrap();
    let result = ledger.settle(
        receipt.attempt_id,
        failed(),
        vec![b"\x89PNG\r\n\x1a\n".to_vec()],
        NOW,
    );
    assert!(
        matches!(result, Err(AdmissionError::Invalid(_))),
        "{result:?}"
    );
    assert!(ledger.available());
    assert_eq!(
        ledger.get(receipt.attempt_id).unwrap().state,
        AttemptState::Running
    );
}
#[tokio::test]
async fn fresh_delivery_names_cannot_allocate_receipts_for_the_same_signed_body() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    let first = admit(&ledger, 1, NOW).await.unwrap();
    for number in 0..16 {
        let duplicate = ledger
            .admit(
                "source".into(),
                format!("replay-{number}"),
                "b".repeat(64),
                rule(),
                deployment(1, NOW),
                NOW,
            )
            .await
            .unwrap();
        assert_eq!(duplicate.attempt_id, first.attempt_id);
        assert!(duplicate.duplicate);
    }
    let count: i64 = ledger
        .connection
        .lock()
        .unwrap()
        .query_row("SELECT count(*) FROM receipts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "unsigned delivery names grew durable accounting");
}

pub(crate) fn exhaust_metadata(ledger: &Ledger) {
    ledger
        .connection
        .lock()
        .unwrap()
        .execute(
            "UPDATE settings SET value=?1 WHERE key='metadata_bytes'",
            [MAX_METADATA],
        )
        .unwrap();
}
#[tokio::test]
async fn a_later_rule_conflict_rolls_back_all_candidate_receipts_but_keeps_the_floor() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    let mut first = rule();
    first.id = "first".into();
    let mut second = rule();
    second.id = "second".into();
    let wanted = deployment(1, NOW);
    let mut conflicting = wanted.clone();
    conflicting.sha = "c".repeat(40);
    ledger
        .admit(
            "source".into(),
            "first-original".into(),
            "a".repeat(64),
            first.clone(),
            wanted.clone(),
            NOW,
        )
        .await
        .unwrap();
    ledger
        .admit(
            "source".into(),
            "second-original".into(),
            "c".repeat(64),
            second.clone(),
            conflicting,
            NOW,
        )
        .await
        .unwrap();
    let result = ledger
        .admit_rules(
            "source".into(),
            "combined".into(),
            "d".repeat(64),
            vec![first, second],
            wanted,
            NOW + 100,
        )
        .await;
    assert!(matches!(result, Err(AdmissionError::Conflict)));
    let connection = ledger.connection.lock().unwrap();
    let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM receipts WHERE delivery='combined'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    let floor: u64 = connection
        .query_row("SELECT value FROM settings WHERE key='floor'", [], |r| {
            row_number(r, 0)
        })
        .unwrap();
    assert_eq!(floor, NOW + 100 - 7 * DAY);
}
#[tokio::test]
async fn invalid_clock_values_neither_advance_the_floor_nor_delete_history() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    let receipt = admit(&ledger, 1, NOW).await.unwrap();
    for bad in [0, u64::MAX, i64::MAX.cast_unsigned()] {
        assert!(matches!(
            ledger.retain(bad),
            Err(AdmissionError::Invalid(_))
        ));
        assert!(matches!(ledger.claim(bad), Err(AdmissionError::Invalid(_))));
        assert!(matches!(
            ledger
                .admit(
                    "source".into(),
                    "bad-time".into(),
                    "b".repeat(64),
                    rule(),
                    deployment(2, NOW),
                    bad
                )
                .await,
            Err(AdmissionError::Invalid(_))
        ));
        assert!(ledger.available());
        assert_eq!(
            ledger.get(receipt.attempt_id).unwrap().state,
            AttemptState::Queued
        );
    }
    let floor: u64 = ledger
        .connection
        .lock()
        .unwrap()
        .query_row("SELECT value FROM settings WHERE key='floor'", [], |r| {
            row_number(r, 0)
        })
        .unwrap();
    assert_eq!(floor, NOW - 7 * DAY);
}

#[tokio::test]
async fn valid_noncanonical_png_is_normalized_before_commit_and_survives_readback() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    let receipt = admit(&ledger, 1, NOW).await.unwrap();
    ledger.claim(NOW).unwrap().unwrap();
    let mut input = hex::decode("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c63f8cfc0f01f00050001ff89993d1d0000000049454e44ae426082").unwrap();
    input.extend_from_slice(b"trailing noncanonical bytes");
    ledger
        .settle(receipt.attempt_id, failed(), vec![input.clone()], NOW)
        .unwrap();
    let stored = ledger.artifact(receipt.attempt_id, 0).unwrap();
    assert_ne!(stored, input);
    assert!(ledger.available());
    drop(ledger);
    let ledger = Ledger::open(&directory.path().join("ledger"), NOW).unwrap();
    assert_eq!(ledger.artifact(receipt.attempt_id, 0).unwrap(), stored);
}
#[tokio::test]
async fn corrupted_durable_clock_floor_cannot_be_reopened() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger");
    let ledger = Ledger::open(&path, NOW).unwrap();
    admit(&ledger, 1, NOW).await.unwrap();
    ledger
        .connection
        .lock()
        .unwrap()
        .execute("UPDATE settings SET value=?1 WHERE key='floor'", [i64::MAX])
        .unwrap();
    drop(ledger);
    assert!(matches!(
        Ledger::open(&path, NOW),
        Err(AdmissionError::Corrupt)
    ));
}
