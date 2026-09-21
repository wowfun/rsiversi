use crate::{Error, PendingPermission, Permission, PermissionOption, Result, State};
use rsi_acp::{Incoming, Peer};
use rsi_acp_journal::{RecordKind, Status};
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
    if closed.is_err()
        || matches!(
            state.snapshot().status,
            Status::Starting | Status::Ready | Status::Loading | Status::Running
        )
    {
        let _recorded = state.status(Status::Unknown).await;
    }
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

async fn accept(state: &State, incoming: Incoming) -> Result<()> {
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
                    .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
                permissions.insert(view.id.clone(), PendingPermission { incoming, view });
                cancelled
            };
            if cancelled {
                cancel_permissions(state).await?;
            }
        }
        Message::Request { id, .. } => state
            .port
            .respond(
                id,
                Err(&json!({"code":-32601,"message":"Method not supported"})),
            )
            .await
            .map_err(|_| Error::Unknown)?,
        Message::Notification { .. } => {}
        Message::Response { .. } => return Err(Error::Input),
    }
    Ok(())
}

pub(super) async fn answer(
    state: &State,
    generation: u64,
    permission: &str,
    option: &str,
) -> Result<()> {
    state.accepting()?;
    if generation != state.generation {
        return Err(Error::Stale);
    }
    let pending = {
        let mut permissions = state.permissions.lock().expect("ACP permissions");
        let pending = permissions.get(permission).ok_or(Error::Stale)?;
        if !pending
            .view
            .options
            .iter()
            .any(|allowed| allowed.id == option)
        {
            return Err(Error::Input);
        }
        permissions.remove(permission).ok_or(Error::Stale)?
    };
    let Message::Request { id, .. } = &pending.incoming.message else {
        return Err(Error::Input);
    };
    let result = state
        .port
        .respond(
            id,
            Ok(&json!({"outcome":{"outcome":"selected","optionId":option}})),
        )
        .await
        .map_err(|_| Error::Unknown);
    state.changed();
    result
}

pub(super) async fn cancel_permissions(state: &State) -> Result<()> {
    let permissions = std::mem::take(&mut *state.permissions.lock().expect("ACP permissions"));
    for pending in permissions.into_values() {
        let Message::Request { id, .. } = &pending.incoming.message else {
            return Err(Error::Input);
        };
        state
            .port
            .respond(id, Ok(&json!({"outcome":{"outcome":"cancelled"}})))
            .await
            .map_err(|_| Error::Unknown)?;
    }
    state.changed();
    Ok(())
}

fn label(value: &str, maximum: usize) -> String {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
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
