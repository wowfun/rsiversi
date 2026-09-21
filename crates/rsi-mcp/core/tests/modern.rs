mod support;
use rsi_mcp::McpError;
use serde_json::json;
use std::sync::{Arc, atomic::Ordering};
use support::*;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn modern_started_calls_keep_fresh_credentials_and_cancel_without_replay() {
    let fixture = HttpFixture::start(Mode {
        modern: true,
        ..Mode::default()
    })
    .await;
    let credentials = Arc::new(Credentials::default());
    let service = Arc::new(fixture.service(credentials.clone()));
    let mut config = fixture.config();
    if let rsi_mcp::TransportConfig::StreamableHttp { credential, .. } =
        &mut config.servers[0].transport
    {
        *credential =
            Some(rsi_credentials_protocol::CredentialRef::new("rsi.mcp", "fixture").unwrap());
    }
    service.configure(config).await.unwrap();
    let frozen = service
        .refresh("fixture", CancellationToken::new())
        .await
        .unwrap();
    fixture.mode.lock().unwrap().wait_call = true;
    let cancel = CancellationToken::new();
    let waiting = fixture.started.notified();
    let call = tokio::spawn({
        let service = service.clone();
        let cancel = cancel.clone();
        async move {
            service
                .call(&frozen, "echo", json!({"message":"once"}), cancel)
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
        .await
        .unwrap();
    cancel.cancel();
    assert_eq!(call.await.unwrap(), Err(McpError::Cancelled));
    fixture.release.notify_one();
    assert!(!service.status()[0].ready);
    assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
    assert_eq!(
        credentials.resolutions.load(Ordering::Acquire),
        fixture.credentials_seen.load(Ordering::Acquire)
    );
    assert_eq!(
        fixture.credentials_seen.load(Ordering::Acquire),
        4,
        "discover, tools, resources and call each resolve credentials"
    );
    service.shutdown().await.unwrap();
    fixture.shutdown().await;
}

#[tokio::test]
async fn modern_http_metadata_headers_results_resources_and_subscription_use_real_wire() {
    for sse in [false, true] {
        let fixture = HttpFixture::start(Mode {
            modern: true,
            watch: true,
            sse,
            ..Mode::default()
        })
        .await;
        let service = fixture.service(Arc::new(Credentials::default()));
        service.configure(fixture.config()).await.unwrap();
        let frozen = service
            .refresh("fixture", CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(frozen.protocol_version, "2026-07-28");
        assert_eq!(frozen.server_info["name"], "modern-fixture");
        let result = service
            .call(
                &frozen,
                "echo",
                json!({"message":" 中文\r\n","nested":{"id":9_007_199_254_740_991_i64}}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result["structuredContent"], json!([true, null, "exact"]));
        assert_eq!(
            service
                .resource(&frozen, "fixture://中文", CancellationToken::new())
                .await
                .unwrap()["contents"][0]["text"],
            "resource text"
        );
        fixture.events.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while service.status()[0].ready {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(service.status()[0].error, Some(McpError::CatalogChanged));
        assert_eq!(
            service
                .call(
                    &frozen,
                    "echo",
                    json!({"message":"stale"}),
                    CancellationToken::new()
                )
                .await,
            Err(McpError::CatalogChanged)
        );
        assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
        service.shutdown().await.unwrap();
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn modern_discovery_errors_and_bad_subscription_acknowledgments_never_downgrade() {
    for (fault, expected) in [
        ("version", McpError::UnsupportedVersion),
        ("version-id", McpError::Protocol),
        ("header", McpError::HeaderMismatch),
        ("capability", McpError::RequiredCapability),
        ("cache", McpError::Protocol),
        ("subscription-id", McpError::Protocol),
        ("before-ack", McpError::Protocol),
    ] {
        let fixture = HttpFixture::start(Mode {
            modern: true,
            watch: true,
            modern_fault: Some(fault),
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
            expected,
            "{fault}"
        );
        assert!(!service.status()[0].ready);
        assert_eq!(fixture.calls.load(Ordering::Acquire), 0);
        service.shutdown().await.unwrap();
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn unfinished_or_unknown_modern_results_cannot_be_published_as_success_or_replayed() {
    for (fault, expected, ready) in [
        ("input-required", McpError::InputRequired, true),
        ("client-input", McpError::RequiredCapability, true),
        ("unknown-result", McpError::Protocol, false),
        ("missing-result", McpError::Protocol, false),
    ] {
        let fixture = HttpFixture::start(Mode {
            modern: true,
            modern_fault: Some(fault),
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
                    json!({"message":"once"}),
                    CancellationToken::new()
                )
                .await,
            Err(expected),
            "{fault}"
        );
        assert_eq!(service.status()[0].ready, ready, "{fault}");
        assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
        service.shutdown().await.unwrap();
        fixture.shutdown().await;
    }
}
