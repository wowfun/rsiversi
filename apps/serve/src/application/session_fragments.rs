use super::session_history::{fragment, resource_key};
use rsi_agent_session_protocol::{MessageId, SessionId};
use rsi_api_http::ApplicationUpstream;
use rsi_extension::{Error, Result};
use serde_json::Value;
use std::{future::Future, sync::Arc};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Identity {
    Facts {
        session: SessionId,
        parent: Option<SessionId>,
        after: u64,
        through: u64,
    },
    Controls {
        session: SessionId,
        parent: Option<SessionId>,
        after: u64,
        through: u64,
    },
    Message {
        session: SessionId,
        parent: Option<SessionId>,
        message: MessageId,
        accepted: u64,
    },
    Inherited {
        target: SessionId,
        source: SessionId,
        after: u64,
        through: u64,
    },
}

#[derive(Debug)]
struct Page {
    identity: Identity,
    through: u64,
    json: Box<str>,
    _permit: OwnedSemaphorePermit,
}

#[derive(Clone, Debug)]
pub(super) struct Pages(Arc<Semaphore>);

impl Default for Pages {
    fn default() -> Self {
        Self(Arc::new(Semaphore::new(2)))
    }
}

impl Pages {
    pub(super) async fn read(
        &self,
        routes: &ApplicationUpstream,
        request: &str,
        reader: &str,
        offset: usize,
        identity: Identity,
        load: impl Future<Output = Result<(u64, String)>>,
    ) -> Result<Value> {
        let key = resource_key("history", reader)?;
        if offset == 0 {
            // The application queues before issuing RPCs. Waiting here would
            // consume slots needed to finish or close already retained pages.
            let permit = self
                .0
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::Capacity)?;
            let (through, json) = load.await?;
            if json.len() > rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES + 2 {
                return Err(Error::Capacity);
            }
            let value = fragment(&json, 0, through)?;
            if value["next_offset"] == value["total_bytes"] {
                routes.caller(request).map_err(|_| Error::Cancelled)?;
            } else {
                routes
                    .retain_resource(
                        request,
                        key,
                        Arc::new(Page {
                            identity,
                            through,
                            json: json.into_boxed_str(),
                            _permit: permit,
                        }),
                    )
                    .map_err(|_| Error::Cancelled)?;
            }
            return Ok(value);
        }
        let (page, retired) = routes
            .shared_resource::<Page>(request, &key)
            .map_err(|_| Error::Cancelled)?;
        if retired.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if page.identity != identity {
            return Err(Error::Protocol);
        }
        let value = fragment(&page.json, offset, page.through)?;
        if value["next_offset"] == value["total_bytes"] {
            routes.release_resource(request, &key);
        }
        Ok(value)
    }
}

#[cfg(test)]
pub(super) mod tests;
