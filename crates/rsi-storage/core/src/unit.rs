use crate::{EncodedValue, Result, StorageError, encoded_entry_bytes, validate_identifier};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Requested document layout. Row stores retain unit-wide versioning for either.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    /// One document and an atomic publication boundary for the complete unit.
    #[default]
    Single,
    /// Independently versioned records, without arbitrary batch atomicity.
    PerRecord,
}

/// Whether a table owns authoritative state or explicitly disposable records.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecordPolicy {
    /// Reject the complete open on any malformed record.
    #[default]
    Reject,
    /// Preserve the original individual source durably, then omit that record.
    BackupAndSkip,
}

/// Immutable, validated backend-facing declaration of one business unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnitDescriptor {
    name: String,
    version: u32,
    compatible_versions: BTreeSet<u32>,
    tables: BTreeMap<String, RecordPolicy>,
    has_global: bool,
    layout: Layout,
}

impl UnitDescriptor {
    /// Admits a declaration once; backends trust this typed descriptor in-process.
    pub fn new(
        name: String,
        version: u32,
        compatible_versions: BTreeSet<u32>,
        tables: BTreeMap<String, RecordPolicy>,
        has_global: bool,
        layout: Layout,
    ) -> Result<Self> {
        validate_identifier("domain", &name)?;
        if version == 0
            || compatible_versions
                .iter()
                .any(|old| *old == 0 || *old >= version)
        {
            return Err(StorageError::InvalidInput(
                "versions must be positive; compatible versions must precede current".into(),
            ));
        }
        for table in tables.keys() {
            validate_identifier("table", table)?;
        }
        if tables.is_empty() && !has_global {
            return Err(StorageError::InvalidInput(
                "domain must declare a table or global slot".into(),
            ));
        }
        Ok(Self {
            name,
            version,
            compatible_versions,
            tables,
            has_global,
            layout,
        })
    }
    /// Exact domain identity.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Version written by new mutations.
    pub const fn version(&self) -> u32 {
        self.version
    }
    /// Declared table identities.
    pub fn tables(&self) -> &BTreeMap<String, RecordPolicy> {
        &self.tables
    }
    /// Whether the declaration includes a global slot.
    pub const fn has_global(&self) -> bool {
        self.has_global
    }
    /// Requested JSON layout.
    pub const fn layout(&self) -> Layout {
        self.layout
    }
    /// Checks a stored version without modifying it.
    pub fn admit_version(&self, stored: u32) -> Result<()> {
        if stored == self.version || self.compatible_versions.contains(&stored) {
            Ok(())
        } else {
            Err(StorageError::DomainVersionMismatch {
                domain: self.name.clone(),
                stored,
                requested: self.version,
            })
        }
    }
    /// Rejects an address outside the declared layout.
    pub fn admit_address(&self, address: &Address) -> Result<()> {
        if (address.is_global() && self.has_global) || self.tables.contains_key(address.table()) {
            Ok(())
        } else {
            Err(StorageError::InvalidInput(
                "address is not declared by this domain".into(),
            ))
        }
    }
}

