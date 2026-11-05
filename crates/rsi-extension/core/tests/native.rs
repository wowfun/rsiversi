use rsi_extension::{ExtensionContract, ExtensionFactory, RejectRequests};
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "opt-in installed Node executable via RSI_EXTENSION_NODE"]
async fn native_process_handshake_multiplex_cancellation_and_cleanup() {
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
    let extension = runtime.root().apply(ResolvedFactory::linked("extension", "test", UpdateMode::Replayable,
        Arc::new(ExtensionFactory::new(Arc::new(RejectRequests)))), json!({
            "program": node, "cwd": root.path(), "generation": "native-test", "environment": environment,
            "arguments": [std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/peer.mjs"), std::path::PathBuf::from("native-test")]
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
    cancellation.cancel();
    assert!(hold.await.unwrap().is_err());
    drop(connection);
    assert!(extension.dispose().await.is_clean());
    assert!(sandbox.dispose().await.is_clean());
    assert!(process.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
