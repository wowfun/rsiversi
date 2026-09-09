use async_trait::async_trait;
use rsi_api::ApiRegistry;
use rsi_api_protocol::{ApiDispatch, ApiError, ApiOutput, ByteBudget, CallOrigin};
use rsi_native_addons_api::{
    NativeAddonAdministration, NativeAddonsApi, RefreshFailure, RefreshReceipt,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{Notify, Semaphore};

#[derive(Debug)]
struct Owner {
    calls: AtomicUsize,
    entered: Notify,
    release: Semaphore,
}
#[async_trait]
impl NativeAddonAdministration for Owner {
    async fn refresh(&self) -> Result<RefreshReceipt, RefreshFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.acquire().await.unwrap().forget();
        Ok(RefreshReceipt {
            source_revision: u64::MAX,
            changed: true,
            selected: 1,
        })
    }
}
#[tokio::test]
async fn refresh_validates_before_owner_and_drain_keeps_abandoned_mutation_owned() {
    let registry = Arc::new(ApiRegistry::new(rsi_meta::Execution::native(
        tokio::runtime::Handle::current(),
    )));
    let owner = Arc::new(Owner {
        calls: AtomicUsize::new(0),
        entered: Notify::new(),
        release: Semaphore::new(0),
    });
    let api = NativeAddonsApi::register(registry.as_ref(), owner.clone()).unwrap();
    let operations = registry.operations();
    assert_eq!(operations.len(), 1);
    let operation = &operations[0];
    assert_eq!(
        operation.effect,
        rsi_api_protocol::OperationEffect::Mutation
    );
    assert!(matches!(
        registry.admit(
            &operation.id,
            CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
                id: rsi_api_protocol::DeviceId::from_bytes([9; 16]),
                revoked: tokio_util::sync::CancellationToken::new()
            })
        ),
        Err(ApiError::Unauthorized)
    ));
    let encode =
        |value: serde_json::Value| ByteBudget::new(1024).unwrap().encode(&value, 1024).unwrap();
    assert!(
        registry
            .admit(&operation.id, CallOrigin::Local)
            .unwrap()
            .invoke(encode(serde_json::json!({"source":"unreviewed"})))
            .await
            .is_err()
    );
    assert_eq!(owner.calls.load(Ordering::SeqCst), 0);
    let call = registry.admit(&operation.id, CallOrigin::Local).unwrap();
    let bytes = encode(serde_json::json!({}));
    let waiter = tokio::spawn(async move { call.invoke(bytes).await });
    owner.entered.notified().await;
    waiter.abort();
    let _ = waiter.await;
    let close = tokio::spawn(api.close());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !registry.operations().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!close.is_finished());
    owner.release.add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(2), close)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner.calls.load(Ordering::SeqCst), 1);

    let api = NativeAddonsApi::register(registry.as_ref(), owner.clone()).unwrap();
    owner.release.add_permits(1);
    let ApiOutput::Reply(reply) = registry
        .admit(&operation.id, CallOrigin::Local)
        .unwrap()
        .invoke(encode(serde_json::json!({})))
        .await
        .unwrap()
    else {
        panic!("finite response")
    };
    let value: serde_json::Value = serde_json::from_slice(reply.json.as_bytes()).unwrap();
    assert_eq!(value["source_revision"], u64::MAX.to_string());
    let receipt: RefreshReceipt = serde_json::from_slice(reply.json.as_bytes()).unwrap();
    assert_eq!(receipt.source_revision, u64::MAX);
    drop(reply);
    api.close().await;
}

#[test]
fn wire_revision_is_exact_and_closed() {
    for value in [
        serde_json::json!(1),
        serde_json::json!("01"),
        serde_json::json!("+1"),
        serde_json::json!("18446744073709551616"),
    ] {
        assert!(
            serde_json::from_value::<RefreshReceipt>(
                serde_json::json!({"source_revision":value,"changed":false,"selected":0})
            )
            .is_err()
        );
    }
    assert!(
        serde_json::from_value::<RefreshReceipt>(
            serde_json::json!({"source_revision":"1","changed":false,"selected":0,"extra":0})
        )
        .is_err()
    );
}
