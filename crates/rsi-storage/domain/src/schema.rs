use rsi_storage::{Address, EncodedValue, RecordPolicy, StorageError};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    any::{Any, TypeId, type_name},
    collections::BTreeMap,
    fmt,
    sync::Arc,
};

type Result<T> = std::result::Result<T, StorageError>;

/// A trusted typed value and its single bounded encoding, both shared on clone.
#[derive(Clone)]
pub struct Record {
    pub(crate) value: Arc<dyn Any + Send + Sync>,
    pub(crate) encoded: EncodedValue,
}

impl fmt::Debug for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Record")
            .field("encoded", &self.encoded)
            .finish_non_exhaustive()
    }
}

impl Record {
    /// Encodes a trusted typed value once; its decoder is not invoked.
    pub fn new<T: Serialize + Send + Sync + 'static>(value: T) -> Result<Self> {
        Ok(Self {
            encoded: EncodedValue::encode(&value)?,
            value: Arc::new(value),
        })
    }
    /// Shares the immutable typed value after checking its declaration's type.
    pub fn get<T: Send + Sync + 'static>(&self) -> Result<Arc<T>> {
        Arc::clone(&self.value).downcast().map_err(|_| {
            StorageError::InvalidInput(format!("record codec is not {}", type_name::<T>()))
        })
    }
    pub(crate) fn type_id(&self) -> TypeId {
        self.value.as_ref().type_id()
    }
}

#[derive(Clone)]
pub(crate) struct Codec {
    type_id: TypeId,
    name: &'static str,
    decode: fn(EncodedValue) -> Result<Record>,
    pub(crate) policy: RecordPolicy,
}

impl fmt::Debug for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Codec")
            .field("type", &self.name)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl PartialEq for Codec {
    fn eq(&self, other: &Self) -> bool {
        self.type_id == other.type_id && self.policy == other.policy
    }
}
impl Eq for Codec {}

impl Codec {
    fn of<T: DeserializeOwned + Serialize + Send + Sync + 'static>(policy: RecordPolicy) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            name: type_name::<T>(),
            policy,
            decode: |encoded| {
                let value = encoded.decode::<T>()?;
                Ok(Record {
                    value: Arc::new(value),
                    encoded,
                })
            },
        }
    }
    pub(crate) fn decode(&self, encoded: EncodedValue) -> Result<Record> {
        (self.decode)(encoded)
    }
    pub(crate) fn check_type(&self, type_id: TypeId) -> Result<()> {
        if type_id == self.type_id {
            Ok(())
        } else {
            Err(StorageError::InvalidInput(format!(
                "record must use declared codec {}",
                self.name
            )))
        }
    }
}

/// Runtime declaration of typed codecs. Each Rust type owns its Deserialize validation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Schema {
    pub(crate) tables: BTreeMap<String, Codec>,
    pub(crate) global: Option<Codec>,
}

impl Schema {
    /// Adds one authoritative table. Duplicate declarations reject at definition.
    pub fn table<T: DeserializeOwned + Serialize + Send + Sync + 'static>(
        self,
        name: &str,
    ) -> Result<Self> {
        self.table_with_policy::<T>(name, RecordPolicy::Reject)
    }
    /// Adds a table with an explicit corruption policy; backend capability is checked at open.
    pub fn table_with_policy<T: DeserializeOwned + Serialize + Send + Sync + 'static>(
        mut self,
        name: &str,
        policy: RecordPolicy,
    ) -> Result<Self> {
        rsi_storage::validate_identifier("table", name)?;
        if self
            .tables
            .insert(name.to_owned(), Codec::of::<T>(policy))
            .is_some()
        {
            return Err(StorageError::InvalidInput(format!(
                "duplicate table `{name}`"
            )));
        }
        Ok(self)
    }
    /// Adds the global slot. Stored null and an unwritten slot remain distinct.
    pub fn global<T: DeserializeOwned + Serialize + Send + Sync + 'static>(
        mut self,
    ) -> Result<Self> {
        if self
            .global
            .replace(Codec::of::<T>(RecordPolicy::Reject))
            .is_some()
        {
            return Err(StorageError::InvalidInput(
                "duplicate global declaration".into(),
            ));
        }
        Ok(self)
    }
    pub(crate) fn codec(&self, address: &Address) -> Result<&Codec> {
        if address.is_global() {
            self.global.as_ref()
        } else {
            self.tables.get(address.table())
        }
        .ok_or_else(|| StorageError::InvalidInput("undeclared record address".into()))
    }
}

