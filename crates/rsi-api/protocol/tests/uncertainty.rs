use rsi_api_protocol::ApiError;

#[test]
fn only_a_possibly_dispatched_mutation_loses_definite_failure_classification() {
    for error in [
        ApiError::Backend("lost result".into()),
        ApiError::Invalid("malformed response".into()),
        ApiError::Capacity,
        ApiError::ShuttingDown,
        ApiError::Unauthorized,
        ApiError::OutcomeUnknown,
    ] {
        assert_eq!(error.clone().after_dispatch(false), error);
        assert_eq!(error.after_dispatch(true), ApiError::OutcomeUnknown);
    }
}