/// A global slot or one table record; keys never participate in native paths.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "AddressWire")]
pub struct Address {
    table: String,
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddressWire {
    table: String,
    key: String,
}

impl TryFrom<AddressWire> for Address {
    type Error = StorageError;
    fn try_from(wire: AddressWire) -> Result<Self> {
        Self::from_parts(wire.table, wire.key)
    }
}

impl Address {
    /// An explicitly present global, including a stored JSON null.
    pub fn global() -> Self {
        Self {
            table: String::new(),
            key: String::new(),
        }
    }
    /// Admits a table record address, allowing arbitrary bounded UTF-8 keys.
    pub fn record(table: impl Into<String>, key: impl Into<String>) -> Result<Self> {
        let table = table.into();
        validate_identifier("table", &table)?;
        let key = key.into();
        validate_key(&key)?;
        Ok(Self { table, key })
    }
    /// Admits separate durable columns; only two empty columns name the global.
    pub fn from_parts(table: String, key: String) -> Result<Self> {
        if table.is_empty() && key.is_empty() {
            Ok(Self::global())
        } else {
            Self::record(table, key)
        }
    }
    /// Empty only for the global slot.
    pub fn table(&self) -> &str {
        &self.table
    }
    /// The business key; an empty table key remains distinct from the global.
    pub fn key(&self) -> &str {
        &self.key
    }
    /// Reuses an admitted table identity while admitting a different business key.
    pub fn with_key(&self, key: impl Into<String>) -> Result<Self> {
        if self.is_global() {
            return Err(StorageError::InvalidInput(
                "global has no table keys".into(),
            ));
        }
        let key = key.into();
        validate_key(&key)?;
        Ok(Self {
            table: self.table.clone(),
            key,
        })
    }
    /// Whether this is the global slot.
    pub fn is_global(&self) -> bool {
        self.table.is_empty()
    }
    /// Injective logical accounting key: empty for global, otherwise table/key.
    pub fn accounting_key(&self) -> String {
        if self.is_global() {
            String::new()
        } else {
            format!("{}/{}", self.table, self.key)
        }
    }
    /// Exact size in the logical record object defined by the core contract.
    pub fn entry_bytes(&self, value_bytes: usize) -> Result<usize> {
        encoded_entry_bytes(&self.accounting_key(), value_bytes)
    }
}

/// Admits arbitrary business keys at their public/durable boundary.
pub fn validate_key(key: &str) -> Result<()> {
    if key.len() > crate::MAXIMUM_STORAGE_IDENTIFIER_BYTES {
        Err(StorageError::InvalidInput(
            "record key exceeds 256 UTF-8 bytes".into(),
        ))
    } else {
        Ok(())
    }
}

/// One replacement or deletion, already encoded by the typed producer.
#[derive(Clone, Debug)]
pub struct Mutation {
    /// Exact slot to replace or remove.
    pub address: Address,
    /// None deletes; Some(JSON null) stores a present null value.
    pub value: Option<EncodedValue>,
}

/// Complete bounded snapshot of one backend unit.
#[derive(Clone, Debug, Default)]
pub struct UnitSnapshot {
    /// No durable version before the first mutation.
    pub version: Option<u32>,
    /// Present records, including the global when it has been written.
    pub records: BTreeMap<Address, EncodedValue>,
    /// Individual disposable sources preserved during durable admission.
    pub recoveries: Vec<Recovery>,
}

/// Value-free report of a disposable record preserved before it was skipped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recovery {
    /// Declared disposable table.
    pub table: String,
    /// Business key, when the source envelope could be decoded safely.
    pub key: Option<String>,
    /// Private retained copy of the original source bytes.
    pub backup: std::path::PathBuf,
    /// Boundary that rejected the record; private payload text is never included.
    pub cause: RecoveryCause,
}

/// Admission stage of a preserved malformed record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryCause {
    /// Invalid per-record envelope or identity.
    Envelope,
    /// Invalid JSON value framing, depth or size.
    Value,
    /// Value rejected by its declared business codec.
    Codec,
}

/// Backend guarantees for a requested unit layout.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Capabilities {
    /// Arbitrary multi-address replacements/deletions commit as one transaction.
    pub atomic_batch: bool,
    /// Corrupt records can be durably preserved independently before skipping.
    pub backup_record: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn addresses_preserve_arbitrary_keys_and_global_identity() {
        let global = Address::global();
        for key in ["", "../a", "a/b", "中 🦀", "\0", "__proto__"] {
            let record = Address::record("table", key).unwrap();
            assert_ne!(record, global);
            assert_eq!(
                serde_json::from_slice::<Address>(&serde_json::to_vec(&record).unwrap()).unwrap(),
                record
            );
        }
        assert!(Address::record("../table", "key").is_err());
        assert!(Address::record("table", "中".repeat(86)).is_err());
        assert!(Address::from_parts(String::new(), "key".into()).is_err());
    }
    #[test]
    fn compatible_versions_are_explicit_and_older() {
        let descriptor = UnitDescriptor::new(
            "unit".into(),
            3,
            BTreeSet::from([1]),
            BTreeMap::from([("rows".into(), RecordPolicy::Reject)]),
            true,
            Layout::Single,
        )
        .unwrap();
        assert!(descriptor.admit_version(1).is_ok());
        assert!(descriptor.admit_version(3).is_ok());
        assert!(matches!(
            descriptor.admit_version(2),
            Err(StorageError::DomainVersionMismatch { .. })
        ));
        assert!(descriptor.admit_address(&Address::global()).is_ok());
        assert!(
            descriptor
                .admit_address(&Address::record("absent", "").unwrap())
                .is_err()
        );
    }
}
