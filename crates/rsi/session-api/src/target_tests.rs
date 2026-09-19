use super::*;
use rsi_api_protocol::{
    ApiContext, ApiHandler, ApiMessage, ApiRegistrar, ApiResponseCapacity, AuthenticatedDevice,
    DeviceId, EndpointId, HostEpoch,
};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct Handler(Arc<AtomicUsize>);
#[async_trait]
impl ApiHandler for Handler {
    async fn invoke(
        &self,
        context: ApiContext,
        _: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        assert!(
            matches!(context.origin,CallOrigin::Device(device) if device.id == DeviceId::from_bytes([7;16]))
        );
        self.0.fetch_add(1, Ordering::SeqCst);
        let ApiResponseCapacity::Finite(capacity) = output else {
            unreachable!()
        };
        Ok(ApiOutput::Reply(ApiMessage {
            json: capacity.encode(&true)?,
            binary: None,
        }))
    }
}
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One public dispatch scenario preserves actual handler call counts"
)]
async fn actual_dispatch_rejects_cross_session_and_origin_forgery_before_invocation() {
    let runtime = rsi_meta::Runtime::default();
    let registry = Arc::new(rsi_api::ApiRegistry::new(runtime.execution().clone()));
    let calls = Arc::new(AtomicUsize::new(0));
    let registrations: Vec<_> = Operation::ALL
        .into_iter()
        .map(|operation| {
            registry
                .register(operation.spec(), Arc::new(Handler(calls.clone())))
                .unwrap()
        })
        .collect();
    let device = AuthenticatedDevice {
        id: DeviceId::from_bytes([7; 16]),
        revoked: tokio_util::sync::CancellationToken::new(),
    };
    let local = SessionTargetClient::from_dispatch(
        registry.clone(),
        ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        CallOrigin::Device(device.clone()),
        SessionId::new("allowed").unwrap(),
    )
    .unwrap();
    assert_eq!(local.operations().len(), 35); // Includes composition resources and three reference reads.
    assert!(local.operations().contains(&Operation::Resource.spec()));
    for operation in [
        Operation::CaptureReference,
        Operation::PreviewReference,
        Operation::ReadReference,
    ] {
        assert!(local.operations().contains(&operation.spec()));
    }
    let local = Arc::new(local);
    let client =
        SessionTargetClient::new(local.clone(), SessionId::new("allowed").unwrap()).unwrap();
    let target = |id: &str| serde_json::json!({"session_id":id,"header_key":"a".repeat(64)});
    let requests = [
        (
            Operation::CaptureReference,
            serde_json::json!({"target":target("allowed"),"input":{"session_id":"other"}}),
        ),
        (
            Operation::Resource,
            serde_json::json!({"target":target("other"),"input":{"kind":"sources"}}),
        ),
        (Operation::Attach, serde_json::json!({"session_id":"other"})),
        (
            Operation::Submit,
            serde_json::json!({"target":target("other"),"input":{}}),
        ),
        (
            Operation::Create,
            serde_json::json!({"session_id":"allowed"}),
        ),
        (
            Operation::Recent,
            serde_json::json!({"after":null,"limit":1}),
        ),
        (
            Operation::Submit,
            serde_json::json!({"target":target("allowed"),"input":{},"origin":"local"}),
        ),
        (
            Operation::Submit,
            serde_json::json!({"target":{"session_id":"allowed","header_key":"a".repeat(64),"origin":"local"},"input":{}}),
        ),
    ];
    for api in [&client as &dyn ApiClient, local.as_ref()] {
        for (operation, request) in &requests {
            let operation = operation.spec();
            let input = api
                .input_budget(operation.class)
                .encode(&request, operation.maximum_request_bytes)
                .unwrap();
            assert!(api.call(&operation, input).await.is_err());
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    for (operation, request) in [
        (
            Operation::Attach,
            serde_json::json!({"session_id":"allowed"}),
        ),
        (
            Operation::Submit,
            serde_json::json!({"target":target("allowed"),"input":{}}),
        ),
        (
            Operation::CaptureReference,
            serde_json::json!({"target":target("allowed"),"input":{"session_id":"allowed"}}),
        ),
    ] {
        let operation = operation.spec();
        let input = client
            .input_budget(operation.class)
            .encode(&request, operation.maximum_request_bytes)
            .unwrap();
        assert!(client.call(&operation, input).await.is_ok());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    device.revoked.cancel();
    let operation = Operation::Attach.spec();
    let input = client
        .input_budget(operation.class)
        .encode(
            &serde_json::json!({"session_id":"allowed"}),
            operation.maximum_request_bytes,
        )
        .unwrap();
    assert!(matches!(
        client.call(&operation, input).await,
        Err(ApiError::Unauthorized)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    for registration in registrations {
        registration.close().await;
    }
    registry.close().await;
    assert!(runtime.shutdown().await.is_clean());
}
