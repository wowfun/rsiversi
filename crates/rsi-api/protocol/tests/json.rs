use async_trait::async_trait;
use rsi_api_protocol::*;
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    accepted: bool,
}
#[derive(Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
enum Failure {
    Rejected,
}

#[tokio::test]
async fn typed_handler_rejects_unknown_input_before_entry_and_bounds_encoded_domain_errors() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let handler = json_handler(move |_: ApiContext, request: Request| {
        count.fetch_add(1, Ordering::SeqCst);
        async move {
            Ok(if request.accepted {
                Ok(true)
            } else {
                Err(Failure::Rejected)
            })
        }
    });
    let budget = ByteBudget::new(128).unwrap();
    let context = || ApiContext {
        origin: CallOrigin::Local,
        retiring: CancellationToken::new(),
    };
    let bad = budget.copy(br#"{"accepted":true,"extra":1}"#).unwrap();
    let result = handler
        .invoke(
            context(),
            bad,
            ApiResponseCapacity::Finite(budget.reserve(32).unwrap().into()),
        )
        .await;
    assert!(matches!(result, Err(ApiError::Invalid(_))));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let request = budget.copy(br#"{"accepted":false}"#).unwrap();
    let result = handler
        .invoke(
            context(),
            request,
            ApiResponseCapacity::Finite(budget.reserve(32).unwrap().into()),
        )
        .await;
    let Err(ApiError::Domain(bytes)) = result else {
        panic!("domain error")
    };
    assert_eq!(
        serde_json::from_slice::<Failure>(bytes.as_bytes()).unwrap(),
        Failure::Rejected
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(bytes);
    assert_eq!(budget.used(), 0);
    let request = budget.copy(br#"{"accepted":false}"#).unwrap();
    let result = handler
        .invoke(
            context(),
            request,
            ApiResponseCapacity::Finite(budget.reserve(1).unwrap().into()),
        )
        .await;
    assert!(
        matches!(result, Err(ApiError::Backend(_))),
        "post-entry encoding must remain infrastructure failure"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(budget.used(), 0);
}

#[derive(Debug)]
struct Client {
    description: ConnectionDescription,
    spec: OperationSpec,
    domain: bool,
    calls: AtomicUsize,
}
#[async_trait]
impl ApiClient for Client {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        std::slice::from_ref(&self.spec)
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(&self, _: &OperationSpec, _: RetainedBytes) -> Result<ApiOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let json = ByteBudget::default().copy(b"{}").unwrap();
        if self.domain {
            Err(ApiError::Domain(json))
        } else {
            Ok(ApiOutput::Reply(ApiMessage { json, binary: None }))
        }
    }
}

#[tokio::test]
async fn typed_client_marks_malformed_mutation_replies_unknown_but_local_encoding_stays_known() {
    for effect in [OperationEffect::Read, OperationEffect::Mutation] {
        for domain in [false, true] {
            let mut spec = describe_operation();
            spec.effect = effect;
            let client = Client {
                description: ConnectionDescription {
                    wire_version: 1,
                    endpoint_id: EndpointId::from_bytes([1; 16]),
                    host_epoch: HostEpoch::from_bytes([2; 16]),
                },
                spec: spec.clone(),
                domain,
                calls: AtomicUsize::new(0),
            };
            let result =
                call_json::<_, bool, Failure>(&client, &spec, &Request { accepted: true }).await;
            if effect == OperationEffect::Mutation {
                assert_eq!(result.unwrap_err(), ApiError::OutcomeUnknown);
            } else {
                assert!(matches!(result, Err(ApiError::Invalid(_))));
            }
            spec.maximum_request_bytes = 1;
            let result =
                call_json::<_, bool, Failure>(&client, &spec, &Request { accepted: true }).await;
            assert!(matches!(result, Err(ApiError::Invalid(_))));
            assert_eq!(client.calls.load(Ordering::SeqCst), 1);
        }
    }
}
