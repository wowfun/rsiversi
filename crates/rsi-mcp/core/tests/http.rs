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

#[tokio::test]
async fn sse_exchange_and_watch_accept_legal_framing_independently_of_writes() {
    for modern in [false, true] {
        for ending in ["\n", "\r", "\r\n"] {
            for chunk in [1, usize::MAX] {
                let fixture = HttpFixture::start(Mode {
                    modern,
                    watch: true,
                    sse: true,
                    sse_ending: Some(ending),
                    sse_chunk: Some(chunk),
                    sse_bom: true,
                    sse_notifications: 65,
                    sse_tail: true,
                    ..Mode::default()
                })
                .await;
                let service = fixture.service(Arc::new(Credentials::default()));
                service.configure(fixture.config()).await.unwrap();
                let frozen = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    service.refresh("fixture", CancellationToken::new()),
                )
                .await
                .unwrap()
                .unwrap();
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    service.call(
                        &frozen,
                        "echo",
                        json!({"message":"中文"}),
                        CancellationToken::new(),
                    ),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(result["content"][0]["text"], "中文");
                fixture.events.send(()).unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    while service.status()[0].ready {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                assert_eq!(service.status()[0].error, Some(McpError::CatalogChanged));
                service.shutdown().await;
                fixture.shutdown().await;
            }
        }
    }
}

#[tokio::test]
async fn streaming_watch_ignores_large_comment_traffic_but_remains_cancellable() {
    for modern in [false, true] {
        let fixture = HttpFixture::start(Mode {
            modern,
            watch: true,
            sse_padding: 100_000,
            sse_chunk: Some(usize::MAX),
            ..Mode::default()
        })
        .await;
        let service = fixture.service(Arc::new(Credentials::default()));
        service.configure(fixture.config()).await.unwrap();
        service
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
        assert_eq!(service.status()[0].error, Some(McpError::CatalogChanged));
        service.shutdown().await;
        fixture.shutdown().await;
    }
}

#[tokio::test]
#[ignore = "opt-in controlled single-flight measurement; no timing pass threshold"]
async fn measure_single_flight_busy_cancellation_and_rediscovery() {
    single_flight_cases(&[1, 2, 8, 32], true).await;
}

#[tokio::test]
async fn concurrent_call_rejection_and_cancelled_mutation_never_replay() {
    single_flight_cases(&[2], false).await;
}

async fn single_flight_cases(caller_counts: &[usize], report: bool) {
    for modern in [false, true] {
        for &callers in caller_counts {
            single_flight_case(modern, callers, report).await;
        }
    }
}

