use crate::{Change, Domain, Record, StorageError, queue::Operation};
use rsi_storage::Address;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    marker::PhantomData,
    sync::Arc,
};

type Result<T> = std::result::Result<T, StorageError>;

/// Handle to one declared typed table. Records are immutable shared values.
#[derive(Debug)]
pub struct Table<T> {
    pub(crate) domain: Domain,
    pub(crate) address: Address,
    pub(crate) marker: PhantomData<fn() -> T>,
}

impl<T> Clone for Table<T> {
    fn clone(&self) -> Self {
        Self {
            domain: self.domain.clone(),
            address: self.address.clone(),
            marker: PhantomData,
        }
    }
}

impl<T: Serialize + Send + Sync + 'static> Table<T> {
    /// Reads the current committed value without copying or performing I/O.
    pub fn get(&self, key: &str) -> Result<Option<Arc<T>>> {
        self.domain.snapshot()?.get(&self.address.with_key(key)?)
    }
    /// Takes an immutable stable snapshot of this table.
    ///
    /// # Panics
    /// Panics if the internal declaration-to-snapshot invariant is broken.
    pub fn snapshot(&self) -> Result<TableSnapshot<T>> {
        let snapshot = self.domain.snapshot()?;
        Ok(TableSnapshot {
            records: snapshot
                .tables
                .get(self.address.table())
                .cloned()
                .expect("declared table"),
            marker: PhantomData,
        })
    }
    /// Encodes and replaces a record through the domain's single queue.
    pub async fn put(&self, key: &str, value: T) -> Result<()> {
        let admission = self.domain.authority.admit().await?;
        self.domain
            .authority
            .submit(
                Operation::Changes(vec![Change::put(self.address.with_key(key)?, value)?]),
                admission,
            )
            .await?;
        Ok(())
    }
    /// Removes an existing record; a missing key causes no write or event.
    pub async fn delete(&self, key: &str) -> Result<bool> {
        Ok(self.delete_many([key]).await? != 0)
    }
    /// Removes distinct present keys in one queue slot.
    pub async fn delete_many<'a>(&self, keys: impl IntoIterator<Item = &'a str>) -> Result<usize> {
        let admission = self.domain.authority.admit().await?;
        let mut distinct = BTreeSet::new();
        for key in keys {
            distinct.insert(self.address.with_key(key)?);
            self.domain.spec().admit_batch_size(distinct.len())?;
        }
        Ok(self
            .domain
            .authority
            .submit(
                Operation::Changes(distinct.into_iter().map(Change::delete).collect()),
                admission,
            )
            .await?
            .changed)
    }
    /// Transforms the value current at this operation's queue slot.
    /// The callback is synchronous and pure; missing records reject.
    ///
    /// # Panics
    /// Panics if an acknowledged update lacks its committed record.
    pub async fn update(
        &self,
        key: &str,
        transform: impl FnOnce(&T) -> Result<T> + Send + 'static,
    ) -> Result<Arc<T>> {
        let admission = self.domain.authority.admit().await?;
        let operation = Operation::Update(
            self.address.with_key(key)?,
            Box::new(move |current: &Record| {
                let current = current.get::<T>()?;
                Record::new(transform(current.as_ref())?)
            }),
        );
        self.domain
            .authority
            .submit(operation, admission)
            .await?
            .value
            .expect("update returns its committed record")
            .get()
    }
    /// Creates a typed change for a multi-table atomic batch.
    pub fn put_change(&self, key: &str, value: T) -> Result<Change> {
        Change::put(self.address.with_key(key)?, value)
    }
    /// Creates a deletion for a multi-table atomic batch.
    pub fn delete_change(&self, key: &str) -> Result<Change> {
        Ok(Change::delete(self.address.with_key(key)?))
    }
}

/// Shared immutable table view; iteration remains stable across later commits.
#[derive(Debug)]
pub struct TableSnapshot<T> {
    records: Arc<BTreeMap<String, Record>>,
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for TableSnapshot<T> {
    fn clone(&self) -> Self {
        Self {
            records: Arc::clone(&self.records),
            marker: PhantomData,
        }
    }
}

impl<T: Send + Sync + 'static> TableSnapshot<T> {
    /// Iterates the stable table without cloning the record map or its values.
    ///
    /// # Panics
    /// Panics if an internal record does not match its declared table codec.
    pub fn entries(&self) -> impl Iterator<Item = (&str, Arc<T>)> {
        self.records
            .iter()
            .map(|(key, record)| (key.as_str(), record.get().expect("declared table codec")))
    }
    /// Stable key iteration without constructing typed value handles.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.records.keys().map(String::as_str)
    }
    /// Number of records in this table snapshot.
    pub fn len(&self) -> usize {
        self.records.len()
    }
    /// Whether the table contains no records.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// Handle to the declared typed global. Absence remains distinct from a stored null.
#[derive(Debug)]
pub struct Global<T> {
    pub(crate) domain: Domain,
    pub(crate) marker: PhantomData<fn() -> T>,
}

impl<T> Clone for Global<T> {
    fn clone(&self) -> Self {
        Self {
            domain: self.domain.clone(),
            marker: PhantomData,
        }
    }
}

impl<T: Serialize + Send + Sync + 'static> Global<T> {
    /// Reads the written global or None before the first set.
    pub fn get(&self) -> Result<Option<Arc<T>>> {
        self.domain.snapshot()?.get(&Address::global())
    }
    /// Replaces the global through the same queue as table writes and returns
    /// the shared committed value for publishing an associated consumer cache.
    pub async fn set(&self, value: T) -> Result<Arc<T>> {
        let admission = self.domain.authority.admit().await?;
        let record = Record::new(value)?;
        let shared = record.get()?;
        self.domain
            .authority
            .submit(
                Operation::Changes(vec![Change {
                    address: Address::global(),
                    value: Some(record),
                }]),
                admission,
            )
            .await?;
        Ok(shared)
    }
    /// Creates a global replacement for an atomic batch with table changes.
    pub fn change(&self, value: T) -> Result<Change> {
        Change::put(Address::global(), value)
    }
}
