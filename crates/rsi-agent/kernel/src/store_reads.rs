//! Payload admission follows preparation; dispatched work owns both guards.

use super::{
    Arc, KernelInner, MAXIMUM_SESSION_FACT_BYTES, SessionId, SessionStore, StoreError, observation,
};
use rsi_agent_store_protocol::SessionValidationLease;
use tokio::sync::OwnedSemaphorePermit;

#[cfg(test)]
mod tests;

pub(super) type Materialized<T> = (T, OwnedSemaphorePermit, Option<Arc<SessionValidationLease>>);
pub(super) const INITIAL_PAGE_BYTES: usize = 1024 * 1024;

/// Retry only a length-only first-record refusal, after the first worker has
/// dropped its permit. Tokio semaphore admission is FIFO, including this fallback.
pub(super) async fn page<T, F, Fut>(
    inner: &KernelInner,
    id: &SessionId,
    limit: usize,
    operation: F,
) -> std::result::Result<Materialized<(T, usize)>, StoreError>
where
    T: Send + 'static,
    F: Fn(Arc<dyn SessionStore>, SessionId, usize, usize) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = std::result::Result<T, StoreError>> + Send + 'static,
{
    rsi_agent_store_protocol::validate_read_limit(limit)?;
    let lease = Arc::new(inner.store.prepare_session(id).await?);
    let mut bytes = INITIAL_PAGE_BYTES;
    for attempt in 0..2 {
        let operation = operation.clone();
        let count = if attempt == 0 { limit } else { 1 };
        let result = read_prepared(inner, id, bytes, Some(lease.clone()), move |store, id| {
            operation(store, id, count, bytes)
        })
        .await;
        match result {
            Err(StoreError::ReadRequiresCapacity { minimum_bytes }) => {
                if attempt != 0
                    || minimum_bytes <= bytes
                    || minimum_bytes > MAXIMUM_SESSION_FACT_BYTES
                {
                    return Err(StoreError::Corrupt(
                        "invalid first-record capacity response".into(),
                    ));
                }
                bytes = minimum_bytes;
            }
            value => return value.map(|(page, permit, lease)| ((page, count), permit, lease)),
        }
    }
    unreachable!("the last bounded read attempt returns")
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
        Some(Arc::new(store.prepare_session(id).await?))
    } else {
        None
    };
    read_prepared(inner, id, bytes, lease, operation).await
}

async fn read_prepared<T, F, Fut>(
    inner: &KernelInner,
    id: &SessionId,
    bytes: usize,
    lease: Option<Arc<SessionValidationLease>>,
    operation: F,
) -> std::result::Result<Materialized<T>, StoreError>
where
    T: Send + 'static,
    F: FnOnce(Arc<dyn SessionStore>, SessionId) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = std::result::Result<T, StoreError>> + Send + 'static,
{
    let store = Arc::clone(&inner.store);
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
