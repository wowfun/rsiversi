use crate::{Error, PendingPermission, Permission, PermissionOption, Result, State, Transition};
use rsi_acp::{Incoming, Peer};
use rsi_acp_journal::RecordKind;
use rsi_acp_protocol::Message;
use serde_json::json;
use std::sync::Arc;

pub(super) async fn run(mut peer: Peer, state: Arc<State>) -> Result<()> {
    let result: Result<()> = async {
        let mut pending = None;
        loop {
            let incoming = if let Some(incoming) = pending.take() {
                incoming
            } else {
                tokio::select! { biased;
                    () = state.stop.cancelled() => return Ok(()),
                    incoming = peer.next() => incoming.ok_or(Error::Unknown)?,
                }
            };
            if is_update(&incoming) {
                let mut bytes = update_size(&incoming)?;
                let mut batch = vec![incoming];
                while batch.len() < 64 {
                    let Some(next) = peer.try_next() else {
                        break;
                    };
                    if !is_update(&next) {
                        pending = Some(next);
                        break;
                    }
                    let size = update_size(&next)?;
                    if bytes + size > rsi_acp_protocol::MAX_FRAME_BYTES {
                        pending = Some(next);
                        break;
                    }
                    bytes += size;
                    batch.push(next);
                }
                let mut records = Vec::with_capacity(batch.len());
                for incoming in &batch {
                    let Message::Notification { params, .. } = &incoming.message else {
                        unreachable!()
                    };
                    rsi_acp_protocol::validate_session_update(params).map_err(|_| Error::Input)?;
                    state.bind_target(params["sessionId"].as_str().ok_or(Error::Input)?)?;
                    records.push((RecordKind::Update, params["update"].clone()));
                }
                state
                    .journal
                    .append_batch(&state.id, state.generation, records)
                    .await
                    .map_err(|_| Error::Journal)?;
                state
                    .processed
                    .send_replace(batch.last().expect("nonempty incoming batch").ordinal);
            } else {
                let ordinal = incoming.ordinal;
                accept(&state, incoming).await?;
                state.processed.send_replace(ordinal);
            }
            state.changed();
        }
    }
    .await;
    if result.is_err() {
        state.stop.cancel();
    }
    state.permissions.lock().expect("ACP permissions").clear();
    let closed = peer.close().await.map_err(|_| Error::Unknown);
    let _recorded = state.transition(Transition::Disconnected).await;
    state.changed();
    closed
}

fn update_size(incoming: &Incoming) -> Result<usize> {
    let Message::Notification { params, .. } = &incoming.message else {
        return Err(Error::Input);
    };
    encoded_size(&params["update"])
}

fn encoded_size(value: &serde_json::Value) -> Result<usize> {
    // Canonical JSON can be larger than the wire spelling (notably numbers).
    // Count without allocating a second serialized frame before batch admission.
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > rsi_acp_protocol::MAX_FRAME_BYTES - self.0 {
                return Err(std::io::ErrorKind::OutOfMemory.into());
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).map_err(|_| Error::Journal)?;
    Ok(counter.0)
}

fn is_update(incoming: &Incoming) -> bool {
    matches!(&incoming.message, Message::Notification { method, .. } if method == "session/update")
}

