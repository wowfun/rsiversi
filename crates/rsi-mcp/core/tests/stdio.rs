#![cfg(unix)]
#[allow(dead_code)]
mod support;
use rsi_mcp::{McpConfig, McpError, McpService, ServerConfig, TransportConfig};
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_process::DuplexProcessContract;
use serde_json::json;
use std::{collections::BTreeMap, path::Path, sync::Arc};
use support::{Credentials, TestSandbox};
use tokio_util::sync::CancellationToken;
async fn owner(directory: &Path, mode: &str) -> (Runtime, rsi_meta::FiberHandle, Arc<McpService>) {
    owner_selection(directory, mode, vec!["echo".into()], false).await
}
async fn owner_selection(
    directory: &Path,
    mode: &str,
    tools: Vec<String>,
    all: bool,
) -> (Runtime, rsi_meta::FiberHandle, Arc<McpService>) {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.process.local",
                "fixture",
                UpdateMode::Replayable,
                Arc::new(rsi_process_local::ProcessLocalFactory),
            ),
            json!({}),
        )
        .await
        .unwrap();
    let process = runtime
        .root()
        .lookup_local::<DuplexProcessContract>()
        .unwrap();
    let process: Arc<dyn rsi_process::DuplexProcess> = if mode == "settlement-failure" {
        Arc::new(FailedSettlementProcess(process))
    } else {
        process
    };
    let mode = if mode == "settlement-failure" {
        "modern"
    } else {
        mode
    };
    let constructor = if all {
        McpService::new_with_all_discovered_tools
    } else {
        McpService::new
    };
    let service = Arc::new(constructor(
        Arc::new(Credentials::default()),
        process,
        Arc::new(TestSandbox),
    ));
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/stdio.py");
    service
        .configure(McpConfig {
            servers: vec![ServerConfig {
                id: "stdio".into(),
                enabled: true,
                tools,
                transport: TransportConfig::Stdio {
                    program: "/usr/bin/python3".into(),
                    arguments: vec![
                        "-u".into(),
                        script.to_str().unwrap().into(),
                        mode.into(),
                        directory.join("started").to_str().unwrap().into(),
                    ],
                    cwd: directory.to_owned(),
                    environment: BTreeMap::new(),
                },
            }],
        })
        .await
        .unwrap();
    (runtime, fiber, service)
}

