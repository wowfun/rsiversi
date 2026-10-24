mod support;
use futures_util::StreamExt as _;
use rsi_api_protocol::{ApiError, ApiOutput, OperationClass};
use std::sync::atomic::Ordering;
use support::*;

#[tokio::test]
async fn discovery_rejects_malformed_keys_at_the_portable_boundary() {
    let fixture = Fixture::new().await;
    for value in [
        serde_json::json!({}),
        serde_json::json!({"key":null}),
        serde_json::json!({"key":{"protocol":{"name":"rsi.test","version":1},"id":{"domain":"../escape","name":"echo","version":1}}}),
        serde_json::json!({"key":{"protocol":{"name":"INVALID","version":0},"id":{"domain":"test","name":"echo","version":1}}}),
    ] {
        let spec = rsi_api_protocol::operation_operation();
        let input = fixture
            .client
            .input_budget(spec.class)
            .encode(&value, spec.maximum_request_bytes)
            .unwrap();
        assert!(matches!(
            fixture.client.call(&spec, input).await,
            Err(ApiError::Invalid(_))
        ));
    }
    fixture.close().await;
}

#[tokio::test]
async fn a_fixed_portable_grant_rediscovers_current_routes_and_policy_without_reexport() {
    let mut fixture = Fixture::new().await;
    let echo = operation("echo");
    fixture.registrations.remove(0).close().await;
    assert!(
        !fixture
            .client
            .operations()
            .await
            .unwrap()
            .contains_all(&[echo.key()])
    );
    let input = || {
        fixture
            .client
            .input_budget(OperationClass::Data)
            .copy(b"{}")
            .unwrap()
    };
    assert!(matches!(
        fixture.client.call(&echo, input()).await,
        Err(ApiError::UnknownOperation)
    ));
    let mut policy = echo.clone();
    policy.class = OperationClass::Control;
    policy.encoding = rsi_api_protocol::RequestEncoding::Json;
    policy.maximum_request_bytes = 1024;
    policy.maximum_response_bytes = 128 * 1024;
    let replacement = fixture.register_echo(policy.clone());
    assert_eq!(fixture.client.operation(&echo.key()).await.unwrap(), policy);
    assert!(fixture.client.call(&echo, input()).await.is_ok());
    assert!(
        !fixture
            .client
            .operations()
            .await
            .unwrap()
            .contains_all(&[operation("hidden").key()])
    );
    replacement.close().await;
    policy.protocol.version += 1;
    let incompatible = fixture.register_echo(policy);
    assert!(matches!(
        fixture.client.call(&echo, input()).await,
        Err(ApiError::ProtocolMismatch)
    ));
    incompatible.close().await;
    fixture.close().await;
}

#[tokio::test]
async fn unfinished_descriptions_share_the_bounded_control_lane() {
    use rsi_api_protocol::portable::{ErrorCode, Header};
    let fixture = Fixture::new().await;
    let capability = fixture
        .runtime
        .root()
        .lookup_local::<rsi_api_portable::ApiExportContract>()
        .unwrap();
    let mut replies = Vec::new();
    for _ in 0..17 {
        let mut call = capability.open().unwrap();
        call.send(rsi_meta::Message::new(
            b"\0{\"kind\":\"describe\"}".to_vec(),
        ))
        .await
        .unwrap();
        // Keep each request unfinished: its provider must hold admission at EOF.
        replies.push(Box::pin(async move { call.recv().await.unwrap().unwrap() }));
    }
    let (reply, _, pending) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        futures_util::future::select_all(replies),
    )
    .await
    .expect("the seventeenth description must reject without waiting for EOF");
    assert!(matches!(
        serde_json::from_slice::<Header>(&reply.as_bytes()[1..]).unwrap(),
        Header::Error {
            code: ErrorCode::Capacity,
            ..
        }
    ));
    drop(reply);
    drop(pending);
    drop(capability);
    tokio::time::timeout(std::time::Duration::from_secs(3), fixture.close())
        .await
        .expect("unfinished descriptions release admission on cancellation");
}

