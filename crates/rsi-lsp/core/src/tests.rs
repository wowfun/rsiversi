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
// Independent test providers still share LocalFiles' process-wide job budget.
static FILE_FIXTURES: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(rsi_files_protocol::MAXIMUM_FILE_JOBS);
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
#[derive(Debug, Default)]
struct SettlementGate {
    used: std::sync::atomic::AtomicBool,
    entered: CancellationToken,
    release: CancellationToken,
}
#[derive(Debug)]
struct FailedSettlement {
    process: rsi_process::ManagedDuplexProcess,
    joins: Arc<std::sync::atomic::AtomicUsize>,
    gate: Option<Arc<SettlementGate>>,
}
#[async_trait::async_trait]
impl rsi_process::DuplexControl for FailedSettlement {
    fn pid(&self) -> u32 {
        self.process.pid()
    }
    fn stdin(&self) -> Arc<dyn rsi_process::DuplexInput> {
        self.process.stdin()
    }
    fn stdout(&self) -> Arc<dyn rsi_process::DuplexOutput> {
        self.process.stdout()
    }
    fn stderr(&self) -> Arc<dyn rsi_process::ProcessOutput> {
        self.process.stderr()
    }
    fn terminate(&self) {
        self.process.terminate();
    }
    async fn wait(&self) -> rsi_process::Result<rsi_process::ProcessOutcome> {
        self.process.wait().await
    }
    async fn wait_settlement(&self) -> rsi_process::Result<()> {
        if let Some(gate) = &self.gate
            && !gate.used.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            gate.entered.cancel();
            gate.release.cancelled().await;
        }
        self.process.wait_settlement().await?;
        if self.joins.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
            Err(rsi_process::ProcessError::SettlementTimeout)
        } else {
            Ok(())
        }
    }
}
#[derive(Debug)]
struct FailingProcess {
    inner: Arc<dyn rsi_process::DuplexProcess>,
    joins: Arc<std::sync::atomic::AtomicUsize>,
    gate: Option<Arc<SettlementGate>>,
}
impl rsi_process::DuplexProcess for FailingProcess {
    fn spawn(
        &self,
        spec: rsi_process::DuplexProcessSpec,
    ) -> rsi_process::Result<rsi_process::ManagedDuplexProcess> {
        Ok(rsi_process::ManagedDuplexProcess::new(Arc::new(
            FailedSettlement {
                process: self.inner.spawn(spec)?,
                joins: self.joins.clone(),
                gate: self.gate.clone(),
            },
        )))
    }
}
struct Fixture {
    _file_budget: tokio::sync::SemaphorePermit<'static>,
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
        Self::with_settlement_failure(mode, None).await
    }
    async fn with_settlement_failure(
        mode: &str,
        joins: Option<Arc<std::sync::atomic::AtomicUsize>>,
    ) -> Self {
        Self::with_controls(mode, joins, None).await
    }
    async fn with_controls(
        mode: &str,
        joins: Option<Arc<std::sync::atomic::AtomicUsize>>,
        gate: Option<Arc<SettlementGate>>,
    ) -> Self {
        let file_budget = FILE_FIXTURES.acquire().await.unwrap();
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
            configuration: if mode == "reply-pressure" {
                serde_json::json!({"payload":"c".repeat(12000)})
            } else {
                serde_json::Value::Null
            },
        };
        let inner = runtime
            .root()
            .lookup_local::<rsi_process::DuplexProcessContract>()
            .unwrap();
        let provider = joins.map_or_else(
            || inner.clone(),
            |joins| {
                Arc::new(FailingProcess {
                    inner: inner.clone(),
                    joins,
                    gate,
                }) as Arc<dyn rsi_process::DuplexProcess>
            },
        );
        let service = LanguageService::new(
            config,
            provider,
            Arc::new(TestSandbox),
            files.clone(),
            runtime.execution().clone(),
        )
        .unwrap();
        Self {
            _file_budget: file_budget,
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
        tokio::time::timeout(Duration::from_secs(10), self.service.close())
            .await
            .expect("LSP close deadline")
            .unwrap();
        for record in self.records().iter().filter(|r| r["event"] == "start") {
            assert!(
                !Path::new(&format!("/proc/{}", record["pid"])).exists(),
                "actual child must be reaped"
            );
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            self.files.close().await;
            assert!(self.process.dispose().await.is_clean());
            assert!(self.runtime.shutdown().await.is_clean());
        })
        .await
        .expect("LSP fixture cleanup deadline");
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn under_limit_bidirectional_pipe_pressure_completes_and_reaps() {
    for mode in ["duplex-pressure", "reply-pressure"] {
        let fixture = Fixture::new(mode).await;
        std::fs::write(fixture.workspace.join("main.rs"), "x".repeat(512 * 1024)).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(8),
            fixture.service.query(
                fixture.workspace.clone(),
                Fixture::query(Operation::Hover),
                CancellationToken::new(),
            ),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(10), fixture.service.close())
            .await
            .expect("LSP close deadline")
            .unwrap();
        assert!(
            matches!(result, Ok(Ok(_))),
            "under-limit duplex exchange: {result:?}"
        );
        assert!(
            fixture
                .records()
                .iter()
                .any(|record| record["event"] == "burst_complete")
        );
        if mode == "reply-pressure" {
            assert_eq!(
                fixture
                    .records()
                    .iter()
                    .filter(|record| record["event"] == "configuration_reply")
                    .count(),
                32
            );
        }
        fixture.close().await;
    }
}

