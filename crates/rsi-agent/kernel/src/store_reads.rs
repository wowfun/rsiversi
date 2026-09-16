//! Payload admission follows preparation; dispatched work owns both guards.

use super::{
    Arc, KernelInner, MAXIMUM_SESSION_FACT_BYTES, MAXIMUM_STORE_BATCH_BYTES, SessionId,
    SessionStore, StoreError, observation,
};
use rsi_agent_store_protocol::SessionValidationLease;
use tokio::sync::OwnedSemaphorePermit;

pub(super) type Materialized<T> = (T, OwnedSemaphorePermit, Option<SessionValidationLease>);

pub(super) fn page_limit(inner: &KernelInner, requested: usize) -> (usize, usize) {
    if requested == 1 || inner.limits.maximum_store_read_bytes < MAXIMUM_STORE_BATCH_BYTES {
        (1, MAXIMUM_SESSION_FACT_BYTES)
    } else {
        (requested, MAXIMUM_STORE_BATCH_BYTES)
    }
}

pub(super) async fn read<T, F, Fut>(
    inner: &KernelInner,
    id: &SessionId,
    bytes: usize,
    prepare: bool,
    operation: F,
) -> std::result::Result<Materialized<T>, StoreError>
where
    T: Send + 'static,
    F: FnOnce(Arc<dyn SessionStore>, SessionId) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = std::result::Result<T, StoreError>> + Send + 'static,
{
    let store = Arc::clone(&inner.store);
    let lease = if prepare {
        Some(store.prepare_session(id).await?)
    } else {
        None
    };
    let permit = observation::acquire_store_read_bytes(inner, bytes).await?;
    let id = id.clone();
    // Dropping a JoinHandle detaches. The task and even its queued result retain
    // admission until materialization is finished and the caller takes ownership.
    tokio::spawn(async move {
        let value = operation(store, id).await?;
        Ok((value, permit, lease))
    })
    .await
    .map_err(|error| StoreError::Io(format!("Store read task failed: {error}")))?
}