async fn accept(state: &Arc<State>, incoming: Incoming) -> Result<()> {
    match &incoming.message {
        Message::Request { method, params, .. } if method == "session/request_permission" => {
            rsi_acp_protocol::validate_permission(params).map_err(|_| Error::Input)?;
            state.bind_target(params["sessionId"].as_str().ok_or(Error::Input)?)?;
            let sequence = state
                .journal
                .append(
                    &state.id,
                    state.generation,
                    RecordKind::Permission,
                    params.clone(),
                )
                .await
                .map_err(|_| Error::Journal)?;
            let mut title = params["toolCall"]["title"]
                .as_str()
                .unwrap_or("External tool")
                .to_owned();
            if title.len() > 512 {
                let mut end = 512;
                while !title.is_char_boundary(end) {
                    end -= 1;
                }
                title.truncate(end);
            }
            let options = params["options"]
                .as_array()
                .ok_or(Error::Input)?
                .iter()
                .map(|option| {
                    Ok(PermissionOption {
                        id: option["optionId"].as_str().ok_or(Error::Input)?.into(),
                        name: label(option["name"].as_str().ok_or(Error::Input)?, 128),
                        kind: option["kind"].as_str().ok_or(Error::Input)?.into(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let view = Permission {
                id: format!("permission-{}", incoming.ordinal),
                generation: state.generation.to_string(),
                title,
                options,
                source_sequence: sequence.to_string(),
            };
            let cancelled = {
                let mut permissions = state.permissions.lock().expect("ACP permissions");
                if permissions.len() >= rsi_acp_protocol::MAX_PENDING {
                    return Err(Error::Busy);
                }
                let cancelled = state
                    .active
                    .lock()
                    .expect("ACP active prompt")
                    .as_ref()
                    .is_none_or(tokio_util::sync::CancellationToken::is_cancelled);
                permissions.insert(view.id.clone(), PendingPermission { incoming, view });
                cancelled
            };
            if cancelled {
                cancel_permissions(state).await?;
            }
        }
        Message::Request { id, .. } => {
            tokio::select! { biased;
                () = state.stop.cancelled() => return Ok(()),
                result = unsupported_answer(state, id) => result?,
            }
        }
        Message::Notification { .. } => {}
        Message::Response { .. } => return Err(Error::Input),
    }
    Ok(())
}

pub(super) fn close_permission_admission(state: &State) {
    let _admission = state.permissions.lock().expect("ACP permissions");
    state.active.lock().expect("ACP active prompt").take();
}

pub(super) async fn answer(
    state: &Arc<State>,
    generation: u64,
    permission: &str,
    option: &str,
) -> Result<()> {
    let task = {
        let mut permissions = state.permissions.lock().expect("ACP permissions");
        state.accepting()?;
        if state
            .active
            .lock()
            .expect("ACP active prompt")
            .as_ref()
            .is_none_or(tokio_util::sync::CancellationToken::is_cancelled)
        {
            return Err(Error::Stale);
        }
        if generation != state.generation {
            return Err(Error::Stale);
        }
        let pending = permissions.get(permission).ok_or(Error::Stale)?;
        if !pending
            .view
            .options
            .iter()
            .any(|allowed| allowed.id == option)
        {
            return Err(Error::Input);
        }
        let Message::Request { id, .. } = &pending.incoming.message else {
            unreachable!("validated permission");
        };
        let receipt = state
            .port
            .try_respond(
                id,
                Ok(&json!({"outcome":{"outcome":"selected","optionId":option}})),
            )
            .map_err(|error| {
                if error == rsi_acp::Error::Capacity {
                    Error::Busy
                } else {
                    state.port.abort();
                    state.stop.cancel();
                    Error::Unknown
                }
            })?;
        permissions.remove(permission);
        flush(state, [receipt])
    };
    state.changed();
    task.await.map_err(|_| Error::Unknown)?
}

pub(super) async fn cancel_permissions(state: &Arc<State>) -> Result<()> {
    let task = {
        let mut permissions = state.permissions.lock().expect("ACP permissions");
        let mut receipts = Vec::with_capacity(permissions.len());
        for pending in permissions.values() {
            let Message::Request { id, .. } = &pending.incoming.message else {
                unreachable!("validated permission")
            };
            if let Ok(receipt) = state
                .port
                .try_respond(id, Ok(&json!({"outcome":{"outcome":"cancelled"}})))
            {
                receipts.push(receipt);
            } else {
                state.port.abort();
                state.stop.cancel();
                permissions.clear();
                state.changed();
                return Err(Error::Unknown);
            }
        }
        permissions.clear();
        flush(state, receipts)
    };
    state.changed();
    task.await.map_err(|_| Error::Unknown)?
}

fn flush<I>(state: &Arc<State>, receipts: I) -> tokio::task::JoinHandle<Result<()>>
where
    I: IntoIterator<Item = rsi_acp::ResponseWrite> + Send + 'static,
    I::IntoIter: Send,
{
    let owner = state.clone();
    state.tasks.spawn(async move {
        let result = tokio::time::timeout(crate::CONTROL, async {
            for receipt in receipts {
                receipt.wait().await?;
            }
            Ok::<_, rsi_acp::Error>(())
        })
        .await;
        if matches!(result, Ok(Ok(()))) {
            Ok(())
        } else {
            owner.port.abort();
            owner.stop.cancel();
            Err(Error::Unknown)
        }
    })
}

fn label(value: &str, maximum: usize) -> String {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

pub(super) async fn unsupported_answer(
    state: &State,
    id: &rsi_acp_protocol::RequestId,
) -> Result<()> {
    let unsupported = json!({"code":-32601,"message":"Method not supported"});
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match state.port.try_respond(id, Err(&unsupported)) {
                Ok(receipt) => return receipt.wait().await,
                Err(rsi_acp::Error::Capacity) => state.port.drain().await?,
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .map_err(|_| Error::Unknown)?
    .map_err(|_| Error::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_admission_counts_canonical_bytes_and_enforces_frame_bound() {
        let wire = br#"{"number":1e9,"text":"\u0001"}"#;
        let value: serde_json::Value = serde_json::from_slice(wire).unwrap();
        let canonical = serde_json::to_vec(&value).unwrap();
        assert!(canonical.len() > wire.len());
        assert_eq!(encoded_size(&value).unwrap(), canonical.len());
        let exact = json!("x".repeat(rsi_acp_protocol::MAX_FRAME_BYTES - 2));
        assert_eq!(
            encoded_size(&exact).unwrap(),
            rsi_acp_protocol::MAX_FRAME_BYTES
        );
        let oversized = json!("x".repeat(rsi_acp_protocol::MAX_FRAME_BYTES - 1));
        assert_eq!(encoded_size(&oversized), Err(Error::Journal));
    }
}
