use rsi_api::{ApiRegistry, ConnectionApi};
use rsi_api_protocol::{
    ApiDispatch, ApiError, ApiOutput, AuthenticatedDevice, CallOrigin, DeviceId, EndpointId,
    HostEpoch, OperationId,
};
use rsi_meta::Execution;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn caller_identity_is_derived_from_authenticated_origin_and_rejects_body_claims() {
    let registry = Arc::new(ApiRegistry::new(Execution::native(
        tokio::runtime::Handle::current(),
    )));
    let owner = ConnectionApi::register(
        registry.clone(),
        registry.as_ref(),
        EndpointId::from_bytes([1; 16]),
        HostEpoch::from_bytes([2; 16]),
    )
    .unwrap();
    let operation = OperationId::new("connection", "caller", 1).unwrap();
    let device = DeviceId::from_bytes([3; 16]);
    for (origin, expected) in [
        (CallOrigin::Local, serde_json::json!({"kind":"local"})),
        (
            CallOrigin::Device(AuthenticatedDevice {
                id: device.clone(),
                revoked: CancellationToken::new(),
            }),
            serde_json::json!({"kind":"device", "device_id":device}),
        ),
    ] {
        let admitted = registry.admit(&operation, origin.clone()).unwrap();
        let input = admitted.input_budget().copy(b"{}").unwrap();
        let ApiOutput::Reply(reply) = admitted.invoke(input).await.unwrap() else {
            panic!("caller is finite");
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(reply.json.as_bytes()).unwrap(),
            expected
        );
        let admitted = registry.admit(&operation, origin).unwrap();
        let input = admitted
            .input_budget()
            .copy(br#"{"kind":"local"}"#)
            .unwrap();
        assert!(matches!(
            admitted.invoke(input).await,
            Err(ApiError::Invalid(_))
        ));
    }
    owner.close().await;
    assert!(registry.operations().is_empty());
    registry.close().await;
}
