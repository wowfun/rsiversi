use crate::{
    validate,
    wire::{self, Operation},
};
use async_trait::async_trait;
use rsi_acp_protocol::{
    observation::{ConversationId, Page, Snapshot},
    service::{Endpoint, Error, ExternalConversations, Resident, Result, Setup, View},
};
use rsi_api_protocol::{ApiClient, ApiError, call_json};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::BTreeSet, sync::Arc};
/// Negotiated finite proxy. Dropping it leaves external peer ownership in the Host.
#[derive(Clone, Debug)]
pub struct Client {
    api: Arc<dyn ApiClient>,
}
impl Client {
    /// Requires the exact complete operation catalog before publishing this capability.
    pub fn new(api: Arc<dyn ApiClient>) -> rsi_api_protocol::Result<Self> {
        if Operation::ALL
            .iter()
            .any(|operation| !api.operations().contains(&operation.spec()))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    async fn call<I: Serialize + Sync, O: DeserializeOwned>(
        &self,
        operation: Operation,
        input: &I,
    ) -> Result<O> {
        call_json::<_, O, Error>(self.api.as_ref(), &operation.spec(), input)
            .await
            .map_err(|error| {
                if error == ApiError::Capacity {
                    Error::Busy
                } else {
                    Error::Unknown
                }
            })?
    }
    async fn state<I: Serialize + Sync>(
        &self,
        operation: Operation,
        input: &I,
        id: &ConversationId,
    ) -> Result<Snapshot> {
        let snapshot = self.call(operation, input).await?;
        validate::snapshot(&snapshot, Some(id)).map_err(|_| Error::Unknown)?;
        Ok(snapshot)
    }
}
#[async_trait]
impl ExternalConversations for Client {
    async fn endpoints(&self) -> Result<Vec<Endpoint>> {
        let values: Vec<Endpoint> = self.call(Operation::Endpoints, &wire::Empty {}).await?;
        let mut ids = BTreeSet::new();
        if values.len() > 64 {
            return Err(Error::Input);
        }
        for endpoint in &values {
            ConversationId::new(&endpoint.id).map_err(|_| Error::Input)?;
            if endpoint.id.len() > 64 || !ids.insert(&endpoint.id) {
                return Err(Error::Input);
            }
        }
        Ok(values)
    }
    async fn residents(&self) -> Result<Vec<Resident>> {
        let values: Vec<Resident> = self.call(Operation::Residents, &wire::Empty {}).await?;
        let mut ids = BTreeSet::new();
        if values.len() > 8 {
            return Err(Error::Input);
        }
        for value in &values {
            validate::resident(value)?;
            if !ids.insert(&value.snapshot.id) {
                return Err(Error::Input);
            }
        }
        Ok(values)
    }
    async fn list(&self, after: Option<ConversationId>) -> Result<Vec<Snapshot>> {
        let values: Vec<Snapshot> = self
            .call(
                Operation::List,
                &wire::List {
                    after: after.clone(),
                },
            )
            .await?;
        if values.len() > 64 {
            return Err(Error::Input);
        }
        let mut previous = after.as_ref();
        for value in &values {
            validate::snapshot(value, None)?;
            if previous.is_some_and(|id| *id >= value.id) {
                return Err(Error::Input);
            }
            previous = Some(&value.id);
        }
        Ok(values)
    }
    async fn view(&self, id: &ConversationId) -> Result<View> {
        let value: View = self
            .call(Operation::View, &wire::Target { id: id.clone() })
            .await?;
        validate::view(&value, id)?;
        Ok(value)
    }
    async fn start(&self, id: ConversationId, endpoint: &str) -> Result<Snapshot> {
        self.state(
            Operation::Start,
            &wire::Start {
                id: id.clone(),
                endpoint: endpoint.into(),
            },
            &id,
        )
        .await
    }
    async fn reconnect(&self, id: &ConversationId, setup: Setup) -> Result<Snapshot> {
        self.state(
            Operation::Reconnect,
            &wire::Reconnect {
                id: id.clone(),
                setup,
            },
            id,
        )
        .await
    }
    async fn submit(&self, id: &ConversationId, text: &str) -> Result<Snapshot> {
        self.state(
            Operation::Submit,
            &wire::Submit {
                id: id.clone(),
                text: text.into(),
            },
            id,
        )
        .await
    }
    async fn cancel(&self, id: &ConversationId) -> Result<Snapshot> {
        self.state(Operation::Cancel, &wire::Target { id: id.clone() }, id)
            .await
    }
    async fn close(&self, id: &ConversationId) -> Result<Snapshot> {
        self.state(Operation::Close, &wire::Target { id: id.clone() }, id)
            .await
    }
    async fn answer(
        &self,
        id: &ConversationId,
        generation: u64,
        permission: &str,
        option: &str,
    ) -> Result<()> {
        self.call(
            Operation::Answer,
            &wire::Answer {
                id: id.clone(),
                generation: generation.to_string(),
                permission: permission.into(),
                option: option.into(),
            },
        )
        .await
    }
    async fn page(&self, id: &ConversationId, epoch: u64, after: u64) -> Result<Page> {
        let request = wire::PageRequest {
            id: id.clone(),
            epoch: epoch.to_string(),
            after: after.to_string(),
        };
        let result: wire::PageReply = self.call(Operation::Page, &request).await?;
        if result.source != request {
            return Err(Error::Input);
        }
        validate::page(&result.page, epoch, after)?;
        Ok(result.page)
    }
    async fn window(
        &self,
        id: &ConversationId,
        epoch: u64,
        sequence: u64,
        start: usize,
    ) -> Result<Vec<u8>> {
        let request = wire::WindowRequest {
            id: id.clone(),
            epoch: epoch.to_string(),
            sequence: sequence.to_string(),
            start,
        };
        let result: wire::WindowReply = self.call(Operation::Window, &request).await?;
        if result.source != request
            || result.hex.len() > 128 * 1024
            || result
                .hex
                .bytes()
                .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
        {
            return Err(Error::Input);
        }
        hex::decode(result.hex).map_err(|_| Error::Input)
    }
}
