use crate::{
    Change, ChangeEvent, ChangeOperation, DomainSpec, Record, Schema, Snapshot, StorageError,
};
use rsi_storage::{Address, KvBackend, KvUnit, Mutation, RecordObjectSize, UnitDescriptor};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Notify, OnceCell, OwnedSemaphorePermit, Semaphore, broadcast, mpsc, oneshot};

type Result<T> = std::result::Result<T, StorageError>;
const EVENT_CAPACITY: usize = 256;
const WRITE_CAPACITY: usize = 2;

pub(crate) type Transform = Box<dyn FnOnce(&Record) -> Result<Record> + Send>;

pub(crate) enum Operation {
    Changes(Vec<Change>),
    Update(Address, Transform),
}

impl fmt::Debug for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Changes(changes) => f.debug_tuple("Changes").field(changes).finish(),
            Self::Update(address, _) => f
                .debug_tuple("Update")
                .field(address)
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Acknowledgement {
    pub(crate) changed: usize,
    pub(crate) value: Option<Record>,
}

#[derive(Debug)]
struct Job {
    owner: Arc<Authority>,
    operation: Operation,
    answer: oneshot::Sender<Result<Acknowledgement>>,
    admission: OwnedSemaphorePermit,
}

#[derive(Debug, Default)]
pub(crate) struct Registry {
    pub(crate) domains: Mutex<BTreeMap<String, Weak<Authority>>>,
    pub(crate) closed: AtomicBool,
}

#[derive(Debug)]
pub(crate) struct Authority {
    pub(crate) spec: DomainSpec,
    pub(crate) schema: Schema,
    pub(crate) descriptor: UnitDescriptor,
    pub(crate) loaded: OnceCell<Loaded>,
    pub(crate) registry: Weak<Registry>,
    recovery_required: AtomicBool,
}

