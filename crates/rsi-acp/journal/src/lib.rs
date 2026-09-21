//! Durable local observations of external ACP agents, separate from native Facts.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod filesystem;
mod store;
mod types;
pub use rsi_acp_protocol::observation::*;
use rusqlite::Connection;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
pub use types::Limits;

/// Cloneable handle retaining the exclusive journal lease through in-flight work.
#[derive(Clone)]
pub struct Journal(Arc<Owner>);
impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalJournal").finish_non_exhaustive()
    }
}
struct Owner {
    storage: Mutex<Option<Storage>>,
    closing: AtomicBool,
    closed: tokio::sync::watch::Sender<bool>,
    limits: Limits,
    workers: Arc<tokio::sync::Semaphore>,
    mutations: Arc<tokio::sync::Semaphore>,
}
struct Storage {
    connection: Connection,
    _lease: filesystem::Lease,
}
impl Journal {
    /// Opens only the dedicated external journal directory, without Agent Store access.
    pub async fn open(root: PathBuf, limits: Limits) -> Result<Self> {
        limits.validate()?;
        tokio::task::spawn_blocking(move || {
            let (path, lease) = filesystem::prepare(&root)?;
            let connection = store::open(&path, limits)?;
            Ok(Self(Arc::new(Owner {
                storage: Mutex::new(Some(Storage {
                    connection,
                    _lease: lease,
                })),
                closing: AtomicBool::new(false),
                closed: tokio::sync::watch::channel(false).0,
                limits,
                workers: Arc::new(tokio::sync::Semaphore::new(2)),
                mutations: Arc::new(tokio::sync::Semaphore::new(32)),
            })))
        })
        .await
        .map_err(|_| Error::Io)?
    }
    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Connection, Limits) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        if self.0.closing.load(Ordering::Acquire) {
            return Err(Error::Io);
        }
        let permit = self
            .0
            .workers
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        self.admitted(permit, work).await
    }
    async fn mutate<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Connection, Limits) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        if self.0.closing.load(Ordering::Acquire) {
            return Err(Error::Io);
        }
        let _queued = self
            .0
            .mutations
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let permit = self
            .0
            .workers
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::Io)?;
        if self.0.closing.load(Ordering::Acquire) {
            return Err(Error::Io);
        }
        self.admitted(permit, work).await
    }
    async fn admitted<T: Send + 'static>(
        &self,
        permit: tokio::sync::OwnedSemaphorePermit,
        work: impl FnOnce(&mut Connection, Limits) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let owner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut storage = owner.storage.lock().map_err(|_| Error::Io)?;
            work(
                &mut storage.as_mut().ok_or(Error::Io)?.connection,
                owner.limits,
            )
        })
        .await
        .map_err(|_| Error::Io)?
    }
    /// Retires admission, joins retained blocking work and releases the writer lease.
    /// Cleanup survives this waiter's cancellation; obsolete handles become unavailable.
    pub async fn close(&self) -> Result<()> {
        let mut closed = self.0.closed.subscribe();
        if !self.0.closing.swap(true, Ordering::AcqRel) {
            let owner = self.0.clone();
            tokio::spawn(async move {
                let _exclusive = owner.workers.clone().acquire_many_owned(2).await;
                owner.workers.close();
                let storage = owner
                    .storage
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                // Final SQLite close can perform filesystem work; do not block the runtime.
                let _closed = tokio::task::spawn_blocking(move || drop(storage)).await;
                owner.closed.send_replace(true);
            });
        }
        while !*closed.borrow_and_update() {
            closed.changed().await.map_err(|_| Error::Io)?;
        }
        Ok(())
    }

    /// Reserves settlement metadata before admitting any peer or observed records.
    pub async fn create(
        &self,
        id: ConversationId,
        endpoint: String,
        cwd: String,
    ) -> Result<Snapshot> {
        self.mutate(move |connection, limits| store::create(connection, limits, id, endpoint, cwd))
            .await
    }
    /// Reads one bounded, validated metadata record.
    pub async fn get(&self, id: &ConversationId) -> Result<Snapshot> {
        let id = id.clone();
        self.run(move |connection, _| store::get(connection, &id))
            .await
    }
    /// Lists at most 64 metadata records after the exact opaque local ID.
    pub async fn list(&self, after: Option<ConversationId>) -> Result<Vec<Snapshot>> {
        self.run(move |connection, _| store::list(connection, after.as_ref()))
            .await
    }
    /// Fences old peers before starting a new explicitly requested connection.
    pub async fn connect(&self, id: &ConversationId) -> Result<Snapshot> {
        let id = id.clone();
        self.mutate(move |connection, _| store::connect(connection, &id))
            .await
    }
    /// Saves the confirmed remote identity and advertised stable capabilities.
    pub async fn bind(
        &self,
        id: &ConversationId,
        generation: u64,
        remote: String,
        capabilities: Capabilities,
    ) -> Result<Snapshot> {
        let id = id.clone();
        self.mutate(move |connection, _| {
            store::bind(connection, &id, generation, remote, capabilities)
        })
        .await
    }
    /// Stores one observation exactly once for the current connection generation.
    pub async fn append(
        &self,
        id: &ConversationId,
        generation: u64,
        kind: RecordKind,
        value: serde_json::Value,
    ) -> Result<u64> {
        self.append_batch(id, generation, vec![(kind, value)])
            .await
            .map(|sequences| sequences[0])
    }
    /// Atomically stores up to 64 observations and 1 MiB encoded payload in order.
    pub async fn append_batch(
        &self,
        id: &ConversationId,
        generation: u64,
        records: Vec<(RecordKind, serde_json::Value)>,
    ) -> Result<Vec<u64>> {
        if records.is_empty() || records.len() > 64 {
            return Err(Error::Input);
        }
        let mut remaining = rsi_acp_protocol::MAX_FRAME_BYTES;
        let records = records
            .into_iter()
            .map(|(kind, value)| {
                let bytes = types::encode(&value, remaining)?;
                remaining -= bytes.len();
                Ok((kind, bytes))
            })
            .collect::<Result<Vec<_>>>()?;
        let id = id.clone();
        self.mutate(move |connection, limits| {
            let transaction = connection
                .transaction()
                .map_err(|error| store::sql(&error))?;
            let sequences = records
                .into_iter()
                .map(|(kind, bytes)| {
                    store::append(&transaction, limits, &id, generation, kind, &bytes)
                })
                .collect::<Result<Vec<_>>>()?;
            transaction.commit().map_err(|error| store::sql(&error))?;
            Ok(sequences)
        })
        .await
    }
    /// Records categorical local settlement without consuming observation quota.
    pub async fn settle(
        &self,
        id: &ConversationId,
        generation: u64,
        status: Status,
    ) -> Result<Snapshot> {
        let id = id.clone();
        self.mutate(move |connection, _| store::settle(connection, &id, generation, status))
            .await
    }
    /// Stores the exact confirmed remote prompt stop reason within reserved metadata.
    pub async fn complete(
        &self,
        id: &ConversationId,
        generation: u64,
        completion: Completion,
    ) -> Result<Snapshot> {
        let id = id.clone();
        self.mutate(move |connection, _| store::complete(connection, &id, generation, completion))
            .await
    }

    /// Routes upcoming load updates into an unpublished replacement epoch.
    pub async fn begin_replay(&self, id: &ConversationId, generation: u64) -> Result<Snapshot> {
        let id = id.clone();
        self.mutate(move |connection, _| store::begin_replay(connection, &id, generation))
            .await
    }
    /// Publishes a complete replay, or keeps the previous visible projection.
    pub async fn finish_replay(
        &self,
        id: &ConversationId,
        generation: u64,
        complete: bool,
    ) -> Result<Snapshot> {
        let id = id.clone();
        self.mutate(move |connection, _| {
            store::finish_replay(connection, &id, generation, complete)
        })
        .await
    }
    /// Reads the exact visible sequence tail without materializing any payload.
    pub async fn position(&self, id: &ConversationId, epoch: u64) -> Result<u64> {
        let id = id.clone();
        self.run(move |connection, _| store::position(connection, &id, epoch))
            .await
    }
    /// Reads bounded source descriptors and inline payloads for one exact epoch.
    pub async fn page(&self, id: &ConversationId, epoch: u64, after: u64) -> Result<Page> {
        let id = id.clone();
        self.run(move |connection, _| store::page(connection, &id, epoch, after))
            .await
    }
    /// Reads at most 64 KiB of exact encoded record bytes at a fixed source identity.
    pub async fn window(
        &self,
        id: &ConversationId,
        epoch: u64,
        sequence: u64,
        start: usize,
    ) -> Result<Vec<u8>> {
        let id = id.clone();
        self.run(move |connection, _| store::window(connection, &id, epoch, sequence, start))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mutations_wait_with_a_bound_and_keep_the_two_worker_limit() {
        let root = tempfile::tempdir().unwrap();
        let journal = Journal::open(root.path().to_owned(), Limits::default())
            .await
            .unwrap();
        let workers = journal
            .0
            .workers
            .clone()
            .acquire_many_owned(2)
            .await
            .unwrap();
        let mut pending = tokio::task::JoinSet::new();
        for _ in 0..32 {
            let journal = journal.clone();
            pending.spawn(async move { journal.mutate(|_, _| Ok(())).await });
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while journal.0.mutations.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(pending.try_join_next().is_none());
        assert_eq!(journal.list(None).await.unwrap_err(), Error::Busy);
        assert_eq!(
            journal.mutate(|_, _| Ok(())).await.unwrap_err(),
            Error::Busy
        );
        drop(workers);
        while let Some(result) = pending.join_next().await {
            result.unwrap().unwrap();
        }
        assert_eq!(journal.0.workers.available_permits(), 2);
        assert!(journal.list(None).await.unwrap().is_empty());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abandoned_blocking_jobs_retain_the_exclusive_lease_and_capacity() {
        let root = tempfile::tempdir().unwrap();
        let journal = Journal::open(root.path().to_owned(), Limits::default())
            .await
            .unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, blocked) = std::sync::mpsc::channel();
        let task = tokio::spawn({
            let journal = journal.clone();
            let entered = entered.clone();
            async move {
                journal
                    .run(move |_, _| {
                        entered.notify_one();
                        blocked.recv().map_err(|_| Error::Io)
                    })
                    .await
            }
        });
        entered.notified().await;
        let second = tokio::spawn({
            let journal = journal.clone();
            async move { journal.list(None).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while journal.0.workers.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(journal.list(None).await.unwrap_err(), Error::Busy);
        task.abort();
        second.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(second.await.unwrap_err().is_cancelled());
        drop(journal);
        assert_eq!(
            Journal::open(root.path().to_owned(), Limits::default())
                .await
                .unwrap_err(),
            Error::Locked
        );
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match Journal::open(root.path().to_owned(), Limits::default()).await {
                    Ok(journal) => break journal,
                    Err(Error::Locked) => tokio::task::yield_now().await,
                    other => panic!("unexpected reopen: {other:?}"),
                }
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explicit_retirement_survives_waiter_loss_and_releases_lease_with_retained_handles() {
        use std::future::Future as _;
        let root = tempfile::tempdir().unwrap();
        let journal = Journal::open(root.path().to_owned(), Limits::default())
            .await
            .unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, blocked) = std::sync::mpsc::channel();
        let task = tokio::spawn({
            let journal = journal.clone();
            let entered = entered.clone();
            async move {
                journal
                    .run(move |_, _| {
                        entered.notify_one();
                        blocked.recv().map_err(|_| Error::Io)
                    })
                    .await
            }
        });
        entered.notified().await;
        let retained = journal.clone();
        let mut close = Box::pin(journal.close());
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(close.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        drop(close);
        assert_eq!(retained.list(None).await.unwrap_err(), Error::Io);
        assert_eq!(
            Journal::open(root.path().to_owned(), Limits::default())
                .await
                .unwrap_err(),
            Error::Locked
        );
        release.send(()).unwrap();
        task.await.unwrap().unwrap();
        retained.close().await.unwrap();
        let reopened = Journal::open(root.path().to_owned(), Limits::default())
            .await
            .unwrap();
        assert!(reopened.list(None).await.unwrap().is_empty());
        assert_eq!(retained.list(None).await.unwrap_err(), Error::Io);
    }
}