#[tokio::test]
async fn explicit_private_discovery_selects_tools_without_changing_configured_name_policy() {
    for all in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let (runtime, fiber, service) =
            owner_selection(directory.path(), "modern", vec![], all).await;
        let frozen = service
            .refresh("stdio", CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(frozen.tools.len(), 1);
        assert_eq!(frozen.tools[0].selected, all);
        let result = service
            .call(
                &frozen,
                "echo",
                json!({"message":"private owner"}),
                CancellationToken::new(),
            )
            .await;
        if all {
            assert_eq!(result.unwrap()["content"][0]["text"], "private owner");
        } else {
            assert_eq!(result.unwrap_err(), McpError::NotFound);
        }
        service.shutdown().await.unwrap();
        drop(service);
        assert!(fiber.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_clean());
    }
}
#[tokio::test]
async fn idle_stdout_half_close_invalidates_readiness_and_reaps_without_another_rpc() {
    let directory = tempfile::tempdir().unwrap();
    let gate = directory.path().join("started.close");
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(&gate)
            .status()
            .unwrap()
            .success()
    );
    let (runtime, fiber, service) = owner(directory.path(), "half-close").await;
    service
        .refresh("stdio", CancellationToken::new())
        .await
        .unwrap();
    assert!(service.status()[0].ready);
    tokio::task::spawn_blocking(move || std::fs::write(gate, b"x"))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while service.status()[0].ready {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("idle stdout close must invalidate readiness");
    assert_eq!(service.status()[0].error, Some(McpError::Disconnected));
    service.shutdown().await.unwrap();
    drop(service);
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn modern_stdio_discovers_subscribes_and_correlates_catalog_changes() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, fiber, service) = owner(directory.path(), "modern-changed").await;
    let frozen = service
        .refresh("stdio", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(frozen.protocol_version, "2026-07-28");
    assert_eq!(frozen.server_info["name"], "modern-stdio");
    // The result and catalog invalidation can arrive in the same stdout chunk.
    let result = service
        .call(
            &frozen,
            "echo",
            json!({"message":"modern stdio 中文"}),
            CancellationToken::new(),
        )
        .await;
    if let Ok(value) = result {
        assert_eq!(value["content"][0]["text"], "modern stdio 中文");
    } else {
        assert_eq!(result.unwrap_err(), McpError::CatalogChanged);
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while service.status()[0].ready {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(service.status()[0].error, Some(McpError::CatalogChanged));
    service.shutdown().await.unwrap();
    drop(service);
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn silent_legacy_stdio_probe_is_reaped_before_one_handshake_only_restart() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, fiber, service) = owner(directory.path(), "legacy-silent-probe").await;
    let frozen = service
        .refresh("stdio", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(frozen.protocol_version, "2025-11-25");
    let markers = std::fs::read_to_string(directory.path().join("started")).unwrap();
    let pids = markers.lines().collect::<Vec<_>>();
    assert_eq!(pids.len(), 2, "exactly one probe and one legacy handshake");
    assert_ne!(
        pids[0], pids[1],
        "late probe responses cannot share the new process"
    );
    #[cfg(target_os = "linux")]
    assert!(
        !Path::new(&format!("/proc/{}", pids[0])).exists(),
        "the first child must already be reaped"
    );
    let result = service
        .call(
            &frozen,
            "echo",
            json!({"message":"once"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "once");
    service.shutdown().await.unwrap();
    drop(service);
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn stdio_preserves_multiple_large_frames_and_drains_stderr_through_the_real_process_owner() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, fiber, service) = owner(directory.path(), "echo").await;
    let frozen = service
        .refresh("stdio", CancellationToken::new())
        .await
        .unwrap();
    for text in ["中文\n\u{0000}".repeat(40000), "second message".into()] {
        let value = service
            .call(
                &frozen,
                "echo",
                json!({"message":text}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(value["content"][0]["text"], text);
    }
    service.shutdown().await.unwrap();
    assert!(!service.status()[0].ready);
    drop(service);
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn stdio_cancelled_started_call_is_reaped_and_never_replayed() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, fiber, service) = owner(directory.path(), "stall").await;
    let frozen = service
        .refresh("stdio", CancellationToken::new())
        .await
        .unwrap();
    let marker = directory.path().join("started");
    let cancel = CancellationToken::new();
    let peer = service.clone();
    let schema = frozen.clone();
    let cancelled = cancel.clone();
    let call = tokio::spawn(async move {
        peer.call(&schema, "echo", json!({"message":"once"}), cancelled)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    #[cfg(target_os = "linux")]
    let pid = std::fs::read_to_string(&marker).unwrap();
    // A queued cancellation must neither write to stdin nor retire this epoch.
    let queued_cancel = CancellationToken::new();
    let mut queued = Box::pin(service.call(
        &frozen,
        "echo",
        json!({"message":"queued"}),
        queued_cancel.clone(),
    ));
    assert!(futures_util::poll!(&mut queued).is_pending());
    queued_cancel.cancel();
    assert_eq!(queued.await.unwrap_err(), McpError::Cancelled);
    assert!(service.status()[0].ready);
    let mut waiting = Vec::new();
    for _ in 0..8 {
        let mut request = Box::pin(service.call(
            &frozen,
            "echo",
            json!({"message":"never sent"}),
            CancellationToken::new(),
        ));
        assert!(futures_util::poll!(&mut request).is_pending());
        waiting.push(request);
    }
    assert_eq!(
        service
            .call(&frozen, "echo", json!({}), CancellationToken::new())
            .await
            .unwrap_err(),
        McpError::Busy
    );
    cancel.cancel();
    assert_eq!(call.await.unwrap().unwrap_err(), McpError::Cancelled);
    for request in waiting {
        assert_eq!(request.await.unwrap_err(), McpError::Disconnected);
    }
    assert_eq!(
        service
            .call(
                &frozen,
                "echo",
                json!({"message":"again"}),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        McpError::Disconnected
    );
    service.shutdown().await.unwrap();
    #[cfg(target_os = "linux")]
    assert!(
        !Path::new("/proc").join(pid).exists(),
        "shutdown must await actual child reaping"
    );
    drop(service);
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn oversized_unterminated_stdio_frame_closes_and_reaps_the_child_without_publishing() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, fiber, service) = owner(directory.path(), "oversize").await;
    assert_eq!(
        service
            .refresh("stdio", CancellationToken::new())
            .await
            .unwrap_err(),
        McpError::Capacity
    );
    assert!(service.manifest().is_err());
    assert!(service.status()[0].last_verified_sha256.is_none());
    service.shutdown().await.unwrap();
    drop(service);
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn dropping_a_refresh_waiter_keeps_admission_until_its_child_is_reaped() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, fiber, service) = owner(directory.path(), "initialize-stall").await;
    let peer = service.clone();
    let refresh =
        tokio::spawn(async move { peer.refresh("stdio", CancellationToken::new()).await });
    let marker = directory.path().join("started");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    #[cfg(target_os = "linux")]
    let pid = std::fs::read_to_string(marker).unwrap();
    refresh.abort();
    assert!(refresh.await.unwrap_err().is_cancelled());
    assert_eq!(
        service
            .refresh("stdio", CancellationToken::new())
            .await
            .unwrap_err(),
        McpError::Busy
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    #[cfg(target_os = "linux")]
    assert!(!Path::new("/proc").join(pid).exists());
    drop(service);
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn server_requests_cannot_stop_stdout_while_a_large_client_frame_is_writing() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, fiber, service) = owner(directory.path(), "duplex-cycle").await;
    let frozen = service
        .refresh("stdio", CancellationToken::new())
        .await
        .unwrap();
    let text = "x".repeat(512 * 1024);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        service.call(
            &frozen,
            "echo",
            json!({"message":text}),
            CancellationToken::new(),
        ),
    )
    .await;
    service.shutdown().await.unwrap();
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(
        result
            .expect("stdout pump waited on its own blocked writer")
            .unwrap()["content"][0]["text"],
        text
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancelled_call_waits_for_child_settlement_before_returning() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, fiber, service) = owner(directory.path(), "cancel-write").await;
    let frozen = service
        .refresh("stdio", CancellationToken::new())
        .await
        .unwrap();
    let stop = CancellationToken::new();
    let peer = service.clone();
    let cancellation = stop.clone();
    let mut call = tokio::spawn(async move {
        peer.call(&frozen, "echo", json!({"message":"once"}), cancellation)
            .await
    });
    let wait_file = |path: std::path::PathBuf| async move {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !path.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
    };
    wait_file(directory.path().join("started")).await;
    let pid = std::fs::read_to_string(directory.path().join("started")).unwrap();
    stop.cancel();
    // This causal barrier is held by the real child's SIGTERM handler.
    let early = tokio::select! {biased;result=&mut call=>Some(result),()=wait_file(directory.path().join("started.stopping"))=>None};
    let returned_early = early.is_some();
    wait_file(directory.path().join("started.stopping")).await;
    std::fs::write(directory.path().join("started.release"), b"release").unwrap();
    let result = match early {
        Some(result) => result,
        None => call.await,
    };
    let reaped = !Path::new("/proc").join(pid).exists();
    service.shutdown().await.unwrap();
    drop(service);
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
    assert!(
        !returned_early,
        "call returned while the controlled writer was still held"
    );
    assert_eq!(result.unwrap().unwrap_err(), McpError::Cancelled);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("started.late")).unwrap(),
        "settled write"
    );
    assert!(reaped, "MCP caller settlement precedes owner shutdown");
}

#[derive(Debug)]
struct FailedSettlementProcess(Arc<dyn rsi_process::DuplexProcess>);
impl rsi_process::DuplexProcess for FailedSettlementProcess {
    fn spawn(
        &self,
        spec: rsi_process::DuplexProcessSpec,
    ) -> rsi_process::Result<rsi_process::ManagedDuplexProcess> {
        self.0.spawn(spec).map(|process| {
            rsi_process::ManagedDuplexProcess::new(Arc::new(FailedSettlement(process)))
        })
    }
}
#[derive(Debug)]
struct FailedSettlement(rsi_process::ManagedDuplexProcess);
#[async_trait::async_trait]
impl rsi_process::DuplexControl for FailedSettlement {
    fn pid(&self) -> u32 {
        self.0.pid()
    }
    fn stdin(&self) -> Arc<dyn rsi_process::DuplexInput> {
        self.0.stdin()
    }
    fn stdout(&self) -> Arc<dyn rsi_process::DuplexOutput> {
        self.0.stdout()
    }
    fn stderr(&self) -> Arc<dyn rsi_process::ProcessOutput> {
        self.0.stderr()
    }
    fn terminate(&self) {
        self.0.terminate();
    }
    async fn wait(&self) -> rsi_process::Result<rsi_process::ProcessOutcome> {
        self.0.wait().await
    }
    async fn wait_settlement(&self) -> rsi_process::Result<()> {
        self.0.wait_settlement().await?;
        Err(rsi_process::ProcessError::Io(
            "fixture settlement failure".into(),
        ))
    }
}
#[tokio::test]
async fn failed_settlement_survives_connection_replacement_and_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, fiber, service) = owner(directory.path(), "settlement-failure").await;
    service
        .refresh("stdio", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        service
            .refresh("stdio", CancellationToken::new())
            .await
            .unwrap_err(),
        McpError::Disconnected
    );
    assert_eq!(
        service.configure(McpConfig { servers: vec![] }).await,
        Err(McpError::Disconnected)
    );
    service
        .configure(McpConfig { servers: vec![] })
        .await
        .expect(
            "an unrelated subsequent configuration is independent of historical cleanup failure",
        );
    assert_eq!(service.shutdown().await, Err(McpError::Disconnected));
    drop(service);
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
