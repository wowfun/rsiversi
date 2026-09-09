use async_trait::async_trait;
use rsi_files_protocol::{
    DirectoryPage, FILE_TOKEN_LIFETIME, FileKind, FilePage, FileToken, Files, FilesBinding,
    FilesError, MAXIMUM_DIRECTORY_PAGE_ENTRIES, MAXIMUM_FILE_JOBS, MAXIMUM_FILE_PAGE_BYTES,
    MAXIMUM_FILE_TOKENS, OpenedFile, RelativePath, Result,
};
use std::sync::Arc;
use std::{
    collections::BTreeMap,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
#[path = "unix.rs"]
mod platform;
#[cfg(not(unix))]
#[path = "unsupported.rs"]
mod platform;

static JOBS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAXIMUM_FILE_JOBS)));
static TOKENS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAXIMUM_FILE_TOKENS)));
static GENERATIONS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct Resource {
    binding: FilesBinding,
    expires: Instant,
    _permit: OwnedSemaphorePermit,
    data: platform::Resource,
}
#[derive(Debug, Default)]
struct State {
    closed: bool,
    jobs: usize,
    next: u64,
    tokens: BTreeMap<FileToken, Arc<Resource>>,
}
#[derive(Debug, Default)]
struct Owner {
    state: Mutex<State>,
    stopped: CancellationToken,
    changed: Notify,
}
#[derive(Debug)]
struct Job {
    owner: Arc<Owner>,
    _permit: OwnedSemaphorePermit,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.owner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .jobs -= 1;
        self.owner.changed.notify_waiters();
    }
}

