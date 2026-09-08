use futures_util::FutureExt;
use rsi_api_protocol::{ApiError, ByteBudget};
use rsi_client::read_with_capacity_retry;
use rsi_meta::Execution;
use rsi_session_protocol::SessionError;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

pub async fn bounded_read_capacity_recovery_and_cancellation(execution: Execution) {
    let budget = ByteBudget::new(64).unwrap();
    let held = budget.reserve(64).unwrap();
    let release_execution = execution.clone();
    let release = execution.spawn(async move {
        release_execution.sleep(Duration::from_millis(110)).await;
        drop(held);
    });
    let attempts = AtomicUsize::new(0);
    let value = read_with_capacity_retry(&execution, || async {
        attempts.fetch_add(1, Ordering::SeqCst);
        let _reply = budget.reserve(64).map_err(SessionError::Api)?;
        Ok(17)
    })
    .await
    .unwrap();
    assert_eq!(value, 17);
    assert!(attempts.load(Ordering::SeqCst) > 1);
    release.await.unwrap();

    let held = budget.reserve(64).unwrap();
    let attempts = AtomicUsize::new(0);
    let result = read_with_capacity_retry(&execution, || async {
        attempts.fetch_add(1, Ordering::SeqCst);
        budget.reserve(64).map_err(SessionError::Api)
    })
    .await;
    assert!(matches!(result, Err(SessionError::Api(ApiError::Capacity))));
    assert_eq!(attempts.load(Ordering::SeqCst), 5);
    drop(held);

    for error in [
        SessionError::Api(ApiError::Unauthorized),
        SessionError::Api(ApiError::OutcomeUnknown),
        SessionError::Backend("read failed".into()),
    ] {
        let attempts = AtomicUsize::new(0);
        let result = read_with_capacity_retry::<(), _>(&execution, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err(error.clone()))
        })
        .await;
        assert_eq!(result.unwrap_err().to_string(), error.to_string());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    let attempts = AtomicUsize::new(0);
    let mut read = Box::pin(read_with_capacity_retry::<(), _>(&execution, || {
        attempts.fetch_add(1, Ordering::SeqCst);
        std::future::ready(Err(SessionError::Capacity))
    }));
    assert!(read.as_mut().now_or_never().is_none());
    drop(read);
    execution.sleep(Duration::from_millis(60)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}
