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
    let service = Arc::new(McpService::new(
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
                tools: vec!["echo".into()],
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
    service.shutdown().await;
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
    service.shutdown().await;
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
    service.shutdown().await;
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
    let pid = std::fs::read_to_string(&marker).unwrap();
    cancel.cancel();
    assert_eq!(call.await.unwrap().unwrap_err(), McpError::Cancelled);
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
    service.shutdown().await;
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
    service.shutdown().await;
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
    service.shutdown().await;
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(
        result
            .expect("stdout pump waited on its own blocked writer")
            .unwrap()["content"][0]["text"],
        text
    );
}
