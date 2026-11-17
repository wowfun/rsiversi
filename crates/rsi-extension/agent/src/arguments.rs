//! Request-owned Tool arguments and retained CAS segments; no durable transfer state.
use async_trait::async_trait;
use rsi_extension::{
    Error, Handler, Result,
    body::{self, Body},
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

static NEXT_BODY_TOKEN: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct Retained {
    session: String,
    operation: String,
    bytes: Bytes,
    cancellation: CancellationToken,
    _permit: OwnedSemaphorePermit,
}
#[derive(Debug)]
enum Bytes {
    Arguments(Vec<u8>),
    Journal(rsi_api_protocol::RetainedBytes),
}
impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Arguments(bytes) => bytes,
            Self::Journal(bytes) => bytes,
        }
    }
}
#[derive(Debug)]
pub(crate) struct Arguments {
    active: Mutex<BTreeMap<String, Retained>>,
    slots: Arc<Semaphore>,
}
impl Default for Arguments {
    fn default() -> Self {
        Self {
            active: Mutex::default(),
            slots: Arc::new(Semaphore::new(2)),
        }
    }
}
pub(crate) struct Scope {
    owner: Arc<Arguments>,
    token: String,
}
impl Drop for Scope {
    fn drop(&mut self) {
        self.owner
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.token);
    }
}
impl Arguments {
    pub(crate) fn enter(
        self: &Arc<Self>,
        session: &str,
        operation: &str,
        value: &Value,
        cancellation: &CancellationToken,
    ) -> Result<(Body, Option<Scope>, usize)> {
        if let Ok(bytes) = rsi_api_protocol::measure_json(value, body::INLINE_BYTES) {
            return Ok((
                Body::Inline {
                    value: value.clone(),
                },
                None,
                bytes,
            ));
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Capacity)?;
        let mut output = crate::encoding::BoundedBytes {
            bytes: Vec::new(),
            maximum: rsi_tools_protocol::MAXIMUM_TOOL_JSON_BYTES,
        };
        serde_json::to_writer(&mut output, value).map_err(|_| Error::Protocol)?;
        let bytes = output.bytes.len();
        let (body, scope) = self.retain(
            session,
            operation,
            Bytes::Arguments(output.bytes),
            cancellation,
            permit,
        )?;
        Ok((body, scope, bytes))
    }
    pub(crate) fn enter_bytes(
        self: &Arc<Self>,
        session: &str,
        operation: &str,
        bytes: rsi_api_protocol::RetainedBytes,
        cancellation: &CancellationToken,
    ) -> Result<(Body, Option<Scope>)> {
        if bytes.len() <= body::INLINE_BYTES {
            return Ok((
                Body::Inline {
                    value: serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?,
                },
                None,
            ));
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Capacity)?;
        self.retain(
            session,
            operation,
            Bytes::Journal(bytes),
            cancellation,
            permit,
        )
    }
    fn retain(
        self: &Arc<Self>,
        session: &str,
        operation: &str,
        bytes: Bytes,
        cancellation: &CancellationToken,
        permit: OwnedSemaphorePermit,
    ) -> Result<(Body, Option<Scope>)> {
        let serial = NEXT_BODY_TOKEN
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map_err(|_| Error::Capacity)?;
        let token = format!("{serial:032x}");
        let body = Body::Paged {
            token: token.clone(),
            bytes: bytes.as_ref().len(),
        };
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                token.clone(),
                Retained {
                    session: session.into(),
                    operation: operation.into(),
                    bytes,
                    cancellation: cancellation.clone(),
                    _permit: permit,
                },
            );
        Ok((
            body,
            Some(Scope {
                owner: self.clone(),
                token,
            }),
        ))
    }
}
#[async_trait]
impl Handler for Arguments {
    async fn call(
        &self,
        _: &str,
        payload: Value,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            session_id: String,
            operation_id: String,
            token: String,
            offset: usize,
        }
        let request: Request = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let body = active.get(&request.token).ok_or(Error::Protocol)?;
        if cancellation.is_cancelled() || body.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if body.session != request.session_id || body.operation != request.operation_id {
            return Err(Error::Protocol);
        }
        body::fragment(body.bytes.as_ref(), request.offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn replacement_registration_rejects_old_tokens_for_the_same_operation() {
        let stop = CancellationToken::new();
        let value = json!("x".repeat(body::INLINE_BYTES));
        let original = Arc::new(Arguments::default());
        let (old, scope, _) = original.enter("s", "same", &value, &stop).unwrap();
        drop((scope, original));
        let replacement = Arc::new(Arguments::default());
        let (new, _scope, _) = replacement.enter("s", "same", &value, &stop).unwrap();
        let (Body::Paged { token: old, .. }, Body::Paged { token: new, .. }) = (old, new) else {
            panic!("paged bodies")
        };
        let request =
            |token| json!({"session_id":"s","operation_id":"same","token":token,"offset":0});
        assert_eq!(
            replacement.call("read", request(old), stop.clone()).await,
            Err(Error::Protocol)
        );
        assert!(replacement.call("read", request(new), stop).await.is_ok());
    }

    #[test]
    fn encoded_byte_bound_selects_inline_or_retained_and_releases_failed_admission() {
        let owner = Arc::new(Arguments::default());
        let stop = CancellationToken::new();
        let inline = json!("x".repeat(body::INLINE_BYTES - 2));
        let (body, scope, bytes) = owner.enter("s", "inline", &inline, &stop).unwrap();
        assert_eq!(bytes, body::INLINE_BYTES);
        assert!(matches!(body, Body::Inline { value } if value == inline));
        assert!(scope.is_none());
        assert_eq!(owner.slots.available_permits(), 2);
        let escaped = json!("\\".repeat(body::INLINE_BYTES / 2));
        let (body, scope, bytes) = owner.enter("s", "paged", &escaped, &stop).unwrap();
        assert_eq!(bytes, body::INLINE_BYTES + 2);
        assert!(matches!(body, Body::Paged { bytes, .. } if bytes == body::INLINE_BYTES + 2));
        assert_eq!(owner.slots.available_permits(), 1);
        drop(scope);
        let maximum = json!("\\".repeat(rsi_tools_protocol::MAXIMUM_TOOL_JSON_BYTES / 2 - 1));
        let (body, scope, _) = owner.enter("s", "maximum", &maximum, &stop).unwrap();
        assert!(
            matches!(body, Body::Paged { bytes, .. } if bytes == rsi_tools_protocol::MAXIMUM_TOOL_JSON_BYTES)
        );
        drop(scope);
        let oversized = json!("\\".repeat(rsi_tools_protocol::MAXIMUM_TOOL_JSON_BYTES / 2));
        assert!(matches!(
            owner.enter("s", "oversized", &oversized, &stop),
            Err(Error::Protocol)
        ));
        assert_eq!(owner.slots.available_permits(), 2);
        assert!(owner.active.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn bodies_are_operation_scoped_bounded_and_retired_by_request_drop() {
        let owner = Arc::new(Arguments::default());
        let stop = CancellationToken::new();
        let operation_stop = stop.child_token();
        let large = || json!("界".repeat(100_000));
        let (first, scope, _) = owner.enter("s", "one", &large(), &operation_stop).unwrap();
        let (_, second, _) = owner.enter("s", "two", &large(), &stop).unwrap();
        assert!(
            owner
                .enter("s", "inline", &json!({"small": true}), &stop)
                .is_ok()
        );
        assert!(matches!(
            owner.enter("s", "three", &large(), &stop),
            Err(Error::Capacity)
        ));
        let Body::Paged { token, .. } = first else {
            panic!("large arguments must page")
        };
        let request =
            |operation| json!({"session_id":"s","operation_id":operation,"token":token,"offset":0});
        assert_eq!(
            owner.call("read", request("other"), stop.clone()).await,
            Err(Error::Protocol)
        );
        assert!(
            owner
                .call("read", request("one"), stop.clone())
                .await
                .is_ok()
        );
        assert_eq!(
            Arguments::default()
                .call("read", request("one"), stop.clone())
                .await,
            Err(Error::Protocol)
        );
        operation_stop.cancel();
        assert_eq!(
            owner.call("read", request("one"), stop.clone()).await,
            Err(Error::Cancelled)
        );
        drop(scope);
        assert_eq!(
            owner.call("read", request("one"), stop.clone()).await,
            Err(Error::Protocol)
        );
        let (_, third, _) = owner.enter("s", "three", &large(), &stop).unwrap();
        drop((second, third));
        assert_eq!(owner.slots.available_permits(), 2);
    }
}
