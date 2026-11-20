#![cfg(any(unix, windows))]

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use rsi_agent_session_protocol::{
    AgentControlRecord, AgentPresetId, FrozenAgentSettings, SESSION_FORMAT_VERSION, SessionFact,
    SessionFactBody, SessionHeader, SessionId, TurnId, TurnOutcome,
};
use rsi_agent_store_protocol::{AGENT_STORE_SCHEMA_VERSION, SessionStore as _, StoreError};
use rsi_agent_store_sqlite::SqliteStore;
use rsi_ai_protocol::ModelRef;
use rsi_sandbox::SandboxMode;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[path = "persistence/execution.rs"]
mod execution;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    session_format: u32,
    store_schema: u32,
    schema_sha256: String,
    files: std::collections::BTreeMap<String, String>,
}

fn header(name: &str) -> SessionHeader {
    SessionHeader::new_local(
        SessionId::new(name).unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}

fn facts() -> [SessionFact; 3] {
    let turn_id = TurnId::new("turn-1").unwrap();
    [
        SessionFact::new(
            1,
            1,
            SessionFactBody::TurnAccepted {
                turn_id: turn_id.clone(),
                text: "Unicode: 中 🦀; escapes: \n\"\\".into(),
                model: None,
                reasoning_effort: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap(),
        SessionFact::new(
            2,
            2,
            SessionFactBody::CancelRequested {
                turn_id: turn_id.clone(),
                reason: Some("stop".into()),
            },
        )
        .unwrap(),
        SessionFact::new(
            3,
            3,
            SessionFactBody::TurnTerminal {
                turn_id,
                outcome: TurnOutcome::Completed,
                result: None,
            },
        )
        .unwrap(),
    ]
}

fn connection(root: &Path) -> Connection {
    Connection::open_with_flags(
        root.join("sessions.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}

fn schema_hash(root: &Path) -> String {
    let db = connection(root);
    let mut query = db
        .prepare(
            "SELECT type, name, sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type, name",
        )
        .unwrap();
    let rows = query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap();
    let mut hash = Sha256::new();
    for row in rows {
        let (kind, name, sql) = row.unwrap();
        hash.update(
            serde_json::to_vec(&(kind, name, sql.split_whitespace().collect::<Vec<_>>())).unwrap(),
        );
        hash.update([0]);
    }
    hex::encode(hash.finalize())
}

fn file_hashes(root: &Path) -> std::collections::BTreeMap<String, String> {
    fn walk(root: &Path, path: &Path, out: &mut std::collections::BTreeMap<String, String>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, out);
            } else if path.file_name().unwrap() != "baseline.json" {
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/");
                let (name, bytes) = fixture_bytes(&name, &path);
                out.insert(name, hex::encode(Sha256::digest(bytes)));
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn assert_encoding<T: serde::de::DeserializeOwned + Serialize + schemars::JsonSchema>(
    db: &Connection,
    query: &str,
) {
    let schema = schemars::generate::SchemaSettings::draft07()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<T>();
    let validator = jsonschema::validator_for(&serde_json::to_value(schema).unwrap()).unwrap();
    let mut statement = db.prepare(query).unwrap();
    for value in statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
    {
        let encoded = value.unwrap();
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert!(
            validator.is_valid(&value),
            "actual writer does not match serialization schema: {:?}",
            validator
                .iter_errors(&value)
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
        );
        let decoded: T = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            serde_json::to_string(&decoded).unwrap(),
            encoded,
            "durable encoding changed without a version change"
        );
    }
}

/// Explicit capture only: refuses any existing destination and never refreshes a fixture.
#[tokio::test]
#[ignore = "requires a new RSI_PERSISTENCE_CAPTURE destination"]
async fn capture_current_writer() {
    let root = PathBuf::from(
        std::env::var_os("RSI_PERSISTENCE_CAPTURE").expect("explicit new destination"),
    );
    std::fs::create_dir(&root).expect("capture destination must not exist");
    for scenario in [
        "mechanical",
        "domains",
        "generations",
        "model-tools",
        "program",
        "queue",
    ] {
        let path = root.join(scenario);
        let store = SqliteStore::open(&path).unwrap();
        let [accepted, event, terminal] = facts();
        match scenario {
            "mechanical" => {
                Box::pin(rsi_agent_testkit::assert_mechanical_store_contract(
                    &store,
                    header(scenario),
                    accepted,
                    event,
                    terminal,
                ))
                .await;
            }
            "domains" => {
                rsi_agent_testkit::assert_domain_store_contract(
                    &store,
                    header(scenario),
                    accepted,
                    event,
                )
                .await;
            }
            "generations" => {
                rsi_agent_testkit::assert_generation_store_contract(
                    &store,
                    header(scenario),
                    accepted,
                )
                .await;
            }
            "model-tools" => execution::capture(&store, &path).await,
            "program" => {
                rsi_agent_testkit::assert_program_store_contract(
                    &store,
                    &header(scenario),
                    &accepted,
                )
                .await;
            }
            "queue" => {
                rsi_agent_testkit::assert_queue_store_contract(&store, header(scenario)).await;
            }
            _ => unreachable!(),
        }
        drop(store);
        SqliteStore::verify(&path).unwrap();
    }
    let baseline = Baseline {
        session_format: SESSION_FORMAT_VERSION,
        store_schema: AGENT_STORE_SCHEMA_VERSION,
        schema_sha256: schema_hash(&root.join("mechanical")),
        files: file_hashes(&root),
    };
    for name in baseline.files.keys() {
        let path = root.join(name);
        let output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join(format!("{name}.gz")))
            .unwrap();
        let mut encoder = flate2::GzBuilder::new()
            .mtime(0)
            .write(output, flate2::Compression::best());
        encoder.write_all(&std::fs::read(&path).unwrap()).unwrap();
        encoder.finish().unwrap();
        std::fs::remove_file(path).unwrap();
    }
    std::fs::write(
        root.join("baseline.json"),
        serde_json::to_vec_pretty(&baseline).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn feedback_reopens_and_checks_local_answer_predecessors() {
    use rsi_agent_session_protocol::FeedbackRating;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("feedback");
    let store = SqliteStore::open(&path).unwrap();
    execution::capture(&store, &path).await;
    let id = header("model-tools").session_id().clone();
    let message = 11.try_into().unwrap();
    let saved = store.read_message_feedback(&id, message).await.unwrap();
    let first = saved.current.unwrap().version.get();
    for (target, predecessor, rating) in [
        (11, None, Some("negative")),
        (11, Some(first), Some("positive")),
        (1, None, Some("positive")),
        (99, None, Some("negative")),
        (5, None, None),
    ] {
        assert!(matches!(
            execution::feedback_change(&store, &id, target, predecessor, rating).await,
            Err(StoreError::Invalid(_))
        ));
    }
    assert_eq!(
        store
            .read_watermarks(&id)
            .await
            .unwrap()
            .durable_control_seq,
        first
    );
    let second = execution::feedback_change(&store, &id, 11, Some(first), Some("negative"))
        .await
        .unwrap();
    assert!(
        execution::feedback_change(&store, &id, 11, Some(first), None)
            .await
            .is_err()
    );
    execution::feedback_change(&store, &id, 11, Some(second), None)
        .await
        .unwrap();
    assert!(
        store
            .read_message_feedback(&id, message)
            .await
            .unwrap()
            .current
            .is_none()
    );
    let third = execution::feedback_change(&store, &id, 11, None, Some("positive"))
        .await
        .unwrap();
    assert!(third > second);
    drop(store);
    SqliteStore::verify(&path).unwrap();
    let store = SqliteStore::open(&path).unwrap();
    let reloaded = store.read_message_feedback(&id, message).await.unwrap();
    let item = reloaded.current.unwrap();
    assert_eq!(item.version.get(), third);
    assert_eq!(item.value.rating, FeedbackRating::Positive);
    assert_eq!(serde_json::to_value(item.value.note).unwrap(), "  多字节  ");
    assert_eq!(
        reloaded.watermarks.durable_fact_seq,
        saved.watermarks.durable_fact_seq
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Each retained corpus is checked for byte identity, public reads and fork boundaries."
)]
async fn retained_writer_corpus() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/persistence");
    let mut corpora = std::fs::read_dir(fixtures)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    corpora.sort();
    assert!(!corpora.is_empty());
    let mut current = false;
    for root in corpora {
        let baseline: Baseline =
            serde_json::from_slice(&std::fs::read(root.join("baseline.json")).unwrap()).unwrap();
        current |= baseline.store_schema == AGENT_STORE_SCHEMA_VERSION
            && baseline.session_format == SESSION_FORMAT_VERSION;
        assert_eq!(
            file_hashes(&root),
            baseline.files,
            "retained writer bytes changed"
        );
        for entry in std::fs::read_dir(&root).unwrap() {
            let entry = entry.unwrap();
            if !entry.file_type().unwrap().is_dir() {
                continue;
            }
            let path = entry.path();
            // Verification owns a writer lease; run it on a disposable copy so fixtures stay read-only.
            let copy = tempfile::tempdir().unwrap();
            copy_tree(&path, copy.path());
            let before = file_hashes(copy.path());
            let outcome = SqliteStore::verify(copy.path());
            if baseline.store_schema != AGENT_STORE_SCHEMA_VERSION
                || baseline.session_format != SESSION_FORMAT_VERSION
            {
                if baseline.store_schema == AGENT_STORE_SCHEMA_VERSION {
                    assert!(matches!(outcome, Err(StoreError::Corrupt(message))
                        if message.contains(&format!("unsupported session format version {}", baseline.session_format))));
                } else {
                    assert!(
                        matches!(outcome, Err(StoreError::SchemaMismatch { expected, actual })
                        if expected == AGENT_STORE_SCHEMA_VERSION && actual == baseline.store_schema)
                    );
                }
                assert_eq!(
                    file_hashes(copy.path()),
                    before,
                    "refusal modified the copied corpus"
                );
                continue;
            }
            outcome.unwrap();
            assert_eq!(schema_hash(copy.path()), baseline.schema_sha256);
            let db = connection(copy.path());
            assert_encoding::<SessionHeader>(
                &db,
                "SELECT header_json FROM sessions ORDER BY session_id",
            );
            // Mailbox claims have a physical reference encoding, reconstructed and
            // digest-checked by SqliteStore::verify above, not SessionFact serde.
            assert_encoding::<SessionFact>(
                &db,
                "SELECT fact_json FROM facts WHERE acceptance_control_seq IS NULL ORDER BY session_id, seq",
            );
            assert_encoding::<AgentControlRecord>(
                &db,
                "SELECT control_json FROM agent_controls ORDER BY session_id, seq",
            );
            drop(db);
            // Cold admission and public queries consume the captured database;
            // no executor, Tool or provider exists in this verification process.
            let store = SqliteStore::open(copy.path()).unwrap();
            let mut after = None;
            loop {
                let page = store.list_sessions(after.as_ref(), 64).await.unwrap();
                page.validate().unwrap();
                for session in &page.sessions {
                    let _proof = store.prepare_session(session).await.unwrap();
                    let header = store.header(session).await.unwrap();
                    assert_eq!(header.session_id(), session);
                    let mut facts = Vec::new();
                    loop {
                        let page = store
                            .read_facts(
                                session,
                                facts.last().map_or(0, SessionFact::seq),
                                64,
                                8 * 1024 * 1024,
                            )
                            .await
                            .unwrap();
                        page.validate().unwrap();
                        let caught_up = page.caught_up();
                        facts.extend(page.facts);
                        if caught_up {
                            break;
                        }
                    }
                    if entry.file_name() == "model-tools" {
                        let last = facts.last().unwrap();
                        let boundary = store
                            .resolve_fork_boundary(
                                session,
                                &rsi_agent_session_protocol::ForkPoint::through(
                                    last.body().turn_id().expect("turn-owned fixture"),
                                ),
                                rsi_agent_session_protocol::ForkTurnSelection::All,
                            )
                            .await
                            .unwrap();
                        assert_eq!(boundary.resolved_terminal_seq, last.seq());
                        assert_eq!(
                            boundary.terminal_prefix_sha256,
                            rsi_agent_session_protocol::fact_prefix_sha256(&facts).unwrap()
                        );
                        assert!(facts.iter().any(|fact| matches!(fact.body(),
                            SessionFactBody::ToolResult {result,..}
                                if result.spills.len() == 1 && result.spills[0].bytes == 100_000)));
                    }
                }
                if !page.has_more {
                    break;
                }
                after = page.sessions.last().cloned();
            }
        }
        assert_eq!(file_hashes(&root), baseline.files);
    }
    assert!(
        current,
        "current Session/Store formats require actual writer evidence"
    );
}

fn copy_tree(source: &Path, destination: &Path) {
    #[cfg(windows)]
    let directory =
        rsi_files_native_fs::set_private_directory_permissions_no_follow(destination).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_str().unwrap();
        let target = destination.join(name.strip_suffix(".gz").unwrap_or(name));
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir(&target).unwrap();
            copy_tree(&entry.path(), &target);
        } else {
            let (_, bytes) = fixture_bytes(name, &entry.path());
            std::fs::write(&target, bytes).unwrap();
            #[cfg(windows)]
            rsi_files_native_fs::set_relative_private_file_permissions_no_follow(
                &directory,
                Path::new(target.file_name().unwrap()),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
    }
}

fn fixture_bytes(name: &str, path: &Path) -> (String, Vec<u8>) {
    if let Some(name) = name.strip_suffix(".gz") {
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap())
            .read_to_end(&mut bytes)
            .unwrap();
        (name.to_owned(), bytes)
    } else {
        (name.to_owned(), std::fs::read(path).unwrap())
    }
}
