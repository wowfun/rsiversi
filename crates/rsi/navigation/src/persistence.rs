use super::{
    ApiError, Document, Domain, DomainSpec, MetadataSize, NAVIGATION_STORAGE_VERSION, Result,
    Schema, SessionId, StoredMetadata, WorkspaceAccount, WorkspaceId, metadata_bytes,
    storage_error,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub(super) struct Revision {
    pub revision: u64,
}

pub(super) fn domain_spec(backend: String) -> DomainSpec {
    DomainSpec {
        id: "rsi.navigation".into(),
        backend,
        version: NAVIGATION_STORAGE_VERSION,
        maximum_records: 8192 * 2 + 1,
        // Keep the existing logical document limit. Table address prefixes and
        // the global wrapper consume additional backend accounting bytes.
        maximum_bytes: 8 * 1024 * 1024 + 8192 * (b"metadata/".len() + b"orders/".len()) + 128,
        ..DomainSpec::default()
    }
}

pub(super) fn domain_schema() -> std::result::Result<Schema, rsi_storage::StorageError> {
    Schema::default()
        .global::<Revision>()?
        .table::<StoredMetadata>("metadata")?
        .table::<WorkspaceAccount>("orders")
}

pub(super) fn load_document(domain: &Domain) -> Result<Document> {
    if !domain.capabilities().atomic_batch {
        return Err(ApiError::Invalid(
            "navigation requires atomic storage batches".into(),
        ));
    }
    let revision = domain
        .global::<Revision>()
        .and_then(|value| value.get())
        .map_err(storage_error)?;
    let records = domain
        .table::<StoredMetadata>("metadata")
        .and_then(|table| table.snapshot())
        .map_err(storage_error)?;
    let orders = domain
        .table::<WorkspaceAccount>("orders")
        .and_then(|table| table.snapshot())
        .map_err(storage_error)?;
    if revision.is_none() && (!records.is_empty() || !orders.is_empty()) {
        return Err(ApiError::Invalid("navigation revision is missing".into()));
    }
    let mut document = Document {
        revision: revision.map_or(0, |value| value.revision),
        ..Document::default()
    };
    for (key, value) in records.entries() {
        let key = SessionId::new(key)
            .map_err(|_| ApiError::Invalid("invalid metadata identity".into()))?;
        document.records.insert(key, (*value).clone());
    }
    for (key, value) in orders.entries() {
        let key = WorkspaceId::parse(key)
            .map_err(|_| ApiError::Invalid("invalid account identity".into()))?;
        document.workspace_orders.insert(key, (*value).clone());
    }
    document.validate()?;
    Ok(document)
}

/// A bounded projection under the Navigation writer, published only after one
/// atomic durable batch. Unrelated records are neither copied nor encoded here.
pub(super) struct DocumentChange {
    pub revision: u64,
    pub accounting: MetadataSize,
    pub records: Vec<(SessionId, Option<StoredMetadata>)>,
    pub orders: Vec<(WorkspaceId, Option<WorkspaceAccount>)>,
}

impl DocumentChange {
    pub fn apply(self, document: &mut Document) {
        for (id, value) in self.records {
            if let Some(value) = value {
                document.records.insert(id, value);
            } else {
                document.records.remove(&id);
            }
        }
        for (id, account) in self.orders {
            if let Some(account) = account {
                document.workspace_orders.insert(id, account);
            } else {
                document.workspace_orders.remove(&id);
            }
        }
        document.revision = self.revision;
        document.accounting = Some(self.accounting);
    }

    pub fn purge(
        document: &Document,
        targets: &[rsi_agent_store_protocol::PurgeTarget],
    ) -> Result<Option<Self>> {
        let removed: HashSet<_> = targets.iter().map(|target| &target.session_id).collect();
        let mut change = Self {
            revision: document.revision,
            accounting: document
                .accounting
                .expect("validated navigation accounting"),
            records: Vec::new(),
            orders: Vec::new(),
        };
        for (id, previous) in &document.workspace_orders {
            if !previous.members.iter().any(|id| removed.contains(id)) {
                continue;
            }
            let members: Vec<_> = previous
                .members
                .iter()
                .filter(|id| !removed.contains(id))
                .cloned()
                .collect();
            let account = WorkspaceAccount {
                updated_at_ms: super::workspace_account::account_time()?,
                members,
            };
            change.set_account(document, id.clone(), Some(account))?;
        }
        for id in removed {
            if let Some(previous) = document.records.get(id) {
                change.accounting.records = change
                    .accounting
                    .records
                    .without_entry(metadata_bytes(id, previous)?)
                    .map_err(storage_error)?;
                change.accounting.pins -= usize::from(previous.pinned);
                change.records.push((id.clone(), None));
            }
        }
        if change.records.is_empty() && change.orders.is_empty() {
            return Ok(None);
        }
        // Preserve the existing revision advance for each affected metadata/order
        // entry while publishing all of them in one transaction.
        change.revision = change
            .revision
            .checked_add((change.records.len() + change.orders.len()) as u64)
            .ok_or_else(|| ApiError::Invalid("navigation revision exhausted".into()))?;
        change.accounting.check(change.revision)?;
        Ok(Some(change))
    }
}

#[cfg(test)]
impl Document {
    pub(super) fn edit(
        &mut self,
        id: &SessionId,
        metadata: &rsi_navigation_api::SessionMetadata,
    ) -> Result<()> {
        if self.accounting.is_none() {
            self.validate()?;
        }
        self.project_metadata(id, metadata)?.apply(self);
        Ok(())
    }
    pub(super) fn edit_order(&mut self, id: &WorkspaceId, members: Vec<SessionId>) -> Result<()> {
        if self.accounting.is_none() {
            self.validate()?;
        }
        self.project_orders(vec![(id.clone(), Some(members))])?
            .apply(self);
        Ok(())
    }
}
