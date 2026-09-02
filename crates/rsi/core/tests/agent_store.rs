use rsi_agent_session_protocol::{
    AgentPresetId, FrozenAgentSettings, SessionFact, SessionFactBody, SessionHeader, SessionId,
    TurnId,
};
use rsi_agent_store_protocol::{AppendBatch, SessionStore};
use rsi_agent_store_sqlite::SqliteStore;
use rsi_ai_protocol::ModelRef;
use rsi_sandbox::SandboxMode;
use serde_json::Value;
use std::process::Command;

#[test]
fn built_binary_verifies_an_explicit_existing_store_without_booting_plugins() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("agent");
    drop(SqliteStore::open(&root).unwrap());

    let output = Command::new(env!("CARGO_BIN_EXE_rsi"))
        .args([
            "agent-store",
            "verify",
            "--root",
            root.to_str().unwrap(),
            "--output",
            "json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["type"], "agent_store_verify");
    assert_eq!(value["status"], "ok");
    assert_eq!(value["root"], root.to_str().unwrap());
}

#[test]
fn built_binary_verify_does_not_create_a_missing_store() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("missing-agent");

    let output = Command::new(env!("CARGO_BIN_EXE_rsi"))
        .args(["agent-store", "verify", "--root", root.to_str().unwrap()])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("Agent Store verification failed")
    );
    assert!(!root.exists());
}

#[test]
fn built_binary_verify_rejects_a_snapshot_with_an_uncheckpointed_wal() {
    let live = tempfile::tempdir().unwrap();
    drop(SqliteStore::open(live.path()).unwrap());
    let store = SqliteStore::open(live.path()).unwrap();
    let session_id = SessionId::new("session-cli-wal").unwrap();
    let turn_id = TurnId::new("turn-cli-wal").unwrap();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(
            store.append(AppendBatch {
                session_id: session_id.clone(),
                expected_seq: 0,
                header: Some(
                    SessionHeader::new(
                        session_id,
                        1,
                        "/workspace",
                        AgentPresetId::new("cli-wal-agent").unwrap(),
                        FrozenAgentSettings::new(
                            "cli-wal",
                            "system",
                            ModelRef::new("deployment", "model").unwrap(),
                            SandboxMode::WorkspaceWrite,
                            false,
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                ),
                facts: vec![
                    SessionFact::new(
                        1,
                        1,
                        SessionFactBody::TurnAccepted {
                            turn_id,
                            text: "hello".into(),
                            model: None,
                            sandbox: SandboxMode::WorkspaceWrite,
                            require_approval: false,
                        },
                    )
                    .unwrap(),
                ],
            }),
        )
        .unwrap();
    assert!(
        std::fs::metadata(live.path().join("sessions.sqlite3-wal"))
            .unwrap()
            .len()
            > 0
    );

    let snapshot = tempfile::tempdir().unwrap();
    for name in ["sessions.sqlite3", "sessions.sqlite3-wal", ".writer.lock"] {
        std::fs::copy(live.path().join(name), snapshot.path().join(name)).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_rsi"))
        .args([
            "agent-store",
            "verify",
            "--root",
            snapshot.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("nonempty WAL"), "{stderr}");
    assert!(stderr.contains("cleanly closed"), "{stderr}");
    drop(store);
}
