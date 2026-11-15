use rsi_extension::{ExtensionContract, ExtensionFactory, Handler};
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "opt-in installed Node executable via RSI_EXTENSION_NODE"]
async fn native_process_handshake_multiplex_cancellation_and_cleanup() {
    run_native(None).await;
}

#[tokio::test]
#[ignore = "opt-in installed Node and actual RSI_EXTENSION_DSH_TRANSPORT module"]
async fn actual_dsh_peer_preserves_control_progress_at_capacity() {
    run_native(Some(
        std::env::var("RSI_EXTENSION_DSH_TRANSPORT").expect("explicit DSH transport module"),
    ))
    .await;
}

#[derive(Debug, Default)]
struct Callbacks {
    entered: std::sync::atomic::AtomicUsize,
    changed: tokio::sync::Notify,
    ping: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl Handler for Callbacks {
    async fn call(
        &self,
        method: &str,
        _: serde_json::Value,
        _: CancellationToken,
    ) -> rsi_extension::Result<serde_json::Value> {
        match method {
            "entered" => {
                self.entered
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.changed.notify_one();
            }
            "ping" => self.ping.notify_one(),
            _ => return Err(rsi_extension::Error::Remote("unknown callback".into())),
        }
        Ok(serde_json::Value::Null)
    }
}

#[allow(clippy::too_many_lines)] // One process lifetime proves admission, wire progress and owned teardown.
async fn run_native(dsh: Option<String>) {
    let node = std::env::var("RSI_EXTENSION_NODE").expect("explicit installed Node path");
    let root = tempfile::tempdir().unwrap();
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
            json!({}),
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
                Arc::new(rsi_sandbox_local::SandboxLocalFactory::default()),
            ),
            json!({}),
        )
        .await
        .unwrap();
    #[cfg(windows)]
    let environment =
        std::collections::BTreeMap::from([("SystemRoot", std::env::var("SystemRoot").unwrap())]);
    #[cfg(not(windows))]
    let environment = std::collections::BTreeMap::<String, String>::new();
    let callbacks = Arc::new(Callbacks::default());
    let mut arguments = vec![
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(if dsh.is_some() {
                "tests/fixtures/dsh-peer.mjs"
            } else {
                "tests/fixtures/peer.mjs"
            })
            .to_str()
            .unwrap()
            .to_owned(),
        "native-test".into(),
    ];
    if let Some(module) = &dsh {
        arguments.push(module.clone());
    }
    let extension = runtime.root().apply(ResolvedFactory::linked("extension", "test", UpdateMode::Replayable,
        Arc::new(ExtensionFactory::new(callbacks.clone()))), json!({
            "program": node, "cwd": root.path(), "generation": "native-test", "environment": environment,
            "arguments": arguments
        })).await.unwrap();
    let connection = runtime.root().lookup_local::<ExtensionContract>().unwrap();
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let active = connection.clone();
    let hold = tokio::spawn(async move { active.request("hold", json!(null), token).await });
    let result = connection
        .request("echo", json!({"original":"中文"}), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result, json!({"original":"中文"}));
    connection
        .request("pause-input", json!(null), CancellationToken::new())
        .await
        .unwrap();
    let payload = json!({"original": "中文".repeat(90_000)});
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        connection.request("echo", payload.clone(), CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result, payload);
    assert!(started.elapsed() >= std::time::Duration::from_secs(1));
    cancellation.cancel();
    assert!(hold.await.unwrap().is_err());
    // The caller's cancellation completes before the peer acknowledges it.
    // A subsequent echo drains this fixture peer's cancellation reply first.
    connection
        .request("echo", json!(null), CancellationToken::new())
        .await
        .unwrap();
    if dsh.is_some() {
        let mut saturated = tokio::task::JoinSet::new();
        for _ in 0..64 {
            let connection = connection.clone();
            saturated.spawn(async move {
                connection
                    .request("hold", json!(null), CancellationToken::new())
                    .await
            });
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while callbacks.entered.load(std::sync::atomic::Ordering::SeqCst) != 65 {
                callbacks.changed.notified().await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            panic!(
                "business admission did not settle: {error}; entered={}; connection={connection:?}",
                callbacks.entered.load(std::sync::atomic::Ordering::SeqCst)
            )
        });
        tokio::time::timeout(std::time::Duration::from_secs(8), callbacks.ping.notified())
            .await
            .unwrap();
        assert!(matches!(
            connection
                .request("echo", json!(null), CancellationToken::new())
                .await,
            Err(rsi_extension::Error::Capacity)
        ));
        connection.shutdown().await.unwrap();
        while let Some(result) = saturated.join_next().await {
            assert!(result.unwrap().is_err());
        }
    }
    drop(connection);
    assert!(extension.dispose().await.is_clean());
    assert!(sandbox.dispose().await.is_clean());
    assert!(process.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
