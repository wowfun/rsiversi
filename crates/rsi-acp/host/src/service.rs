use crate::{Owner, client, journal, lifecycle};
use async_trait::async_trait;
use rsi_acp_client::Handle;
use rsi_acp_journal::{ConversationId, Page, Snapshot};
use rsi_acp_protocol::{
    schema::{ContentBlock, TextContent},
    service::{
        Endpoint, Error, ExternalConversations, PermissionSummary, Resident, Result, Setup, View,
    },
};
fn view(handle: &Handle) -> View {
    View {
        snapshot: handle.snapshot(),
        connected: handle.connected(),
        permissions: handle.permissions(),
    }
}
#[async_trait]
impl ExternalConversations for Owner {
    async fn endpoints(&self) -> Result<Vec<Endpoint>> {
        Ok(self
            .state
            .endpoints
            .iter()
            .map(|endpoint| Endpoint {
                id: endpoint.id.clone(),
                enabled: endpoint.enabled,
            })
            .collect())
    }
    async fn residents(&self) -> Result<Vec<Resident>> {
        let mut rows: Vec<_> = self
            .state
            .residents
            .lock()
            .expect("ACP residents")
            .values()
            .filter_map(|resident| {
                resident.handle.as_ref().map(|handle| Resident {
                    snapshot: handle.snapshot(),
                    sequence: "0".into(),
                    connected: handle.connected(),
                    permissions: handle
                        .permissions()
                        .into_iter()
                        .map(|permission| PermissionSummary {
                            id: permission.id,
                            generation: permission.generation,
                            title: permission.title,
                        })
                        .collect(),
                })
            })
            .collect();
        for row in &mut rows {
            row.sequence = self
                .state
                .journal
                .position(&row.snapshot.id, row.snapshot.epoch)
                .await
                .map_err(journal)?
                .to_string();
        }
        Ok(rows)
    }
    async fn list(&self, after: Option<ConversationId>) -> Result<Vec<Snapshot>> {
        self.state.journal.list(after).await.map_err(journal)
    }
    async fn view(&self, id: &ConversationId) -> Result<View> {
        if let Ok(handle) = self.state.handle(id) {
            return Ok(view(&handle));
        }
        Ok(View {
            snapshot: self.state.journal.get(id).await.map_err(journal)?,
            connected: false,
            permissions: vec![],
        })
    }
    async fn start(&self, id: ConversationId, endpoint: &str) -> Result<Snapshot> {
        {
            let residents = self.state.residents.lock().expect("ACP residents");
            if let Some(resident) = residents.get(&id) {
                if resident.endpoint != endpoint {
                    return Err(Error::Stale);
                }
                return resident
                    .handle
                    .as_ref()
                    .map(Handle::snapshot)
                    .ok_or(Error::Busy);
            }
        }
        match self.state.journal.get(&id).await {
            Ok(snapshot) if snapshot.endpoint == endpoint => return Ok(snapshot),
            Ok(_) => return Err(Error::Stale),
            Err(rsi_acp_journal::Error::NotFound) => {}
            Err(error) => return Err(journal(error)),
        }
        lifecycle::open(self.state.clone(), id, endpoint, Setup::New).await
    }
    async fn reconnect(&self, id: &ConversationId, setup: Setup) -> Result<Snapshot> {
        if setup == Setup::New {
            return Err(Error::Input);
        }
        let saved = self.state.journal.get(id).await.map_err(journal)?;
        lifecycle::open(self.state.clone(), id.clone(), &saved.endpoint, setup).await
    }
    async fn submit(&self, id: &ConversationId, text: &str) -> Result<Snapshot> {
        if text.is_empty() || text.len() > 512 * 1024 {
            return Err(Error::Input);
        }
        self.state
            .handle(id)?
            .submit(vec![ContentBlock::Text(TextContent::new(text))])
            .await
            .map_err(client)
    }
    async fn cancel(&self, id: &ConversationId) -> Result<Snapshot> {
        self.state.handle(id)?.cancel().await.map_err(client)
    }
    async fn close(&self, id: &ConversationId) -> Result<Snapshot> {
        lifecycle::close(&self.state, id).await
    }
    async fn answer(
        &self,
        id: &ConversationId,
        generation: u64,
        permission: &str,
        option: &str,
    ) -> Result<()> {
        self.state
            .handle(id)?
            .answer(generation, permission, option)
            .await
            .map_err(client)
    }
    async fn page(&self, id: &ConversationId, epoch: u64, after: u64) -> Result<Page> {
        self.state
            .journal
            .page(id, epoch, after)
            .await
            .map_err(journal)
    }
    async fn window(
        &self,
        id: &ConversationId,
        epoch: u64,
        sequence: u64,
        start: usize,
    ) -> Result<Vec<u8>> {
        self.state
            .journal
            .window(id, epoch, sequence, start)
            .await
            .map_err(journal)
    }
}
