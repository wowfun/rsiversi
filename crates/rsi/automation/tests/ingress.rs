use rsi_api_protocol::{AuthenticatedDevice, CallOrigin, DeviceId};
use rsi_automation::*;
use rsi_browser::{Assertion, CheckSpec};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;
fn test_directory() -> tempfile::TempDir {
    tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap()
}
fn rule() -> AutomationRule {
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
fn device(id: &str) -> CallOrigin {
    CallOrigin::Device(AuthenticatedDevice {
        id: DeviceId::from_bytes([if id == "viewer" { 1 } else { 2 }; 16]),
        revoked: CancellationToken::new(),
    })
}
fn now() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep signature admission, scoped controls and revocation in one authority scenario"
)]
async fn signed_admission_scoped_reads_distinct_controls_and_revocation() {
    let directory = test_directory();
    let ledger = Ledger::open(&directory.path().join("ledger"), now()).unwrap();
    let policy = Arc::new(PolicyOwner::open(directory.path().join("policy")).unwrap());
    let p = Policy {
        revision: 0,
        rules: BTreeMap::from([("source".into(), vec![rule()])]),
        grants: vec![AutomationGrant {
            device: "01".repeat(16),
            source: "source".into(),
            rule: "preview".into(),
            view: true,
            cancel: false,
            resume: false,
        }],
        retired: vec![],
    };
    policy.update(0, p).unwrap();
    let owner = AutomationService::new(
        ledger.clone(),
        policy.clone(),
        None,
        BTreeMap::from([(
            "source".into(),
            rsi_credentials_protocol::SecretValue::new("fixture-only-secret").unwrap(),
        )]),
        None,
    );
    let timestamp = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let body=serde_json::to_vec(&json!({"repository":{"id":7},"deployment":{"id":1,"environment":"preview","sha":"a".repeat(40),"created_at":timestamp},"deployment_status":{"id":1,"state":"success","environment":"preview","environment_url":"https://deployment.example.invalid/","created_at":timestamp}})).unwrap();
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"fixture-only-secret");
    let signature = format!(
        "sha256={}",
        hex::encode(ring::hmac::sign(&key, &body).as_ref())
    );
    assert!(
        owner
            .accept("source", "invalid", "deployment_status", "sha256=00", &body)
            .await
            .is_err()
    );
    let receipt = owner
        .accept("source", "delivery", "deployment_status", &signature, &body)
        .await
        .unwrap()
        .remove(0);
    assert!(
        owner
            .accept("source", "delivery", "deployment_status", &signature, &body)
            .await
            .unwrap()[0]
            .duplicate
    );
    assert!(
        owner
            .accept("source", "ping", "ping", &signature, &body)
            .await
            .is_err()
    );
    let ping = b"{\"zen\":\"fixture\"}";
    let ping_signature = format!(
        "sha256={}",
        hex::encode(ring::hmac::sign(&key, ping).as_ref())
    );
    assert!(
        owner
            .accept("source", "ping", "ping", &ping_signature, ping)
            .await
            .unwrap()
            .is_empty()
    );
    let request = || rsi_automation_api::Request::Get {
        id: receipt.attempt_id.to_string(),
    };
    assert!(owner.api(device("unknown"), request(), None).await.is_err());
    assert!(owner.api(device("viewer"), request(), None).await.is_ok());
    assert!(
        owner
            .api(
                device("viewer"),
                rsi_automation_api::Request::Cancel {
                    id: receipt.attempt_id.to_string(),
                    request_id: "forbidden".into()
                },
                None
            )
            .await
            .is_err()
    );
    let scope =
        rsi_agent_session_protocol::SessionProtectionScope::new("automation", "source:preview")
            .unwrap();
    let lease =
        rsi_session_protocol::SessionProtection::view(&*policy, &scope, &device("viewer")).unwrap();
    let mut p = (*policy.snapshot().unwrap()).clone();
    p.grants.clear();
    policy.update(1, p).unwrap();
    assert!(lease.is_cancelled());
    assert!(owner.api(device("viewer"), request(), None).await.is_err());
    let mut p = (*policy.snapshot().unwrap()).clone();
    p.rules.clear();
    policy.update(2, p).unwrap();
    let mut p = (*policy.snapshot().unwrap()).clone();
    p.rules.insert("source".into(), vec![rule()]);
    assert!(policy.update(3, p).is_err());
    owner.close().await;
}

