mod support;
use rsi_mcp::{McpError, TransportConfig};
use serde_json::json;
use std::sync::{Arc, atomic::Ordering};
use support::*;
use tokio_util::sync::CancellationToken;
#[tokio::test]
async fn complete_discovery_preserves_metadata_and_credentials_are_resolved_for_each_request() {
    for sse in [false, true] {
        let fixture = HttpFixture::start(Mode {
            sse,
            ..Mode::default()
        })
        .await;
        let credentials = Arc::new(Credentials::default());
        let service = fixture.service(credentials.clone());
        let mut config = fixture.config();
        if let TransportConfig::StreamableHttp { credential, url } =
            &mut config.servers[0].transport
        {
            if sse {
                *url = url.replace("127.0.0.1", "localhost");
            }
            *credential =
                Some(rsi_credentials_protocol::CredentialRef::new("rsi.mcp", "fixture").unwrap());
        }
        service.configure(config).await.unwrap();
        assert_eq!(service.manifest().unwrap_err(), McpError::Disconnected);
        let frozen = service
            .refresh("fixture", CancellationToken::new())
            .await
            .unwrap();
        let manifest = service.manifest().unwrap();
        manifest.snapshot().unwrap();
        assert_eq!(&manifest.servers[0], frozen.manifest());
        assert_eq!(
            frozen.tools[0].definition.extensions["_meta"]["exact"].to_string(),
            "18446744073709551615"
        );
        assert_eq!(
            service
                .call(
                    &frozen,
                    "echo",
                    json!({"message":"binary-free 中文"}),
                    CancellationToken::new()
                )
                .await
                .unwrap()["content"][0]["text"],
            "binary-free 中文"
        );
        assert_eq!(
            service
                .resource(&frozen, "fixture://text", CancellationToken::new())
                .await
                .unwrap()["contents"][0]["text"],
            "Frozen-catalog resource 中文"
        );
        assert_eq!(
            service
                .resource(&frozen, "http://127.0.0.1/other", CancellationToken::new())
                .await
                .unwrap_err(),
            McpError::NotFound
        );
        assert_eq!(
            credentials.resolutions.load(Ordering::Acquire),
            fixture.credentials_seen.load(Ordering::Acquire)
        );
        assert!(credentials.resolutions.load(Ordering::Acquire) >= 7);
        service.shutdown().await;
        fixture.shutdown().await;
    }
}
#[tokio::test]
async fn reconnect_never_substitutes_old_schema_and_retains_last_verified_manifest() {
    let fixture = HttpFixture::start(Mode::default()).await;
    let service = fixture.service(Arc::new(Credentials::default()));
    service.configure(fixture.config()).await.unwrap();
    let old = service
        .refresh("fixture", CancellationToken::new())
        .await
        .unwrap();
    fixture.mode.lock().unwrap().changed = true;
    let new = service
        .refresh("fixture", CancellationToken::new())
        .await
        .unwrap();
    assert_ne!(old, new);
    assert_eq!(
        service
            .call(
                &old,
                "echo",
                json!({"message":"old"}),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        McpError::CatalogChanged
    );
    assert_eq!(fixture.calls.load(Ordering::Acquire), 0);
    fixture.mode.lock().unwrap().oversize = true;
    assert_eq!(
        service
            .refresh("fixture", CancellationToken::new())
            .await
            .unwrap_err(),
        McpError::Capacity
    );
    assert_eq!(
        service.status()[0].last_verified_sha256,
        Some(new.sha256().to_owned())
    );
    assert!(!service.status()[0].ready);
    assert!(service.manifest().is_err());
    service.shutdown().await;
    fixture.shutdown().await;
}
#[tokio::test]
async fn list_changed_invalidates_the_idle_epoch_before_a_tool_can_start() {
    let fixture = HttpFixture::start(Mode {
        watch: true,
        ..Mode::default()
    })
    .await;
    let service = fixture.service(Arc::new(Credentials::default()));
    service.configure(fixture.config()).await.unwrap();
    let frozen = service
        .refresh("fixture", CancellationToken::new())
        .await
        .unwrap();
    fixture.events.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while service.status()[0].ready {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        service
            .call(
                &frozen,
                "echo",
                json!({"message":"stale"}),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        McpError::CatalogChanged
    );
    assert_eq!(fixture.calls.load(Ordering::Acquire), 0);
    service.shutdown().await;
    fixture.shutdown().await;
}
#[tokio::test]
async fn cancellation_after_send_closes_epoch_and_does_not_replay_the_call() {
    let fixture = HttpFixture::start(Mode {
        wait_call: true,
        ..Mode::default()
    })
    .await;
    let service = Arc::new(fixture.service(Arc::new(Credentials::default())));
    service.configure(fixture.config()).await.unwrap();
    let frozen = service
        .refresh("fixture", CancellationToken::new())
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let peer = service.clone();
    let schema = frozen.clone();
    let cancellation = cancel.clone();
    let call = tokio::spawn(async move {
        peer.call(&schema, "echo", json!({"message":"once"}), cancellation)
            .await
    });
    fixture.started.notified().await;
    assert_eq!(
        service
            .call(
                &frozen,
                "echo",
                json!({"message":"overlap"}),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        McpError::Busy
    );
    cancel.cancel();
    assert_eq!(call.await.unwrap().unwrap_err(), McpError::Cancelled);
    fixture.release.notify_one();
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
    assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
    service.shutdown().await;
    fixture.shutdown().await;
}
#[tokio::test]
async fn malformed_correlation_and_repeated_cursors_never_publish_a_partial_manifest() {
    for mode in [
        Mode {
            wrong_id: true,
            ..Mode::default()
        },
        Mode {
            duplicate_cursor: true,
            ..Mode::default()
        },
    ] {
        let fixture = HttpFixture::start(mode).await;
        let service = fixture.service(Arc::new(Credentials::default()));
        service.configure(fixture.config()).await.unwrap();
        assert_eq!(
            service
                .refresh("fixture", CancellationToken::new())
                .await
                .unwrap_err(),
            McpError::Protocol
        );
        assert!(service.manifest().is_err());
        assert!(service.status()[0].last_verified_sha256.is_none());
        service.shutdown().await;
        fixture.shutdown().await;
    }
}
#[tokio::test]
async fn remote_error_remains_explicit_and_does_not_discard_a_verified_connection() {
    let fixture = HttpFixture::start(Mode {
        remote_error: true,
        ..Mode::default()
    })
    .await;
    let service = fixture.service(Arc::new(Credentials::default()));
    service.configure(fixture.config()).await.unwrap();
    let frozen = service
        .refresh("fixture", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        service
            .call(
                &frozen,
                "echo",
                json!({"message":"failure"}),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        McpError::RemoteError
    );
    assert!(service.status()[0].ready);
    service.shutdown().await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn encoded_responses_fail_before_decoding_or_manifest_publication() {
    let fixture = HttpFixture::start(Mode {
        encoded_response: true,
        ..Mode::default()
    })
    .await;
    let service = fixture.service(Arc::new(Credentials::default()));
    service.configure(fixture.config()).await.unwrap();
    assert_eq!(
        service
            .refresh("fixture", CancellationToken::new())
            .await
            .unwrap_err(),
        McpError::Protocol
    );
    assert!(service.manifest().is_err());
    assert_eq!(fixture.calls.load(Ordering::Acquire), 0);
    service.shutdown().await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn removed_endpoint_invalidates_frozen_calls_without_dispatch() {
    let fixture = HttpFixture::start(Mode::default()).await;
    let service = fixture.service(Arc::new(Credentials::default()));
    service.configure(fixture.config()).await.unwrap();
    let frozen = service
        .refresh("fixture", CancellationToken::new())
        .await
        .unwrap();
    service
        .configure(rsi_mcp::McpConfig::default())
        .await
        .unwrap();
    assert_eq!(
        service
            .call(
                &frozen,
                "echo",
                json!({"message":"removed"}),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        McpError::CatalogChanged
    );
    assert_eq!(
        service
            .refresh("fixture", CancellationToken::new())
            .await
            .unwrap_err(),
        McpError::NotFound
    );
    assert_eq!(fixture.calls.load(Ordering::Acquire), 0);
    service.shutdown().await;
    fixture.shutdown().await;
}