/// Immutable shared committed state; later publications cannot change its records.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub(crate) revision: u64,
    pub(crate) tables: Arc<BTreeMap<String, Arc<BTreeMap<String, Record>>>>,
    pub(crate) global: Option<Record>,
}

impl Snapshot {
    pub(crate) fn from_records(
        records: BTreeMap<Address, Record>,
        tables: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut snapshot = Self {
            tables: Arc::new(
                tables
                    .into_iter()
                    .map(|name| (name, Arc::default()))
                    .collect(),
            ),
            ..Self::default()
        };
        for (address, record) in records {
            let _ = snapshot.put(&address, record);
        }
        snapshot
    }
    pub(crate) fn put(&mut self, address: &Address, value: Record) -> Option<Record> {
        if address.is_global() {
            self.global.replace(value)
        } else {
            let tables = Arc::make_mut(&mut self.tables);
            let records = Arc::make_mut(tables.get_mut(address.table()).expect("declared table"));
            if let Some(current) = records.get_mut(address.key()) {
                Some(std::mem::replace(current, value))
            } else {
                records.insert(address.key().to_owned(), value)
            }
        }
    }
    pub(crate) fn remove(&mut self, address: &Address) -> Option<Record> {
        Arc::make_mut(
            Arc::make_mut(&mut self.tables)
                .get_mut(address.table())
                .expect("declared table"),
        )
        .remove(address.key())
    }
    /// Handle-local commit revision; unrelated to a business global revision.
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    /// Number of present records, including a written global.
    pub fn len(&self) -> usize {
        self.tables.values().map(|table| table.len()).sum::<usize>()
            + usize::from(self.global.is_some())
    }
    /// Whether no record or global has been written.
    pub fn is_empty(&self) -> bool {
        self.global.is_none() && self.tables.values().all(|table| table.is_empty())
    }
    /// Borrows the encoded/typed record without copying its value.
    pub fn record(&self, address: &Address) -> Option<&Record> {
        if address.is_global() {
            self.global.as_ref()
        } else {
            self.tables
                .get(address.table())
                .and_then(|table| table.get(address.key()))
        }
    }
    /// Shares the value at an address; absent globals do not synthesize a write.
    pub fn get<T: Send + Sync + 'static>(&self, address: &Address) -> Result<Option<Arc<T>>> {
        self.record(address).map(Record::get::<T>).transpose()
    }
}

/// One value-free notification, emitted only after durable publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangeEvent {
    /// Handle-local commit revision shared by all changes in a batch.
    pub revision: u64,
    /// Address affected by this commit.
    pub address: Address,
    /// Whether the slot was replaced or removed.
    pub operation: ChangeOperation,
}

/// Committed change classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeOperation {
    /// A value was inserted or replaced.
    Put,
    /// A present value was removed.
    Delete,
}

/// An atomic typed replacement or deletion. Missing deletions are ignored at the queue slot.
#[derive(Clone, Debug)]
pub struct Change {
    /// Slot to change.
    pub address: Address,
    /// None removes a table record; a global can only be set.
    pub value: Option<Record>,
}

impl Change {
    /// Constructs a replacement while encoding the trusted value once.
    pub fn put<T: Serialize + Send + Sync + 'static>(address: Address, value: T) -> Result<Self> {
        Ok(Self {
            address,
            value: Some(Record::new(value)?),
        })
    }
    /// Constructs a deletion; globals are rejected by domain admission.
    pub fn delete(address: Address) -> Self {
        Self {
            address,
            value: None,
        }
    }
}