#[tokio::test(start_paused = true)]
async fn expired_evidence_is_retired_even_when_browser_execution_is_disabled() {
    let directory = test_directory();
    let old = now() - 31 * 86_400_000;
    let ledger = Ledger::open(&directory.path().join("ledger"), old).unwrap();
    let policy = Arc::new(PolicyOwner::open(directory.path().join("policy")).unwrap());
    let receipt = ledger
        .admit(
            "source".into(),
            "old-delivery".into(),
            "a".repeat(64),
            rule(),
            Deployment {
                repository_id: 7,
                deployment_id: 1,
                status_id: 1,
                deployment_created_ms: old,
                status_created_ms: old,
                environment: "preview".into(),
                sha: "b".repeat(40),
                url: "https://deployment.example.invalid/".into(),
            },
            old,
        )
        .await
        .unwrap();
    ledger.claim(old).unwrap();
    ledger
        .settle(
            receipt.attempt_id,
            rsi_browser::CheckResult {
                outcome: rsi_browser::CheckOutcome::TargetUnavailable,
                final_url: "https://deployment.example.invalid/".into(),
                assertions: vec![],
                snapshot: String::new(),
                dialogs_dismissed: 0,
                evidence_error: None,
            },
            vec![],
            old,
        )
        .unwrap();
    assert!(ledger.get(receipt.attempt_id).is_ok());
    let service = AutomationService::new(ledger.clone(), policy, None, BTreeMap::new(), None);
    service.start();
    let retired = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if matches!(
                ledger.get(receipt.attempt_id),
                Err(AdmissionError::NotFound)
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    service.close().await;
    assert!(
        retired.is_ok(),
        "retention must not require a ready browser"
    );
}

#[tokio::test]
async fn mutation_ids_are_independent_for_two_authorized_principals() {
    let directory = test_directory();
    let ledger = Ledger::open(&directory.path().join("ledger"), now()).unwrap();
    let policy = Arc::new(PolicyOwner::open(directory.path().join("policy")).unwrap());
    policy
        .update(
            0,
            Policy {
                rules: BTreeMap::from([("source".into(), vec![rule()])]),
                grants: [1, 2]
                    .into_iter()
                    .map(|byte| AutomationGrant {
                        device: hex::encode([byte; 16]),
                        source: "source".into(),
                        rule: "preview".into(),
                        view: true,
                        cancel: true,
                        resume: false,
                    })
                    .collect(),
                ..Default::default()
            },
        )
        .unwrap();
    let owner = AutomationService::new(ledger.clone(), policy, None, BTreeMap::new(), None);
    for (id, principal) in [(1, "viewer"), (2, "controller")] {
        let deployment = Deployment {
            repository_id: 7,
            deployment_id: id,
            status_id: id,
            deployment_created_ms: now(),
            status_created_ms: now(),
            environment: "preview".into(),
            sha: "a".repeat(40),
            url: format!("https://deployment-{id}.example.invalid/"),
        };
        let receipt = ledger
            .admit(
                "source".into(),
                format!("delivery-{id}"),
                "b".repeat(64),
                rule(),
                deployment,
                now(),
            )
            .await
            .unwrap();
        let reply = owner
            .api(
                device(principal),
                rsi_automation_api::Request::Cancel {
                    id: receipt.attempt_id.to_string(),
                    request_id: "shared-id".into(),
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(reply["state"], "cancelled");
    }
    owner.close().await;
}