/// Native read provider. Runtime compositions normally use `FilesFactory`.
#[derive(Debug)]
pub struct LocalFiles {
    generation: u64,
    owner: Arc<Owner>,
}
impl LocalFiles {
    /// Allocate an independent token generation.
    pub fn new() -> Result<Self> {
        let generation = GENERATIONS
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| FilesError::Capacity)?;
        Ok(Self {
            generation,
            owner: Arc::new(Owner::default()),
        })
    }
    /// Stop admission, revoke retained tokens and await actual blocking work.
    pub async fn close(&self) {
        {
            let mut state = self
                .owner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closed = true;
            state.tokens.clear();
        }
        self.owner.stopped.cancel();
        loop {
            let changed = self.owner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self
                .owner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .jobs
                == 0
            {
                break;
            }
            changed.await;
        }
    }
    fn acquire(&self, cancellation: &CancellationToken) -> Result<Job> {
        if cancellation.is_cancelled() || self.owner.stopped.is_cancelled() {
            return Err(FilesError::Cancelled);
        }
        let permit = Arc::clone(&JOBS)
            .try_acquire_owned()
            .map_err(|_| FilesError::Capacity)?;
        let mut state = self
            .owner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(FilesError::Cancelled);
        }
        prune(&mut state);
        state.jobs += 1;
        Ok(Job {
            owner: self.owner.clone(),
            _permit: permit,
        })
    }
    async fn run<T: Send + 'static>(
        &self,
        cancellation: CancellationToken,
        operation: impl FnOnce(CancellationToken) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let job = self.acquire(&cancellation)?;
        let stopped = cancellation.child_token();
        let _on_drop = stopped.clone().drop_guard();
        let result = tokio::task::spawn_blocking(move || {
            let _job = job;
            check(&stopped)?;
            operation(stopped)
        });
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(FilesError::Cancelled),
            () = self.owner.stopped.cancelled() => Err(FilesError::Cancelled),
            result = result => result.map_err(|_| FilesError::Io)?,
        }
    }
    fn resource(&self, binding: &FilesBinding, token: &FileToken) -> Result<Arc<Resource>> {
        let mut state = self
            .owner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(FilesError::Cancelled);
        }
        prune(&mut state);
        let resource = state.tokens.get(token).ok_or(FilesError::Unavailable)?;
        if &resource.binding != binding {
            return Err(FilesError::Binding);
        }
        Ok(resource.clone())
    }
}
impl Drop for LocalFiles {
    fn drop(&mut self) {
        self.owner.stopped.cancel();
        let mut state = self
            .owner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        state.tokens.clear();
    }
}
fn prune(state: &mut State) {
    let now = Instant::now();
    state.tokens.retain(|_, resource| resource.expires > now);
}
fn check(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(FilesError::Cancelled)
    } else {
        Ok(())
    }
}
#[async_trait]
impl Files for LocalFiles {
    async fn open(
        &self,
        binding: FilesBinding,
        path: RelativePath,
        kind: FileKind,
        cancellation: CancellationToken,
    ) -> Result<OpenedFile> {
        let resource = self
            .run(cancellation.clone(), move |stopped| {
                let permit = Arc::clone(&TOKENS)
                    .try_acquire_owned()
                    .map_err(|_| FilesError::Capacity)?;
                let data = platform::open(&binding, path, kind, &stopped)?;
                check(&stopped)?;
                Ok(Resource {
                    binding,
                    expires: Instant::now() + FILE_TOKEN_LIFETIME,
                    _permit: permit,
                    data,
                })
            })
            .await?;
        check(&cancellation)?;
        let mut state = self
            .owner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(FilesError::Cancelled);
        }
        let next = state.next.checked_add(1).ok_or(FilesError::Capacity)?;
        state.next = next;
        let token = FileToken::try_from(format!("{:016x}{next:016x}", self.generation))?;
        let length = resource.data.length();
        state.tokens.insert(token.clone(), Arc::new(resource));
        Ok(OpenedFile {
            token,
            kind,
            length,
        })
    }

    async fn read(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: u64,
        maximum: usize,
        cancellation: CancellationToken,
    ) -> Result<FilePage> {
        if maximum == 0 || maximum > MAXIMUM_FILE_PAGE_BYTES {
            return Err(FilesError::Invalid);
        }
        let resource = self.resource(&binding, &token)?;
        self.run(cancellation, move |stopped| {
            resource.data.read(offset, maximum, &stopped)
        })
        .await
    }
    async fn list(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: usize,
        maximum: usize,
        cancellation: CancellationToken,
    ) -> Result<DirectoryPage> {
        if maximum == 0 || maximum > MAXIMUM_DIRECTORY_PAGE_ENTRIES {
            return Err(FilesError::Invalid);
        }
        let resource = self.resource(&binding, &token)?;
        self.run(cancellation, move |stopped| {
            resource.data.list(offset, maximum, &stopped)
        })
        .await
    }
    fn release(&self, binding: &FilesBinding, token: &FileToken) -> Result<()> {
        let mut state = self
            .owner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune(&mut state);
        if state
            .tokens
            .get(token)
            .is_some_and(|resource| &resource.binding != binding)
        {
            return Err(FilesError::Binding);
        }
        state.tokens.remove(token);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use rsi_files_protocol::FilesCaller;
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn dropped_waiters_and_retirement_keep_actual_jobs_charged_across_generations() {
        let _serial = SERIAL.lock().await;
        let old = Arc::new(LocalFiles::new().unwrap());
        let new = LocalFiles::new().unwrap();
        let mut releases = Vec::new();
        for _ in 0..MAXIMUM_FILE_JOBS {
            let service = old.clone();
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let waiter = tokio::spawn(async move {
                service
                    .run(CancellationToken::new(), move |stopped| {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        assert!(stopped.is_cancelled());
                        Ok(())
                    })
                    .await
            });
            entered_rx.await.unwrap();
            waiter.abort();
            assert!(waiter.await.unwrap_err().is_cancelled());
            releases.push(release_tx);
        }
        assert!(matches!(
            new.acquire(&CancellationToken::new()),
            Err(FilesError::Capacity)
        ));
        let closing = old.clone();
        let close = tokio::spawn(async move {
            closing.close().await;
        });
        old.owner.stopped.cancelled().await;
        assert!(!close.is_finished());
        assert_eq!(old.owner.state.lock().unwrap().jobs, MAXIMUM_FILE_JOBS);
        assert!(matches!(
            old.acquire(&CancellationToken::new()),
            Err(FilesError::Cancelled)
        ));
        for release in releases {
            release.send(()).unwrap();
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), close)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old.owner.state.lock().unwrap().jobs, 0);
        let permit = new.acquire(&CancellationToken::new()).unwrap();
        drop(permit);
        new.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn expiry_is_fixed_and_last_in_flight_owner_keeps_its_token_permit() {
        let _serial = SERIAL.lock().await;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        std::fs::write(root.join("file"), "body").unwrap();
        let service = LocalFiles::new().unwrap();
        let binding = FilesBinding::new(FilesCaller::default(), "session", "header", root).unwrap();
        let opened = service
            .open(
                binding.clone(),
                RelativePath::new(b"file").unwrap(),
                FileKind::File,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let held = service.resource(&binding, &opened.token).unwrap();
        let expires = held.expires;
        service
            .read(
                binding.clone(),
                opened.token.clone(),
                0,
                1,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            service.resource(&binding, &opened.token).unwrap().expires,
            expires
        );
        service.release(&binding, &opened.token).unwrap();
        assert_eq!(TOKENS.available_permits(), MAXIMUM_FILE_TOKENS - 1);
        drop(held);
        assert_eq!(TOKENS.available_permits(), MAXIMUM_FILE_TOKENS);
        let opened = service
            .open(
                binding.clone(),
                RelativePath::new(b"file").unwrap(),
                FileKind::File,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        {
            let mut state = service.owner.state.lock().unwrap();
            Arc::get_mut(state.tokens.get_mut(&opened.token).unwrap())
                .unwrap()
                .expires = Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap();
        }
        assert!(matches!(
            service.resource(&binding, &opened.token),
            Err(FilesError::Unavailable)
        ));
        assert_eq!(TOKENS.available_permits(), MAXIMUM_FILE_TOKENS);
        service.close().await;
    }
}
