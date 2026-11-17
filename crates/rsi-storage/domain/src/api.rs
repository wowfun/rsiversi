use crate::{
    Change, ChangeEvent, Global, Schema, Snapshot, StorageError, Table,
    queue::{Authority, Operation},
};
use async_trait::async_trait;
use rsi_meta::LocalContract;
use rsi_storage::{
    Address, Capabilities, Layout, MAXIMUM_STORAGE_DOMAIN_BYTES, MAXIMUM_STORAGE_RECORDS, Recovery,
    validate_identifier,
};
use serde::{Deserialize, Serialize};
use std::{any::TypeId, collections::BTreeSet, fmt, marker::PhantomData, sync::Arc};
use tokio::sync::broadcast;

/// Immutable identity, routing and resource declaration of one business domain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainSpec {
    /// Exact consumer-owned identity.
    pub id: String,
    /// Exact registered backend route.
    pub backend: String,
    /// Current consumer schema version.
    pub version: u32,
    /// Older versions accepted by the same current codecs; reads never restamp.
    #[serde(default)]
    pub compatible_versions: BTreeSet<u32>,
    /// Requested JSON layout; row stores retain unit-wide versioning.
    #[serde(default)]
    pub layout: Layout,
    /// Maximum present records, including the global when written.
    pub maximum_records: usize,
    /// Exact maximum bytes in the core logical record object.
    pub maximum_bytes: usize,
}

impl Default for DomainSpec {
    fn default() -> Self {
        Self {
            id: String::new(),
            backend: String::new(),
            version: 1,
            compatible_versions: BTreeSet::new(),
            layout: Layout::Single,
            maximum_records: MAXIMUM_STORAGE_RECORDS,
            maximum_bytes: MAXIMUM_STORAGE_DOMAIN_BYTES,
        }
    }
}

impl DomainSpec {
    pub(crate) fn admit_batch_size(&self, records: usize) -> Result<(), StorageError> {
        if records > 2 * self.maximum_records {
            return Err(StorageError::InvalidInput("batch capacity exceeded".into()));
        }
        Ok(())
    }
    pub(crate) fn validate(&self) -> Result<(), StorageError> {
        validate_identifier("domain", &self.id)?;
        validate_identifier("backend", &self.backend)?;
        if self.maximum_records == 0 || self.maximum_records > MAXIMUM_STORAGE_RECORDS {
            return Err(StorageError::InvalidInput(format!(
                "maximum_records must be within 1..={MAXIMUM_STORAGE_RECORDS}"
            )));
        }
        if self.maximum_bytes < 2 || self.maximum_bytes > MAXIMUM_STORAGE_DOMAIN_BYTES {
            return Err(StorageError::InvalidInput(format!(
                "maximum_bytes must be within 2..={MAXIMUM_STORAGE_DOMAIN_BYTES}"
            )));
        }
        Ok(())
    }
}

/// One open typed domain. Clones share its write queue and committed state.
#[derive(Clone, Debug)]
pub struct Domain {
    pub(crate) authority: Arc<Authority>,
}

impl Domain {
    /// Fixed declaration retained until the domain closes.
    pub fn spec(&self) -> &DomainSpec {
        &self.authority.spec
    }
    /// Local generation-health check, without durable I/O.
    pub fn ensure_available(&self) -> Result<(), StorageError> {
        self.authority.ensure_available()
    }
    /// Capabilities of this exact route and requested layout.
    pub fn capabilities(&self) -> Capabilities {
        self.authority
            .loaded()
            .backend
            .capabilities(self.spec().layout)
    }
    /// Takes a shared immutable committed snapshot without copying values.
    pub fn snapshot(&self) -> Result<Snapshot, StorageError> {
        self.authority.snapshot()
    }
    /// Atomically takes a snapshot and subscribes to later commits.
    /// A lagged receiver must resnapshot and subscribe again.
    pub fn snapshot_and_subscribe(
        &self,
    ) -> Result<(Snapshot, broadcast::Receiver<ChangeEvent>), StorageError> {
        self.authority.snapshot_and_subscribe()
    }
    /// Preserved disposable records omitted while opening, with no payload values.
    pub fn recoveries(&self) -> &[Recovery] {
        &self.authority.loaded().recoveries
    }
    /// Resolves a typed table declared at open; no I/O or value decoding occurs.
    pub fn table<T: Send + Sync + 'static>(&self, name: &str) -> Result<Table<T>, StorageError> {
        self.ensure_available()?;
        let address = Address::record(name, "")?;
        self.authority
            .schema
            .codec(&address)?
            .check_type(TypeId::of::<T>())?;
        Ok(Table {
            domain: self.clone(),
            address,
            marker: PhantomData,
        })
    }
    /// Resolves the typed global declared at open.
    pub fn global<T: Send + Sync + 'static>(&self) -> Result<Global<T>, StorageError> {
        self.ensure_available()?;
        self.authority
            .schema
            .codec(&Address::global())?
            .check_type(TypeId::of::<T>())?;
        Ok(Global {
            domain: self.clone(),
            marker: PhantomData,
        })
    }
    /// Publishes unique typed replacements/deletions in one queue slot.
    /// Arbitrary multi-record writes require backend atomic-batch support.
    pub async fn batch(&self, changes: Vec<Change>) -> Result<usize, StorageError> {
        self.spec().admit_batch_size(changes.len())?;
        let admission = self.authority.admit().await?;
        Ok(self
            .authority
            .submit(Operation::Changes(changes), admission)
            .await?
            .changed)
    }
    /// Closes admission immediately; the returned future waits for accepted work.
    /// Dropping that future never reopens admission or cancels accepted writes.
    pub fn close(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        self.authority.begin_close();
        let owner = Arc::clone(&self.authority);
        async move { owner.drained().await }
    }
}

/// Facility that reserves and opens one exact routed domain declaration.
#[async_trait]
pub trait DomainFacility: fmt::Debug + Send + Sync + 'static {
    /// Concurrent identical opens share authority and initialization.
    async fn open(&self, spec: DomainSpec, schema: Schema) -> Result<Domain, StorageError>;
}

/// Nominal Local contract for the domain facility.
#[derive(Debug)]
pub struct DomainFacilityContract;
impl LocalContract for DomainFacilityContract {
    const KEY: &'static str = "rsi.storage.domain";
    type Service = dyn DomainFacility;
}