impl Authority {
    pub(crate) fn new(
        spec: DomainSpec,
        schema: Schema,
        descriptor: UnitDescriptor,
        registry: Weak<Registry>,
    ) -> Self {
        Self {
            spec,
            schema,
            descriptor,
            loaded: OnceCell::new(),
            registry,
            recovery_required: AtomicBool::new(false),
        }
    }
    pub(crate) fn loaded(&self) -> &Loaded {
        self.loaded.get().expect("initialized domain")
    }
    pub(crate) fn ensure_available(&self) -> Result<()> {
        if self.recovery_required.load(Ordering::Acquire) {
            return Err(StorageError::RecoveryRequired);
        }
        if self.loaded().finished.load(Ordering::Acquire) {
            return Err(StorageError::Closed);
        }
        self.loaded().backend.ensure_available()
    }
    pub(crate) fn snapshot(&self) -> Result<Snapshot> {
        self.ensure_available()?;
        let state = self
            .loaded()
            .published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.ensure_available()?;
        Ok(state.snapshot.clone())
    }
    pub(crate) fn ensure_writable(&self) -> Result<()> {
        self.ensure_available()?;
        if self
            .loaded()
            .sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            Err(StorageError::Closed)
        } else {
            Ok(())
        }
    }
    pub(crate) fn snapshot_and_subscribe(
        &self,
    ) -> Result<(Snapshot, broadcast::Receiver<ChangeEvent>)> {
        self.ensure_available()?;
        let state = self
            .loaded()
            .published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.ensure_available()?;
        Ok((
            state.snapshot.clone(),
            state
                .events
                .as_ref()
                .expect("open domain event stream")
                .subscribe(),
        ))
    }
    pub(crate) async fn admit(&self) -> Result<OwnedSemaphorePermit> {
        self.ensure_writable()?;
        let permit = Arc::clone(&self.loaded().admission)
            .acquire_owned()
            .await
            .map_err(|_| StorageError::Closed)?;
        self.ensure_writable()?;
        Ok(permit)
    }
    pub(crate) async fn submit(
        self: &Arc<Self>,
        operation: Operation,
        admission: OwnedSemaphorePermit,
    ) -> Result<Acknowledgement> {
        self.ensure_available()?;
        let (answer, receive) = oneshot::channel();
        {
            let sender = self
                .loaded()
                .sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let sender = sender.as_ref().ok_or(StorageError::Closed)?;
            self.ensure_available()?;
            sender
                .send(Job {
                    owner: Arc::clone(self),
                    operation,
                    answer,
                    admission,
                })
                .map_err(|_| StorageError::RecoveryRequired)?;
        }
        receive
            .await
            .map_err(|_| StorageError::OutcomeUnknown("domain worker did not complete".into()))?
    }
    pub(crate) fn begin_close(&self) {
        self.loaded().admission.close();
        self.loaded()
            .sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if self.loaded().finished.load(Ordering::Acquire) {
            self.release_name();
        }
    }
    pub(crate) async fn drained(&self) {
        loop {
            let notified = self.loaded().completed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.loaded().finished.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
    fn release_name(&self) {
        if let Some(registry) = self.registry.upgrade() {
            let mut domains = registry
                .domains
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if domains
                .get(&self.spec.id)
                .is_some_and(|entry| std::ptr::eq(entry.as_ptr(), self))
            {
                domains.remove(&self.spec.id);
            }
        }
    }
}

impl Drop for Authority {
    fn drop(&mut self) {
        self.release_name();
    }
}

#[derive(Debug)]
pub(crate) struct Loaded {
    pub(crate) backend: Arc<dyn KvBackend>,
    unit: Mutex<Option<Arc<dyn KvUnit>>>,
    pub(crate) recoveries: Vec<rsi_storage::Recovery>,
    runtime: tokio::runtime::Handle,
    published: Mutex<Published>,
    sender: Mutex<Option<mpsc::UnboundedSender<Job>>>,
    receiver: Mutex<Option<mpsc::UnboundedReceiver<Job>>>,
    finished: AtomicBool,
    completed: Notify,
    admission: Arc<Semaphore>,
}

#[derive(Debug)]
struct Published {
    snapshot: Snapshot,
    size: RecordObjectSize,
    events: Option<broadcast::Sender<ChangeEvent>>,
}

pub(crate) fn loaded(
    backend: Arc<dyn KvBackend>,
    unit: Arc<dyn KvUnit>,
    records: BTreeMap<Address, Record>,
    size: RecordObjectSize,
    recoveries: Vec<rsi_storage::Recovery>,
    tables: impl IntoIterator<Item = String>,
) -> Loaded {
    // Every queued or executing job owns one admission permit. The channel
    // never retains more than WRITE_CAPACITY jobs, including cancelled callers.
    let (sender, receiver) = mpsc::unbounded_channel();
    let (events, _) = broadcast::channel(EVENT_CAPACITY);
    Loaded {
        backend,
        unit: Mutex::new(Some(unit)),
        recoveries,
        runtime: tokio::runtime::Handle::current(),
        published: Mutex::new(Published {
            snapshot: Snapshot::from_records(records, tables),
            size,
            events: Some(events),
        }),
        sender: Mutex::new(Some(sender)),
        receiver: Mutex::new(Some(receiver)),
        finished: AtomicBool::new(false),
        completed: Notify::new(),
        admission: Arc::new(Semaphore::new(WRITE_CAPACITY)),
    }
}

pub(crate) fn start(owner: &Arc<Authority>) {
    let receiver = owner
        .loaded()
        .receiver
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(receiver) = receiver {
        // Guard exists before spawn, including when an unpolled worker is dropped.
        let lifetime = WorkerLifetime {
            owner: Arc::downgrade(owner),
            normal: false,
        };
        owner.loaded().runtime.spawn(run(receiver, lifetime));
    }
}

struct WorkerLifetime {
    owner: Weak<Authority>,
    normal: bool,
}
impl Drop for WorkerLifetime {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            if !self.normal {
                owner.recovery_required.store(true, Ordering::Release);
            }
            owner.begin_close();
            let unit = owner
                .loaded()
                .unit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            drop(unit);
            if self.normal {
                owner.release_name();
            }
            {
                let mut state = owner
                    .loaded()
                    .published
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                drop(state.events.take());
                owner.loaded().finished.store(true, Ordering::Release);
            }
            owner.loaded().completed.notify_waiters();
        }
    }
}

async fn run(mut receiver: mpsc::UnboundedReceiver<Job>, mut lifetime: WorkerLifetime) {
    while let Some(job) = receiver.recv().await {
        let Job {
            owner,
            operation,
            answer,
            admission,
        } = job;
        let result = commit(&owner, operation).await;
        if result.as_ref().is_err_and(StorageError::requires_recovery) {
            owner.recovery_required.store(true, Ordering::Release);
        }
        let _ = answer.send(result);
        drop(admission);
        // No strong authority survives the idle receive. Accepted jobs themselves
        // keep authority alive; dropping the final handle closes an idle channel.
    }
    lifetime.normal = true;
}

