use crate::{
    Address, Capabilities, Layout, Mutation, Result, StorageError, UnitDescriptor, UnitSnapshot,
};
use async_trait::async_trait;
use std::{collections::BTreeSet, fmt, path::PathBuf, sync::Arc};

/// An owned backend unit and its initial bounded durable snapshot.
#[derive(Debug)]
pub struct OpenedUnit {
    /// Holds medium state and exclusive unit-name ownership through actual I/O.
    pub unit: Arc<dyn KvUnit>,
    /// Decoded JSON framing; owning business codecs decode typed values once.
    pub snapshot: UnitSnapshot,
}

/// Generation-owned backend factory registered under one exact route.
#[async_trait]
pub trait KvBackend: fmt::Debug + Send + Sync + 'static {
    /// Local health check, without I/O.
    fn ensure_available(&self) -> Result<()>;
    /// Capabilities for a requested unit layout, before opening any medium.
    fn capabilities(&self, layout: Layout) -> Capabilities;
    /// Reserves a unit name and loads its bounded initial state.
    /// A cancelled opener retains its lease through actual background I/O.
    async fn open(&self, descriptor: UnitDescriptor) -> Result<OpenedUnit>;
}

/// One owned backend unit. Its final I/O lease releases the unit name and cache.
#[async_trait]
pub trait KvUnit: fmt::Debug + Send + Sync + 'static {
    /// Immutable validated declaration retained by this unit.
    fn descriptor(&self) -> &UnitDescriptor;
    /// Checks the retained backend generation without I/O.
    fn ensure_available(&self) -> Result<()>;
    /// Publishes replacements/deletions according to the backend capability.
    /// Values are already bounded compact JSON; do not decode or re-encode them.
    /// Atomic row/document backends commit the whole batch. Per-record JSON
    /// permits a single replacement or rollback-capable delete-many only.
    async fn write(&self, changes: Vec<Mutation>) -> Result<()>;
    /// Preserves an individual source before the domain skips a failed codec.
    async fn backup_record(&self, _address: &Address) -> Result<PathBuf> {
        Err(StorageError::Unsupported(
            "backend cannot preserve an individual record".into(),
        ))
    }
}

/// Admits a raw batch's addresses against its unit declaration, without decoding
/// trusted encoded values or repeating address-grammar validation.
pub fn admit_mutations(
    unit: &UnitDescriptor,
    changes: &[Mutation],
    capabilities: Capabilities,
) -> Result<()> {
    if changes.len() > 2 * crate::MAXIMUM_STORAGE_RECORDS {
        return Err(StorageError::InvalidInput("batch capacity exceeded".into()));
    }
    let mut seen = BTreeSet::new();
    for change in changes {
        unit.admit_address(&change.address)?;
        if !seen.insert(&change.address) {
            return Err(StorageError::InvalidInput("duplicate batch address".into()));
        }
        if change.address.is_global() && change.value.is_none() {
            return Err(StorageError::InvalidInput(
                "global slot cannot be deleted".into(),
            ));
        }
    }
    if changes.len() > 1
        && changes.iter().any(|change| change.value.is_some())
        && !capabilities.atomic_batch
    {
        return Err(StorageError::Unsupported(
            "backend cannot atomically publish arbitrary batches".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MAXIMUM_STORAGE_RECORDS, RecordPolicy};
    use std::collections::BTreeMap;

    #[test]
    fn batch_capacity_bounds_missing_deletions_before_backend_work() {
        let unit = UnitDescriptor::new(
            "bounded".into(),
            1,
            BTreeSet::new(),
            BTreeMap::from([("rows".into(), RecordPolicy::Reject)]),
            false,
            Layout::Single,
        )
        .unwrap();
        let changes: Vec<_> = (0..=2 * MAXIMUM_STORAGE_RECORDS)
            .map(|key| Mutation {
                address: Address::record("rows", key.to_string()).unwrap(),
                value: None,
            })
            .collect();
        assert!(matches!(
            admit_mutations(&unit, &changes, Capabilities::default()),
            Err(StorageError::InvalidInput(message)) if message.contains("batch capacity")
        ));
        admit_mutations(
            &unit,
            &changes[..changes.len() - 1],
            Capabilities::default(),
        )
        .unwrap();
    }
}
