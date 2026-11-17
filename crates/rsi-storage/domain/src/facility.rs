use crate::{
    Domain, DomainFacility, DomainSpec, Schema, StorageError,
    queue::{self, Authority, Registry},
};
use async_trait::async_trait;
use rsi_storage::{RecordObjectSize, RecordPolicy, StorageHub, UnitDescriptor};
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak, atomic::Ordering},
};

#[derive(Debug)]
pub(crate) struct Facility {
    pub(crate) hub: Arc<dyn StorageHub>,
    pub(crate) registry: Arc<Registry>,
}

#[async_trait]
impl DomainFacility for Facility {
    async fn open(&self, spec: DomainSpec, schema: Schema) -> Result<Domain, StorageError> {
        spec.validate()?;
        let descriptor = UnitDescriptor::new(
            spec.id.clone(),
            spec.version,
            spec.compatible_versions.clone(),
            schema
                .tables
                .iter()
                .map(|(name, codec)| (name.clone(), codec.policy))
                .collect(),
            schema.global.is_some(),
            spec.layout,
        )?;
        let authority = {
            let mut domains = self
                .registry
                .domains
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.registry.closed.load(Ordering::Acquire) {
                return Err(closed());
            }
            if let Some(authority) = domains.get(&spec.id).and_then(Weak::upgrade) {
                authority
            } else {
                let authority = Arc::new(Authority::new(
                    spec.clone(),
                    schema.clone(),
                    descriptor,
                    Arc::downgrade(&self.registry),
                ));
                domains.insert(spec.id.clone(), Arc::downgrade(&authority));
                authority
            }
        };
        // Strong authority drops happen after the registry mutex is released.
        if authority.spec != spec || authority.schema != schema {
            return Err(StorageError::InvalidInput(format!(
                "domain `{}` is already open with a different declaration",
                spec.id
            )));
        }
        authority
            .loaded
            .get_or_try_init(|| self.load(&authority))
            .await?;
        queue::start(&authority);
        if self.registry.closed.load(Ordering::Acquire) {
            authority.begin_close();
            return Err(closed());
        }
        authority.ensure_available()?;
        authority.ensure_writable()?;
        Ok(Domain { authority })
    }
}

impl Facility {
    async fn load(&self, authority: &Authority) -> Result<queue::Loaded, StorageError> {
        let backend = self.hub.resolve(&authority.spec.backend)?;
        let capabilities = backend.capabilities(authority.spec.layout);
        if authority
            .schema
            .tables
            .values()
            .any(|codec| codec.policy == RecordPolicy::BackupAndSkip)
            && !capabilities.backup_record
        {
            return Err(StorageError::Unsupported(
                "selected backend cannot preserve individual corrupt records".into(),
            ));
        }
        let opened = backend.open(authority.descriptor.clone()).await?;
        let stored = opened.snapshot;
        let mut size = RecordObjectSize::default();
        let mut records = BTreeMap::new();
        let mut recoveries = stored.recoveries;
        for (address, value) in stored.records {
            let projected = size
                .with_entry(None, address.entry_bytes(value.encoded_len())?)
                .map_err(corrupt)?;
            if projected.records() > authority.spec.maximum_records
                || projected.bytes() > authority.spec.maximum_bytes
            {
                return Err(corrupt("loaded domain exceeds its record or byte bound"));
            }
            let codec = authority.schema.codec(&address).map_err(corrupt)?;
            let record = match codec.decode(value) {
                Ok(record) => record,
                Err(error) if codec.policy == RecordPolicy::BackupAndSkip => {
                    let backup = opened.unit.backup_record(&address).await?;
                    recoveries.push(rsi_storage::Recovery {
                        table: address.table().to_owned(),
                        key: Some(address.key().to_owned()),
                        backup,
                        cause: rsi_storage::RecoveryCause::Codec,
                    });
                    // The retained original is the diagnostic source. Do not
                    // log a serde error that may contain the private value.
                    drop(error);
                    continue;
                }
                Err(error) => return Err(error),
            };
            size = projected;
            records.insert(address, record);
        }
        if self.registry.closed.load(Ordering::Acquire) {
            return Err(closed());
        }
        Ok(queue::loaded(
            backend,
            opened.unit,
            records,
            size,
            recoveries,
            authority.schema.tables.keys().cloned(),
        ))
    }

    pub(crate) fn begin_close(&self) -> Vec<Arc<Authority>> {
        let owners: Vec<_> = {
            let domains = self
                .registry
                .domains
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.registry.closed.store(true, Ordering::Release);
            domains.values().filter_map(Weak::upgrade).collect()
        };
        for owner in &owners {
            if owner.loaded.get().is_some() {
                queue::start(owner);
                owner.begin_close();
            }
        }
        owners
    }
}

fn closed() -> StorageError {
    StorageError::Closed
}
fn corrupt(error: impl std::fmt::Display) -> StorageError {
    StorageError::Corrupt(error.to_string())
}