#[tokio::test]
async fn real_meta_transport_fragments_json_binary_and_domain_errors_without_widening_exports() {
    let fixture = Fixture::new().await;
    assert_eq!(
        fixture
            .client
            .operations()
            .await
            .unwrap()
            .operations()
            .len(),
        7
    );
    assert!(
        !fixture
            .client
            .operations()
            .await
            .unwrap()
            .operations()
            .contains(&operation("hidden"))
    );
    let bytes: Vec<_> = (0..200_000)
        .map(|index| u8::try_from(index % 256).unwrap())
        .collect();
    let input = fixture
        .client
        .input_budget(OperationClass::Data)
        .copy(&bytes)
        .unwrap();
    let ApiOutput::Reply(reply) = fixture
        .client
        .call(&operation("echo"), input)
        .await
        .unwrap()
    else {
        panic!("reply")
    };
    assert_eq!(reply.binary.unwrap().as_bytes(), bytes);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(reply.json.as_bytes()).unwrap()["length"],
        200_000
    );
    let input = fixture
        .client
        .input_budget(OperationClass::Data)
        .copy(b"")
        .unwrap();
    let error = fixture
        .client
        .call(&operation("domain"), input)
        .await
        .unwrap_err();
    let ApiError::Domain(bytes) = error else {
        panic!("domain JSON")
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(bytes.as_bytes()).unwrap()["rejected"]
            .as_str()
            .unwrap()
            .len(),
        20_000
    );
    fixture.close().await;
}

#[tokio::test]
async fn dropped_mutation_waiter_keeps_actual_api_work_owned_through_export_retirement() {
    let fixture = Fixture::new().await;
    let client = fixture.client.clone();
    let task = tokio::spawn(async move {
        let input = client
            .input_budget(OperationClass::Data)
            .copy(b"once")
            .unwrap();
        client.call(&operation("mutate"), input).await
    });
    until(|| fixture.behavior.mutations.load(Ordering::SeqCst) == 1).await;
    task.abort();
    let exporting = fixture.exporter.clone();
    let closing = tokio::spawn(async move { exporting.dispose().await });
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(!closing.is_finished());
    assert_eq!(fixture.behavior.finished.load(Ordering::SeqCst), 0);
    fixture.behavior.release.add_permits(1);
    assert!(closing.await.unwrap().is_clean());
    assert_eq!(fixture.behavior.finished.load(Ordering::SeqCst), 1);
    fixture.close().await;
}

#[tokio::test]
async fn unpolled_stream_releases_provider_capture_when_importer_retires() {
    let fixture = Fixture::new().await;
    let input = fixture
        .client
        .input_budget(OperationClass::Subscription)
        .copy(b"")
        .unwrap();
    let ApiOutput::Stream(mut stream) = fixture
        .client
        .call(&operation("stream"), input)
        .await
        .unwrap()
    else {
        panic!("stream")
    };
    assert_eq!(stream.next().await.unwrap().unwrap().json.as_bytes(), b"0");
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fixture.importer.dispose()
        )
        .await
        .unwrap()
        .is_clean()
    );
    assert!(matches!(
        stream.next().await,
        Some(Err(ApiError::ShuttingDown))
    ));
    let input = fixture
        .client
        .input_budget(OperationClass::Data)
        .copy(b"")
        .unwrap();
    assert!(matches!(
        fixture.client.call(&operation("echo"), input).await,
        Err(ApiError::ShuttingDown)
    ));
    drop(stream);
    fixture.close().await;
}

