use crate::{Resident, State, client, journal, launch};
use rsi_acp_client::Client;
use rsi_acp_journal::{ConversationId, Snapshot, Status};
use rsi_acp_protocol::service::{Error, Result, Setup};
use std::{sync::Arc, time::Duration};
use tokio::sync::{oneshot, watch};

pub(super) async fn open(
    state: Arc<State>,
    id: ConversationId,
    endpoint_id: &str,
    setup: Setup,
) -> Result<Snapshot> {
    let endpoint = state
        .endpoints
        .iter()
        .find(|endpoint| endpoint.id == endpoint_id && endpoint.enabled)
        .cloned()
        .ok_or(Error::NotFound)?;
    let (accepted, receive) = oneshot::channel();
    let stop = state.stop.child_token();
    let (done, _) = watch::channel(None);
    {
        let mut residents = state.residents.lock().expect("ACP residents");
        if state.stop.is_cancelled() {
            return Err(Error::Unknown);
        }
        if residents.contains_key(&id) || residents.len() >= 8 {
            return Err(Error::Busy);
        }
        residents.insert(
            id.clone(),
            Resident {
                endpoint: endpoint.id.clone(),
                stop: stop.clone(),
                done: done.clone(),
                handle: None,
            },
        );
        let owner = state.clone();
        state.tasks.spawn(async move {
            let mut client_owner = None;
            let mut reserved = None;
            let prepare = async {
                let servers = tokio::time::timeout(
                    Duration::from_secs(45),
                    prepare_local(
                        &owner,
                        &id,
                        &endpoint,
                        setup,
                        &mut client_owner,
                        &mut reserved,
                    ),
                )
                .await
                .map_err(|_| Error::Unknown)??;
                client_owner
                    .as_ref()
                    .ok_or(Error::Unknown)?
                    .initialize(setup, servers, &endpoint.session_options)
                    .await
                    .map_err(client)
            };
            let outcome = tokio::select! { biased;
                () = stop.cancelled() => Err(Error::Unknown),
                result = prepare => result,
            };
            let succeeded = outcome.is_ok();
            let error = outcome.as_ref().err().copied();
            let _accepted = accepted.send(outcome);
            if succeeded {
                stop.cancelled().await;
            }
            let acquired = client_owner.is_some() || reserved.is_some();
            let closed = if let Some(connection) = client_owner {
                connection.close().await.map_err(client)
            } else if let Some(snapshot) = reserved {
                let status = if matches!(error, Some(Error::Launch | Error::Input)) {
                    Status::Failed
                } else {
                    Status::Unknown
                };
                owner
                    .journal
                    .settle(&id, snapshot.generation, status)
                    .await
                    .map_err(journal)
            } else {
                Err(error.unwrap_or(Error::Unknown))
            };
            // Release the slot only after Client close has joined Process reaping.
            if !acquired || closed.is_ok() {
                owner.residents.lock().expect("ACP residents").remove(&id);
            } else {
                owner
                    .failed_cleanup
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            done.send_replace(Some(closed));
        });
    }
    receive.await.map_err(|_| Error::Unknown)?
}

async fn prepare_local(
    owner: &State,
    id: &ConversationId,
    endpoint: &crate::config::EndpointConfig,
    setup: Setup,
    client_owner: &mut Option<Client>,
    reserved: &mut Option<Snapshot>,
) -> Result<Vec<rsi_acp_protocol::schema::McpServer>> {
    let cwd = tokio::fs::canonicalize(&endpoint.cwd)
        .await
        .map_err(|_| Error::Launch)?;
    let cwd_text = cwd.to_str().ok_or(Error::Input)?.to_owned();
    if setup == Setup::New {
        *reserved = Some(
            owner
                .journal
                .create(id.clone(), endpoint.id.clone(), cwd_text.clone())
                .await
                .map_err(journal)?,
        );
    } else {
        let saved = owner.journal.get(id).await.map_err(journal)?;
        if saved.endpoint != endpoint.id || saved.cwd != cwd_text || saved.remote.is_none() {
            return Err(Error::Stale);
        }
    }
    let snapshot = owner.journal.connect(id).await.map_err(journal)?;
    *reserved = Some(snapshot.clone());
    let (peer, servers) = launch::open(owner, endpoint, &cwd).await?;
    let connection = Client::attach(peer, owner.journal.clone(), snapshot).map_err(client)?;
    owner
        .residents
        .lock()
        .expect("ACP residents")
        .get_mut(id)
        .ok_or(Error::Stale)?
        .handle = Some(connection.handle());
    *client_owner = Some(connection);
    Ok(servers)
}

pub(super) async fn close(state: &State, id: &ConversationId) -> Result<Snapshot> {
    let pending = {
        let residents = state.residents.lock().expect("ACP residents");
        residents.get(id).map(|resident| {
            resident.stop.cancel();
            resident.done.subscribe()
        })
    };
    let Some(mut done) = pending else {
        return state.journal.get(id).await.map_err(journal);
    };
    loop {
        if let Some(result) = done.borrow_and_update().clone() {
            return result;
        }
        done.changed().await.map_err(|_| Error::Unknown)?;
    }
}