async fn commit(owner: &Authority, operation: Operation) -> Result<Acknowledgement> {
    owner.ensure_available()?;
    let (changes, value) = match operation {
        Operation::Changes(changes) => (changes, None),
        Operation::Update(address, transform) => {
            let current = {
                let state = owner
                    .loaded()
                    .published
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.snapshot.record(&address).cloned().ok_or_else(|| {
                    StorageError::InvalidInput(format!(
                        "missing record {}",
                        address.accounting_key()
                    ))
                })?
            };
            // This worker owns the sole write queue; no write can interleave
            // while the pure synchronous transform runs without any lock.
            let value = transform(&current)?;
            (
                vec![Change {
                    address,
                    value: Some(value.clone()),
                }],
                Some(value),
            )
        }
    };
    let mut changes = changes;
    let (projected, revision) = admit_changes(owner, &mut changes)?;
    if changes.is_empty() {
        return Ok(Acknowledgement { changed: 0, value });
    }
    let revision = revision
        .checked_add(1)
        .ok_or_else(|| StorageError::InvalidInput("domain event revision exhausted".into()))?;
    let durable = changes
        .iter()
        .map(|change| Mutation {
            address: change.address.clone(),
            value: change.value.as_ref().map(|record| record.encoded.clone()),
        })
        .collect();
    let unit = owner
        .loaded()
        .unit
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .cloned()
        .expect("running worker owns its unit");
    unit.write(durable).await?;
    let count = changes.len();
    let mut state = owner
        .loaded()
        .published
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut retired = Vec::with_capacity(count);
    for change in &changes {
        let previous = if let Some(value) = &change.value {
            state.snapshot.put(&change.address, value.clone())
        } else {
            state.snapshot.remove(&change.address)
        };
        retired.extend(previous);
    }
    state.snapshot.revision = revision;
    state.size = projected;
    for change in changes {
        let event = ChangeEvent {
            revision,
            address: change.address,
            operation: if change.value.is_some() {
                ChangeOperation::Put
            } else {
                ChangeOperation::Delete
            },
        };
        let _ = state
            .events
            .as_ref()
            .expect("running worker event stream")
            .send(event);
    }
    drop(state);
    // A business type may implement Drop; destroy replaced values without a
    // publication lock so its destructor cannot deadlock a reentrant reader.
    drop(retired);
    Ok(Acknowledgement {
        changed: count,
        value,
    })
}

fn admit_changes(owner: &Authority, changes: &mut Vec<Change>) -> Result<(RecordObjectSize, u64)> {
    let mut unique = BTreeSet::new();
    let (snapshot, mut projected) = {
        let state = owner
            .loaded()
            .published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.snapshot.clone(), state.size)
    };
    {
        for change in changes.iter() {
            let codec = owner.schema.codec(&change.address)?;
            if !unique.insert(&change.address) {
                return Err(StorageError::InvalidInput("duplicate batch address".into()));
            }
            let previous = snapshot
                .record(&change.address)
                .map(|record| change.address.entry_bytes(record.encoded.encoded_len()))
                .transpose()?;
            if let Some(record) = &change.value {
                codec.check_type(record.type_id())?;
                projected = projected.with_entry(
                    previous,
                    change.address.entry_bytes(record.encoded.encoded_len())?,
                )?;
            } else {
                if change.address.is_global() {
                    return Err(StorageError::InvalidInput(
                        "global slot cannot be deleted".into(),
                    ));
                }
                if let Some(previous) = previous {
                    projected = projected.without_entry(previous)?;
                }
            }
        }
        // Missing-key deletes perform no backend write, stamp or event.
        changes
            .retain(|change| change.value.is_some() || snapshot.record(&change.address).is_some());
    }
    if projected.records() > owner.spec.maximum_records
        || projected.bytes() > owner.spec.maximum_bytes
    {
        return Err(StorageError::InvalidInput(
            "domain mutation exceeds its record or byte bound".into(),
        ));
    }
    Ok((projected, snapshot.revision))
}