#[tokio::test]
async fn malformed_requests_cannot_choose_origin_or_dispatch_a_partial_mutation() {
    use rsi_api_protocol::portable::{ErrorCode, Header};
    use rsi_meta::Message;
    let fixture = Fixture::new().await;
    let capability = fixture
        .runtime
        .root()
        .lookup_local::<rsi_api_portable::ApiExportContract>()
        .unwrap();
    let mutation = operation("mutate");
    let requests = [
        (
            serde_json::json!({"kind":"call","operation":mutation.call(),"bytes":0,"origin":"local"}),
            None,
            ErrorCode::Invalid,
        ),
        (
            serde_json::json!({"kind":"call","operation":mutation.call(),"bytes":mutation.maximum_request_bytes+1}),
            None,
            ErrorCode::Invalid,
        ),
        (
            serde_json::json!({"kind":"call","operation":mutation.call(),"bytes":2}),
            Some(vec![1, 0, 0, 0, 0, b'x']),
            ErrorCode::Invalid,
        ),
        (
            serde_json::json!({"kind":"call","operation":mutation.call(),"bytes":0}),
            Some(vec![1, 0, 0, 0, 0, b'x']),
            ErrorCode::Invalid,
        ),
        (
            serde_json::json!({"kind":"call","operation":operation("hidden").call(),"bytes":0}),
            None,
            ErrorCode::Unauthorized,
        ),
    ];
    for (header, extra, expected) in requests {
        let mut call = capability.open().unwrap();
        let mut bytes = vec![0];
        bytes.extend(serde_json::to_vec(&header).unwrap());
        call.send(Message::new(bytes)).await.unwrap();
        if let Some(extra) = extra {
            call.send(Message::new(extra)).await.unwrap();
        }
        call.finish();
        let reply = call.recv().await.unwrap().unwrap();
        let Header::Error { code, domain } =
            serde_json::from_slice(&reply.as_bytes()[1..]).unwrap()
        else {
            panic!("rejection")
        };
        assert_eq!(code, expected);
        assert!(domain.is_none());
        assert!(call.recv().await.unwrap().is_none());
    }
    assert_eq!(fixture.behavior.mutations.load(Ordering::SeqCst), 0);
    drop(capability);
    fixture.close().await;
}

#[tokio::test(start_paused = true)]
async fn portable_subscription_inherits_the_runtime_deadline_even_after_items() {
    let mut limits = rsi_meta::RuntimeLimits::default();
    limits.deadlines.service_call = std::time::Duration::from_millis(100);
    let fixture = Fixture::with_runtime(rsi_meta::Runtime::new(limits).unwrap()).await;
    let input = fixture
        .client
        .input_budget(OperationClass::Subscription)
        .copy(b"")
        .unwrap();
    let ApiOutput::Stream(mut stream) = fixture
        .client
        .call(&operation("stream"), input)
        .await
        .unwrap()
    else {
        panic!("stream");
    };
    for _ in 0..3 {
        assert!(stream.next().await.unwrap().is_ok());
    }
    tokio::time::advance(std::time::Duration::from_millis(101)).await;
    assert!(stream.next().await.unwrap().is_err());
    fixture.close().await;
}