#[tokio::test]
async fn idle_connection_answers_requests_without_a_new_query() {
    let fixture = Fixture::new("idle-request").await;
    fixture
        .service
        .query(
            fixture.workspace.clone(),
            Fixture::query(Operation::Hover),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !fixture
            .records()
            .iter()
            .any(|record| record["event"] == "idle_reply")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fixture
        .service
        .query(
            fixture.workspace.clone(),
            Fixture::query(Operation::Definition),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|record| record["event"] == "start")
            .count(),
        1
    );
    fixture.close().await;
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
    let mut task = tokio::spawn({
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
    tokio::select! {
        () = fixture.entered() => {},
        result = &mut task => panic!("query finished before peer entry: {result:?}; records={:?}", fixture.records()),
    }
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
    tokio::time::timeout(Duration::from_secs(10), fixture.service.close())
        .await
        .expect("LSP close deadline")
        .unwrap();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_settlement_is_retained_without_masking_query_failure() {
    let joins = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = Fixture::with_settlement_failure("server-error", Some(joins.clone())).await;
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        fixture.service.query(
            fixture.workspace.clone(),
            Fixture::query(Operation::Hover),
            CancellationToken::new(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(result.unwrap_err(), Error::Server(-32801));
    assert!(fixture.service.retired());
    assert_eq!(joins.load(std::sync::atomic::Ordering::SeqCst), 1);
    let second = fixture
        .service
        .query(
            fixture.workspace.clone(),
            Fixture::query(Operation::Hover),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(second.unwrap_err(), Error::Retired);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), fixture.service.close())
            .await
            .unwrap(),
        Err(Error::Unavailable)
    );
    assert_eq!(joins.load(std::sync::atomic::Ordering::SeqCst), 2);
    fixture.close().await;
    assert_eq!(
        joins.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "close retries the same retained control"
    );
}

#[tokio::test]
async fn dropping_close_keeps_unvisited_connections_owned_for_the_next_join() {
    let joins = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = Fixture::with_settlement_failure("normal", Some(joins.clone())).await;
    let second = fixture.temporary.path().join("second");
    std::fs::create_dir(&second).unwrap();
    std::fs::copy(fixture.workspace.join("main.rs"), second.join("main.rs")).unwrap();
    for workspace in [fixture.workspace.clone(), second] {
        fixture
            .service
            .query(
                workspace,
                Fixture::query(Operation::Hover),
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }
    tokio::task::yield_now().await;
    let mut close = Box::pin(fixture.service.close());
    assert!(futures_util::poll!(&mut close).is_pending()); // first pump join cannot complete without yielding this runtime
    drop(close);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), fixture.service.close())
            .await
            .unwrap(),
        Err(Error::Unavailable)
    );
    fixture.close().await;
    assert_eq!(
        joins.load(std::sync::atomic::Ordering::SeqCst),
        4,
        "both controls must be joined, including the unvisited slot"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retiring_workspace_stays_reserved_until_its_managed_process_settles() {
    let gate = Arc::new(SettlementGate::default());
    let fixture = Fixture::with_controls(
        "normal",
        Some(Arc::new(std::sync::atomic::AtomicUsize::new(2))),
        Some(gate.clone()),
    )
    .await;
    let mut workspaces = Vec::new();
    for index in 0..6 {
        let workspace = fixture.temporary.path().join(format!("pool-{index}"));
        std::fs::create_dir(&workspace).unwrap();
        std::fs::copy(fixture.workspace.join("main.rs"), workspace.join("main.rs")).unwrap();
        workspaces.push(workspace);
    }
    for workspace in &workspaces[..4] {
        fixture
            .service
            .query(
                workspace.clone(),
                Fixture::query(Operation::Hover),
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }
    let service = fixture.service.clone();
    let next = workspaces[4].clone();
    let replacement = tokio::spawn(async move {
        service
            .query(
                next,
                Fixture::query(Operation::Hover),
                CancellationToken::new(),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), gate.entered.cancelled())
        .await
        .unwrap();
    for workspace in [&workspaces[0], &workspaces[5]] {
        assert_eq!(
            fixture
                .service
                .query(
                    workspace.clone(),
                    Fixture::query(Operation::Hover),
                    CancellationToken::new()
                )
                .await
                .unwrap_err(),
            Error::Capacity
        );
    }
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|record| record["event"] == "start")
            .count(),
        4
    );
    gate.release.cancel();
    tokio::time::timeout(Duration::from_secs(5), replacement)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|record| record["event"] == "start")
            .count(),
        5
    );
    fixture.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_source_still_joins_failed_idle_process() {
    let joins = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = Fixture::with_settlement_failure("normal", Some(joins.clone())).await;
    fixture
        .service
        .query(
            fixture.workspace.clone(),
            Fixture::query(Operation::Hover),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let pid = fixture
        .records()
        .iter()
        .find(|record| record["event"] == "start")
        .unwrap()["pid"]
        .as_u64()
        .unwrap();
    assert!(
        std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while joins.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut query = Fixture::query(Operation::Hover);
    query.line = 9999;
    assert_eq!(
        fixture
            .service
            .query(fixture.workspace.clone(), query, CancellationToken::new())
            .await
            .unwrap_err(),
        Error::Unavailable
    );
    assert!(fixture.service.retired());
    assert_eq!(
        joins.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "first join observes the pump's retained failure"
    );
    assert_eq!(fixture.service.close().await, Err(Error::Unavailable));
    assert_eq!(joins.load(std::sync::atomic::Ordering::SeqCst), 2);
    fixture.close().await;
    assert_eq!(joins.load(std::sync::atomic::Ordering::SeqCst), 3);
}