async fn single_flight_case(modern: bool, callers: usize, report: bool) {
    let fixture = HttpFixture::start(Mode {
        modern,
        wait_call: true,
        ..Mode::default()
    })
    .await;
    let credentials = Arc::new(Credentials::default());
    let service = Arc::new(fixture.service(credentials.clone()));
    let mut config = fixture.config();
    if let TransportConfig::StreamableHttp { credential, .. } = &mut config.servers[0].transport {
        *credential =
            Some(rsi_credentials_protocol::CredentialRef::new("rsi.mcp", "fixture").unwrap());
    }
    service.configure(config).await.unwrap();
    let frozen = service
        .refresh("fixture", CancellationToken::new())
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let running = {
        let service = service.clone();
        let frozen = frozen.clone();
        let stop = cancel.clone();
        tokio::spawn(async move {
            service
                .call(&frozen, "echo", json!({"message":"mutation"}), stop)
                .await
        })
    };
    fixture.started.notified().await;
    let mut waiting = tokio::task::JoinSet::new();
    for _ in 1..callers {
        let service = service.clone();
        let frozen = frozen.clone();
        waiting.spawn(async move {
            let begin = std::time::Instant::now();
            let result = service
                .call(
                    &frozen,
                    "echo",
                    json!({"message":"peer"}),
                    CancellationToken::new(),
                )
                .await;
            (result, begin.elapsed().as_nanos())
        });
    }
    let mut busy_ns = vec![];
    while let Some(result) = waiting.join_next().await {
        let (result, time) = result.unwrap();
        assert_eq!(result, Err(McpError::Busy));
        busy_ns.push(time);
    }
    busy_ns.sort_unstable();
    let begin = std::time::Instant::now();
    cancel.cancel();
    assert_eq!(running.await.unwrap(), Err(McpError::Cancelled));
    let cancel_ns = begin.elapsed().as_nanos();
    assert!(!service.status()[0].ready);
    assert_eq!(
        service
            .call(
                &frozen,
                "echo",
                json!({"message":"stale"}),
                CancellationToken::new()
            )
            .await,
        Err(McpError::Disconnected)
    );
    fixture.release.notify_one();
    fixture.mode.lock().unwrap().wait_call = false;
    let requests_before = credentials.resolutions.load(Ordering::Acquire);
    let begin = std::time::Instant::now();
    service
        .refresh("fixture", CancellationToken::new())
        .await
        .unwrap();
    let refresh_ns = begin.elapsed().as_nanos();
    let refresh_requests = credentials.resolutions.load(Ordering::Acquire) - requests_before;
    assert_eq!(
        fixture.calls.load(Ordering::Acquire),
        1,
        "dispatched mutation must never replay"
    );
    if report {
        eprintln!(
            "single_flight {}",
            json!({"modern":modern,"callers":callers,"admitted":1,"busy":busy_ns.len(),"busy_ns":busy_ns,"cancel_ns":cancel_ns,"rediscovery_ns":refresh_ns,"rediscovery_requests":refresh_requests,"mutation_dispatches":1})
        );
    }
    service.shutdown().await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn first_sse_response_survives_duplicate_and_mismatched_coalesced_tail() {
    for modern in [false, true] {
        let fixture = HttpFixture::start(Mode {
            modern,
            sse: true,
            sse_tail: true,
            sse_chunk: Some(usize::MAX),
            ..Mode::default()
        })
        .await;
        let service = fixture.service(Arc::new(Credentials::default()));
        service.configure(fixture.config()).await.unwrap();
        let frozen = service
            .refresh("fixture", CancellationToken::new())
            .await
            .unwrap();
        let value = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            service.call(
                &frozen,
                "echo",
                json!({"message":"first response"}),
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(value.to_string().contains("first response"));
        assert!(!value.to_string().contains("poison"));
        assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
        service.shutdown().await;
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn sse_first_response_at_the_total_limit_ignores_coalesced_trailing_bytes() {
    let fixture = HttpFixture::start(Mode {
        modern: true,
        sse: true,
        sse_near_limit: true,
        sse_tail: true,
        sse_chunk: Some(usize::MAX),
        ..Mode::default()
    })
    .await;
    let service = fixture.service(Arc::new(Credentials::default()));
    service.configure(fixture.config()).await.unwrap();
    let manifest = service
        .refresh("fixture", CancellationToken::new())
        .await
        .unwrap();
    let value = service
        .call(
            &manifest,
            "echo",
            json!({"message":"near-limit"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(value["content"][0]["text"], "near-limit");
    service.shutdown().await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn sse_prefix_overflow_still_rejects_without_publishing_a_catalog() {
    let fixture = HttpFixture::start(Mode {
        modern: true,
        sse: true,
        sse_padding: 100_000,
        sse_chunk: Some(usize::MAX),
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
        McpError::Capacity
    );
    assert!(!service.status()[0].ready);
    service.shutdown().await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn oversized_encoded_request_fails_before_http_dispatch_and_keeps_readiness() {
    let fixture = HttpFixture::start(Mode::default()).await;
    let service = fixture.service(Arc::new(Credentials::default()));
    service.configure(fixture.config()).await.unwrap();
    let manifest = service
        .refresh("fixture", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        service
            .call(
                &manifest,
                "echo",
                json!({"message":"x".repeat(rsi_mcp::MAXIMUM_FRAME_BYTES)}),
                CancellationToken::new()
            )
            .await,
        Err(McpError::Capacity)
    );
    assert_eq!(fixture.calls.load(Ordering::Acquire), 0);
    assert!(service.status()[0].ready);
    service.shutdown().await;
    fixture.shutdown().await;
}