#[tokio::test]
async fn retired_export_fences_a_captured_client_without_dispatching_again() {
    let fixture = Fixture::new().await;
    assert!(fixture.exporter.dispose().await.is_clean());
    let input = fixture
        .client
        .input_budget(OperationClass::Data)
        .copy(b"never")
        .unwrap();
    assert!(
        fixture
            .client
            .call(&operation("mutate"), input)
            .await
            .is_err()
    );
    assert_eq!(fixture.behavior.mutations.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[derive(Debug)]
struct DirectExport {
    api: std::sync::Arc<dyn rsi_api_protocol::ApiClient>,
    selected: Vec<rsi_api_protocol::OperationKey>,
}
#[async_trait::async_trait]
impl rsi_meta::PluginFactory for DirectExport {
    fn prepare(
        &self,
        config: &rsi_meta::ConfigValue,
    ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        rsi_api_portable::export_api(&plan, "rejected.api", self.api.clone(), &self.selected)?;
        Ok(())
    }
}

#[tokio::test]
async fn both_export_entry_points_reject_duplicate_and_oversized_selections() {
    use rsi_meta::PluginFactory as _;
    let fixture = Fixture::new().await;
    let duplicate = vec![operation("echo").key(); 2];
    let oversized = (0..rsi_api_protocol::MAXIMUM_OPERATIONS - 1)
        .map(|index| rsi_api_protocol::OperationKey {
            protocol: operation("echo").protocol,
            id: rsi_api_protocol::OperationId::new("fixture", format!("op{index}"), 1).unwrap(),
        })
        .collect();
    let mut malformed = operation("echo").key();
    malformed.protocol.name = "INVALID".into();
    for selected in [duplicate, oversized, vec![malformed]] {
        let config = serde_json::json!({"service":"rejected.api", "operations":selected});
        assert!(matches!(
            rsi_api_portable::PortableApiExportFactory.prepare(&config),
            Err(rsi_meta::MetaError::InvalidInput(_))
        ));
        let result = fixture
            .runtime
            .root()
            .apply(
                rsi_meta::ResolvedFactory::linked(
                    "direct",
                    "1",
                    rsi_meta::UpdateMode::Replayable,
                    std::sync::Arc::new(DirectExport {
                        api: fixture.client.clone(),
                        selected,
                    }),
                ),
                rsi_meta::ConfigValue::Null,
            )
            .await;
        assert!(matches!(
            result.unwrap().snapshot().state,
            rsi_meta::FiberState::Failed(_)
        ));
        assert!(fixture.runtime.root().service("rejected.api").is_err());
    }
    fixture.close().await;
}

#[derive(Debug)]
struct UnsupportedVersion {
    description: rsi_api_protocol::ConnectionDescription,
}
#[async_trait::async_trait]
impl rsi_api_protocol::ApiClient for UnsupportedVersion {
    fn description(&self) -> &rsi_api_protocol::ConnectionDescription {
        &self.description
    }
    async fn operations(&self) -> rsi_api_protocol::Result<rsi_api_protocol::OperationCatalog> {
        rsi_api_protocol::OperationCatalog::new(vec![])
    }
    fn input_budget(&self, _: OperationClass) -> rsi_api_protocol::ByteBudget {
        rsi_api_protocol::ByteBudget::default()
    }
    async fn call(
        &self,
        _: &rsi_api_protocol::OperationSpec,
        _: rsi_api_protocol::RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        panic!("rejected grant cannot dispatch");
    }
}

#[tokio::test]
async fn exporter_rejects_unsupported_client_wire_version_before_publishing_a_grant() {
    let runtime = rsi_meta::Runtime::default();
    let api = std::sync::Arc::new(UnsupportedVersion {
        description: rsi_api_protocol::ConnectionDescription {
            wire_version: 99,
            endpoint_id: rsi_api_protocol::EndpointId::from_bytes([1; 16]),
            host_epoch: rsi_api_protocol::HostEpoch::from_bytes([2; 16]),
        },
    });
    let handle = runtime
        .root()
        .apply(
            rsi_meta::ResolvedFactory::linked(
                "direct",
                "1",
                rsi_meta::UpdateMode::Replayable,
                std::sync::Arc::new(DirectExport {
                    api,
                    selected: vec![],
                }),
            ),
            rsi_meta::ConfigValue::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        handle.snapshot().state,
        rsi_meta::FiberState::Failed(_)
    ));
    assert!(runtime.root().service("rejected.api").is_err());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn export_bounds_policy_lookups_before_upstream_work() {
    let fixture = Fixture::new().await;
    fixture.behavior.pause_policy.store(true, Ordering::SeqCst);
    let before = fixture.behavior.policy_calls.load(Ordering::SeqCst);
    let mut pending = Vec::new();
    for _ in 0..16 {
        let client = fixture.client.clone();
        pending.push(tokio::spawn(async move {
            let spec = operation("stream");
            let input = client.input_budget(spec.class).copy(b"{}").unwrap();
            client.call(&spec, input).await
        }));
    }
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while fixture.behavior.policy_calls.load(Ordering::SeqCst) < before + 16 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let spec = operation("echo");
    let input = fixture.client.input_budget(spec.class).copy(b"{}").unwrap();
    assert!(matches!(
        fixture.client.call(&spec, input).await,
        Err(ApiError::Capacity)
    ));
    assert_eq!(
        fixture.behavior.policy_calls.load(Ordering::SeqCst),
        before + 16
    );
    fixture.behavior.pause_policy.store(false, Ordering::SeqCst);
    fixture.behavior.policy_release.add_permits(16);
    for task in pending {
        drop(task.await.unwrap().unwrap());
    }
    fixture.close().await;
}
