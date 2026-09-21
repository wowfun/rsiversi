#![cfg(target_os = "linux")]
use super::*;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_sandbox::{Sandbox, SandboxMode};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;
#[derive(Debug)]
struct TestSandbox;
#[async_trait::async_trait]
impl Sandbox for TestSandbox {
    async fn workspace_read(
        &self,
        request: rsi_sandbox::WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
        assert_eq!(request.mode, SandboxMode::ReadOnly);
        rsi_sandbox::WorkspaceReadScope::new(request, rsi_sandbox::SandboxGeneration::default())
    }
    async fn confine(
        &self,
        request: rsi_sandbox::ProcessRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
        assert_eq!(request.mode, SandboxMode::ReadOnly);
        Ok(rsi_sandbox::ConfinedProcess {
            program: request.program,
            arguments: request.arguments.into_iter().map(Into::into).collect(),
            cwd: request.cwd,
            stdio: request.stdio,
            stamp: rsi_sandbox::EnforcementStamp {
                requested: request.mode,
                backend: rsi_sandbox::SandboxBackend::Unconfined,
                filesystem: rsi_sandbox::SandboxFileSystem::Unconfined,
                scratch: rsi_sandbox::SandboxScratch::Host,
                network: rsi_sandbox::SandboxNetwork::Host,
                workspace: request.workspace,
            },
            owner: None,
        })
    }
}
struct Fixture {
    temporary: tempfile::TempDir,
    runtime: Runtime,
    process: rsi_meta::FiberHandle,
    files: Arc<rsi_files::LocalFiles>,
    service: Arc<LanguageService>,
    workspace: PathBuf,
    log: PathBuf,
}
impl Fixture {
    async fn new(mode: &str) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(
            workspace.join("main.rs"),
            "fn main() { let _ = \"🦀\"; target(); }\nfn target() {}\n",
        )
        .unwrap();
        let log = temporary.path().join("peer.jsonl");
        let runtime = Runtime::default();
        let process = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "process",
                    "test",
                    UpdateMode::Replayable,
                    Arc::new(rsi_process_local::ProcessLocalFactory),
                ),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        let files = Arc::new(rsi_files::LocalFiles::new().unwrap());
        let config = Config {
            program: "/usr/bin/python3".into(),
            arguments: vec![
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../fixtures/rsi/lsp/test_peer.py")
                    .canonicalize()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .into(),
            ],
            environment: [
                ("LSP_TEST_LOG".into(), log.to_str().unwrap().into()),
                ("LSP_TEST_MODE".into(), mode.into()),
            ]
            .into(),
            languages: [(".rs".into(), "rust".into())].into(),
            initialization_options: serde_json::Value::Null,
            configuration: serde_json::Value::Null,
        };
        let service = LanguageService::new(
            config,
            runtime
                .root()
                .lookup_local::<rsi_process::DuplexProcessContract>()
                .unwrap(),
            Arc::new(TestSandbox),
            files.clone(),
            runtime.execution().clone(),
        )
        .unwrap();
        Self {
            temporary,
            runtime,
            process,
            files,
            service,
            workspace,
            log,
        }
    }
    fn query(operation: Operation) -> Query {
        Query {
            operation,
            path: "main.rs".into(),
            line: 1,
            column: 26,
        }
    }
    fn records(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self.records().iter().any(|r| r["event"] == "query") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn close(self) {
        self.service.close().await.unwrap();
        for record in self.records().iter().filter(|r| r["event"] == "start") {
            assert!(
                !Path::new(&format!("/proc/{}", record["pid"])).exists(),
                "actual child must be reaped"
            );
        }
        self.files.close().await;
        assert!(self.process.dispose().await.is_clean());
        assert!(self.runtime.shutdown().await.is_clean());
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_stdio_four_queries_sync_current_unicode_source_and_reject_server_edits() {
    let fixture = Fixture::new("edit").await;
    for operation in [
        Operation::Definition,
        Operation::References,
        Operation::Implementation,
        Operation::Hover,
    ] {
        let output = fixture
            .service
            .query(
                fixture.workspace.clone(),
                Fixture::query(operation),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        output_declaration()
            .unwrap()
            .validate_value(&serde_json::to_value(output).unwrap())
            .unwrap();
    }
    let records = fixture.records();
    let wire = records.iter().find(|r| r["event"] == "query").unwrap();
    assert_eq!(
        wire["params"]["position"]["character"], 26,
        "emoji adds a UTF-16 unit"
    );
    assert!(
        records
            .iter()
            .any(|r| r["params"]["context"]["includeDeclaration"] == true)
    );
    assert_eq!(
        records
            .iter()
            .filter(|r| r["event"] == "edit_rejected")
            .count(),
        4
    );
    std::fs::write(fixture.workspace.join("main.rs"), "fn changed() {}\n").unwrap();
    let mut query = Fixture::query(Operation::Hover);
    query.column = 4;
    fixture
        .service
        .query(fixture.workspace.clone(), query, CancellationToken::new())
        .await
        .unwrap();
    let records = fixture.records();
    assert_eq!(
        records.iter().filter(|r| r["event"] == "start").count(),
        1,
        "workspace reuses one live provider generation"
    );
    assert_eq!(
        records.iter().rfind(|r| r["event"] == "open").unwrap()["text"],
        "fn changed() {}\n"
    );
    fixture.close().await;
}
#[tokio::test]
async fn invalid_local_source_queries_keep_the_healthy_process() {
    let fixture = Fixture::new("edit").await;
    let query = Fixture::query(Operation::Hover);
    fixture
        .service
        .query(
            fixture.workspace.clone(),
            query.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    for invalid in [
        Query {
            path: "missing.rs".into(),
            ..query.clone()
        },
        Query {
            line: 9999,
            ..query.clone()
        },
        Query {
            column: 9999,
            ..query.clone()
        },
    ] {
        assert!(
            fixture
                .service
                .query(fixture.workspace.clone(), invalid, CancellationToken::new())
                .await
                .is_err()
        );
    }
    fixture
        .service
        .query(fixture.workspace.clone(), query, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|r| r["event"] == "start")
            .count(),
        1,
        "local validation cannot destroy a healthy workspace process"
    );
    fixture.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_hung_exited_and_invalid_responses_retire_actual_processes() {
    for (mode, operation, expected) in [
        ("oversize", Operation::Definition, Error::Limit),
        ("notifications", Operation::Definition, Error::Limit),
        ("hover-limit", Operation::Hover, Error::Limit),
        ("encoding", Operation::Definition, Error::Unsupported),
        ("wrong-id", Operation::Definition, Error::Protocol),
        ("escape", Operation::Definition, Error::Protocol),
        ("exit", Operation::Definition, Error::Unavailable),
        ("server-error", Operation::Hover, Error::Server(-32801)),
    ] {
        let fixture = Fixture::new(mode).await;
        assert_eq!(
            fixture
                .service
                .query(
                    fixture.workspace.clone(),
                    Fixture::query(operation),
                    CancellationToken::new()
                )
                .await,
            Err(expected),
            "{mode}"
        );
        fixture.close().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_waiter_keeps_real_process_owned_until_cancellation_and_retirement_drain() {
    let fixture = Fixture::new("hang").await;
    let task = tokio::spawn({
        let owner = fixture.service.clone();
        let workspace = fixture.workspace.clone();
        async move {
            owner
                .query(
                    workspace,
                    Fixture::query(Operation::Definition),
                    CancellationToken::new(),
                )
                .await
        }
    });
    fixture.entered().await;
    assert_eq!(
        fixture
            .service
            .query(
                fixture.workspace.clone(),
                Fixture::query(Operation::Hover),
                CancellationToken::new()
            )
            .await,
        Err(Error::Capacity)
    );
    task.abort();
    let _ = task.await;
    fixture.service.close().await.unwrap();
    assert!(fixture.records().iter().any(|r| r["event"] == "cancel"));
    assert_eq!(
        fixture
            .service
            .query(
                fixture.workspace.clone(),
                Fixture::query(Operation::Hover),
                CancellationToken::new()
            )
            .await,
        Err(Error::Retired)
    );
    fixture.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_limits_symlinks_and_idle_pool_eviction_are_bounded() {
    let fixture = Fixture::new("normal").await;
    std::os::unix::fs::symlink("/etc/passwd", fixture.workspace.join("linked.rs")).unwrap();
    let mut query = Fixture::query(Operation::Hover);
    query.path = "linked.rs".into();
    assert!(
        fixture
            .service
            .query(
                fixture.workspace.clone(),
                query.clone(),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    std::fs::write(
        fixture.workspace.join("large.rs"),
        vec![b'x'; 1024 * 1024 + 1],
    )
    .unwrap();
    query.path = "large.rs".into();
    assert_eq!(
        fixture
            .service
            .query(fixture.workspace.clone(), query, CancellationToken::new())
            .await,
        Err(Error::Limit)
    );
    assert!(
        fixture.records().is_empty(),
        "source admission precedes process launch"
    );
    for index in 0..6 {
        let workspace = fixture.temporary.path().join(format!("pool-{index}"));
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(
            workspace.join("main.rs"),
            "fn main() { let _ = \"🦀\"; target(); }\n",
        )
        .unwrap();
        fixture
            .service
            .query(
                workspace,
                Fixture::query(Operation::Definition),
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }
    let starts = fixture
        .records()
        .iter()
        .filter(|r| r["event"] == "start")
        .count();
    assert!(
        fixture
            .service
            .query(
                fixture.workspace.clone(),
                Query {
                    path: "missing.rs".into(),
                    ..Fixture::query(Operation::Hover)
                },
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    for index in 2..6 {
        fixture
            .service
            .query(
                fixture.temporary.path().join(format!("pool-{index}")),
                Fixture::query(Operation::Hover),
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|r| r["event"] == "start")
            .count(),
        starts,
        "invalid fifth workspace cannot evict a healthy process"
    );
    let records = fixture.records();
    let live = records
        .iter()
        .filter(|r| r["event"] == "start")
        .filter(|r| Path::new(&format!("/proc/{}", r["pid"])).exists())
        .count();
    assert_eq!(live, 4);
    fixture.close().await;
}
#[test]
fn unicode_scalar_and_utf16_positions_reject_mid_surrogates_and_foreign_locations() {
    let text = "a🦀界\r\nsecond\n";
    assert_eq!(
        position(text, 1, 3).unwrap(),
        Position {
            line: 0,
            character: 3
        }
    );
    assert_eq!(
        byte_offset(
            text,
            Position {
                line: 0,
                character: 3
            }
        )
        .unwrap(),
        5
    );
    assert!(
        byte_offset(
            text,
            Position {
                line: 0,
                character: 2
            }
        )
        .is_err()
    );
    assert!(position(text, 1, 5).is_err());
    assert!(protocol::relative("../source.rs").is_err());
    assert!(protocol::relative("C:/source.rs").is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit RSI_RUST_ANALYZER and native Linux Bubblewrap"]
#[expect(
    clippy::too_many_lines,
    reason = "complete native server setup, semantic assertions and retirement"
)]
async fn pinned_rust_analyzer_four_queries_under_native_read_only_sandbox() {
    let program = PathBuf::from(
        std::env::var_os("RSI_RUST_ANALYZER").expect("explicit pinned rust-analyzer"),
    );
    let version = std::process::Command::new(&program)
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap().trim(),
        "rust-analyzer 1.97.0 (2d8144b 2026-07-07)"
    );
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    let home = temporary.path().join("home");
    std::fs::create_dir_all(home.join("cargo")).unwrap();
    std::fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname='language_fixture'\nversion='0.1.0'\nedition='2024'\n[workspace]\n",
    )
    .unwrap();
    let source = "pub trait Greet { fn greet(&self) -> u32; }\npub struct Bird;\nimpl Greet for Bird { fn greet(&self) -> u32 { 7 } }\npub fn run() -> u32 { let _ = \"🦀\"; let bird = Bird; bird.greet() }\n";
    std::fs::write(workspace.join("src/lib.rs"), source).unwrap();
    std::fs::write(
        workspace.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"language_fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let runtime = Runtime::default();
    let process = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "process",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_process_local::ProcessLocalFactory),
            ),
            serde_json::json!({}),
        )
        .await
        .unwrap();
    let sandbox = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "sandbox",
                "test",
                UpdateMode::Replayable,
                Arc::new(
                    rsi_sandbox_local::SandboxLocalFactory::default().require_restricted_backend(),
                ),
            ),
            serde_json::json!({"bubblewrap":["/usr/bin/bwrap"],"landlock":[]}),
        )
        .await
        .unwrap();
    let files = Arc::new(rsi_files::LocalFiles::new().unwrap());
    let options = serde_json::json!({"cargo":{"buildScripts":{"enable":false},"sysroot":null},"procMacro":{"enable":false},"checkOnSave":false});
    let config = Config {
        program: program.clone(),
        arguments: vec![],
        environment: [
            (
                "PATH".into(),
                format!("{}:/usr/bin:/bin", program.parent().unwrap().display()),
            ),
            ("HOME".into(), home.to_str().unwrap().into()),
            (
                "CARGO_HOME".into(),
                home.join("cargo").to_str().unwrap().into(),
            ),
        ]
        .into(),
        languages: [(".rs".into(), "rust".into())].into(),
        initialization_options: options.clone(),
        configuration: serde_json::json!({"rust-analyzer":options}),
    };
    let service = LanguageService::new(
        config,
        runtime
            .root()
            .lookup_local::<rsi_process::DuplexProcessContract>()
            .unwrap(),
        runtime
            .root()
            .lookup_local::<rsi_sandbox::SandboxContract>()
            .unwrap(),
        files.clone(),
        runtime.execution().clone(),
    )
    .unwrap();
    let column = |line: usize, needle: &str| {
        u32::try_from(
            source
                .lines()
                .nth(line)
                .unwrap()
                .split_once(needle)
                .unwrap()
                .0
                .chars()
                .count()
                + 1,
        )
        .unwrap()
    };
    let queries = [
        (Operation::Definition, 4, column(3, "Bird")),
        (Operation::References, 2, column(1, "Bird")),
        (Operation::Implementation, 1, column(0, "Greet")),
        (Operation::Hover, 4, column(3, "Bird")),
    ];
    for (operation, line, column) in queries {
        let query = Query {
            operation,
            path: "src/lib.rs".into(),
            line,
            column,
        };
        let output = tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let output = service
                    .query(workspace.clone(), query.clone(), CancellationToken::new())
                    .await
                    .unwrap();
                let ready = match &output.result {
                    QueryResult::Locations { locations } => !locations.is_empty(),
                    QueryResult::Hover { text, .. } => !text.is_empty(),
                };
                if ready {
                    break output;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        match &output.result {
            QueryResult::Locations { locations } => {
                assert!(locations.iter().all(|l| l.path == "src/lib.rs"));
                if operation == Operation::Definition {
                    assert_eq!(locations[0].range.start.line, 1);
                }
                if operation == Operation::References {
                    assert!(locations.len() >= 3);
                }
                if operation == Operation::Implementation {
                    assert_eq!(locations[0].range.start.line, 2);
                }
            }
            QueryResult::Hover { text, .. } => assert!(text.contains("Bird")),
        }
        output_declaration()
            .unwrap()
            .validate_value(&serde_json::to_value(&output).unwrap())
            .unwrap();
        println!("{}", serde_json::to_string(&output).unwrap());
    }
    service.close().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(workspace.join("src/lib.rs")).unwrap(),
        source
    );
    files.close().await;
    assert!(sandbox.dispose().await.is_clean());
    assert!(process.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_error_survives_a_failed_document_close() {
    let fixture = Fixture::new("server-error-closed-input").await;
    assert_eq!(
        fixture
            .service
            .query(
                fixture.workspace.clone(),
                Fixture::query(Operation::Definition),
                CancellationToken::new()
            )
            .await,
        Err(Error::Server(-32801))
    );
    fixture.close().await;
}

#[tokio::test]
async fn indexing_notifications_do_not_consume_server_request_admission() {
    let fixture = Fixture::new("indexing").await;
    fixture
        .service
        .query(
            fixture.workspace.clone(),
            Fixture::query(Operation::Hover),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    fixture.close().await;
}
