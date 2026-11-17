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
#[ignore = "opt-in installed Node and actual RSI_DSH_INSTALLED_COHORT"]
async fn actual_dsh_peer_preserves_control_progress_at_capacity() {
    let cohort = std::path::PathBuf::from(
        std::env::var("RSI_DSH_INSTALLED_COHORT").expect("explicit installed DSH cohort"),
    );
    assert!(cohort.is_absolute(), "installed cohort must be absolute");
    run_native(Some(cohort)).await;
}

#[derive(Debug, Default)]
struct Callbacks {
    entered: std::sync::atomic::AtomicUsize,
    changed: tokio::sync::Notify,
    ping: tokio::sync::Notify,
    body: std::sync::Mutex<Option<Vec<u8>>>,
}
#[async_trait::async_trait]
impl Handler for Callbacks {
    async fn call(
        &self,
        method: &str,
        payload: serde_json::Value,
        _: CancellationToken,
    ) -> rsi_extension::Result<serde_json::Value> {
        match method {
            "body.read" => {
                let body = self.body.lock().unwrap();
                assert_eq!(payload["token"], "a".repeat(32));
                return rsi_extension::body::fragment(
                    body.as_ref().unwrap(),
                    usize::try_from(payload["offset"].as_u64().unwrap()).unwrap(),
                );
            }
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
async fn run_native(dsh: Option<std::path::PathBuf>) {
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
    if let Some(cohort) = &dsh {
        for relative in [
            "node_modules/rsi-dsh-cordis/src/runtime/transport.mjs",
            "node_modules/rsi-dsh-plugin-bridge/src/request-body.mjs",
        ] {
            let module = cohort.join(relative);
            assert!(module.is_file(), "installed cohort is missing {relative}");
            arguments.push(module.to_string_lossy().into_owned());
        }
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
        for (expected, text, maximum) in [
            ("message", "\n".repeat(1024 * 1024), 8 * 1024 * 1024),
            ("tool", "界".repeat(1024 * 1024), 4 * 1024 * 1024),
            (
                "application-limit",
                "x".repeat(8 * 1024 * 1024 - 11),
                8 * 1024 * 1024,
            ),
        ] {
            let bytes = serde_json::to_vec(&json!({"text": text})).unwrap();
            assert!(bytes.len() <= maximum);
            let body = rsi_extension::body::Body::Paged {
                token: "a".repeat(32),
                bytes: bytes.len(),
            };
            *callbacks.body.lock().unwrap() = Some(bytes);
            assert_eq!(
                connection
                    .request(
                        "echo-body",
                        json!({"body":body,"expected":expected}),
                        CancellationToken::new()
                    )
                    .await
                    .unwrap(),
                json!(true)
            );
            callbacks.body.lock().unwrap().take();
        }
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
            Err(rsi_extension::RequestFailure::NotDispatched(
                rsi_extension::Error::Capacity
            ))
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
