use rsi_api_protocol::{CallShape, OperationCall, OperationId, OperationKey, connection_protocol};

#[test]
fn all_call_shapes_have_one_canonical_transport_path() {
    for shape in [CallShape::Read, CallShape::Mutation, CallShape::Stream] {
        let call = OperationCall {
            key: OperationKey {
                protocol: connection_protocol(),
                id: OperationId::new("test", "echo", 1).unwrap(),
            },
            shape,
        };
        assert_eq!(OperationCall::from_path(&call.to_path()).unwrap(), call);
        for bad in [
            call.to_path() + "/",
            call.to_path().replace("/1/", "/01/"),
            call.to_path().replace("/test/", "/../"),
            call.to_path().replace("/echo/", "/%65cho/"),
            call.to_path().replace("/v2/", "/v1/"),
            call.to_path().replace("/rsi-api-protocol/", "/./"),
        ] {
            assert!(OperationCall::from_path(&bad).is_err(), "{bad}");
        }
    }
}
