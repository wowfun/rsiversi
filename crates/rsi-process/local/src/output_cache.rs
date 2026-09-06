//! Completed-only cache. The worker alone owns filesystem effects and quota.

use async_trait::async_trait;
use rsi_process::{OutputPage, ProcessError, ProcessOutputCache, Result, validate_output_read};
use rustix::fs::{AtFlags, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read as _, Seek as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};

const QUEUED_BYTES: usize = 8 * 1024 * 1024;
const MAXIMUM_SCAN_ENTRIES: usize = 4096;

/// Bounded completed-output cache configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputCacheConfig {
    /// Explicit absolute private cache directory; no ambient path discovery.
    pub directory: PathBuf,
    /// Maximum complete bytes retained per stream.
    #[serde(default = "stream_bytes")]
    pub maximum_stream_bytes: u64,
    /// Maximum complete plus reserved active stream bytes.
    #[serde(default = "total_bytes")]
    pub maximum_total_bytes: u64,
    /// Maximum complete plus active stream files.
    #[serde(default = "file_count")]
    pub maximum_files: usize,
}

const fn stream_bytes() -> u64 {
    rsi_process::MAXIMUM_COMPLETED_OUTPUT_BYTES
}
const fn total_bytes() -> u64 {
    512 * 1024 * 1024
}
const fn file_count() -> usize {
    256
}

impl OutputCacheConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        if !self.directory.is_absolute()
            || self
                .directory
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
            || self.maximum_stream_bytes == 0
            || self.maximum_stream_bytes > stream_bytes()
            || self.maximum_total_bytes < self.maximum_stream_bytes
            || self.maximum_total_bytes > 1024 * 1024 * 1024
            || self.maximum_files == 0
            || self.maximum_files > 256
        {
            return Err(ProcessError::InvalidInput(
                "invalid output cache bounds or directory".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
enum CaptureState {
    #[default]
    Pending,
    Complete(String),
    Unavailable,
}

fn lock(state: &Mutex<CaptureState>) -> std::sync::MutexGuard<'_, CaptureState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug)]
pub(crate) struct Capture {
    state: Arc<Mutex<CaptureState>>,
    cache: Arc<Cache>,
    id: String,
}

impl Capture {
    pub(crate) fn reference(&self) -> Option<String> {
        match &*lock(&self.state) {
            CaptureState::Complete(id) => Some(id.clone()),
            CaptureState::Pending | CaptureState::Unavailable => None,
        }
    }

    pub(crate) fn push(&self, bytes: &[u8]) {
        if !matches!(*lock(&self.state), CaptureState::Pending) {
            return;
        }
        let Ok(permit) =
            self.cache.bytes.clone().try_acquire_many_owned(
                u32::try_from(bytes.len()).expect("pipe chunks are bounded"),
            )
        else {
            self.abandon();
            return;
        };
        let command = Command::Write {
            id: self.id.clone(),
            bytes: bytes.to_vec(),
            _permit: permit,
        };
        if self.cache.sender.try_send(command).is_err() {
            self.abandon();
        }
    }

    pub(crate) async fn finish(&self) {
        let (sender, receiver) = oneshot::channel();
        if self
            .cache
            .sender
            .try_send(Command::Finish {
                id: self.id.clone(),
                done: sender,
            })
            .is_err()
        {
            self.abandon();
            return;
        }
        if tokio::time::timeout(Duration::from_millis(250), receiver)
            .await
            .is_err()
        {
            self.abandon();
        }
    }

    pub(crate) fn abandon(&self) {
        let mut state = lock(&self.state);
        // A completed publication already won the EOF/deadline race.
        if matches!(*state, CaptureState::Pending) {
            *state = CaptureState::Unavailable;
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.abandon();
    }
}

#[derive(Debug)]
pub(crate) struct Cache {
    closed: Arc<AtomicBool>,
    stopped: watch::Receiver<bool>,
    sender: mpsc::Sender<Command>,
    bytes: Arc<Semaphore>,
}

impl Cache {
    pub(crate) fn open(config: OutputCacheConfig) -> Result<Arc<Self>> {
        config.validate()?;
        let worker = Worker::open(config)?;
        let (sender, receiver) = mpsc::channel(1024);
        let closed = Arc::new(AtomicBool::new(false));
        let stopping = closed.clone();
        let (completed, stopped) = watch::channel(false);
        std::thread::Builder::new()
            .name("rsi-output-cache".into())
            .spawn(move || {
                worker.run(receiver, &stopping);
                completed.send_replace(true);
            })
            .map_err(io_error)?;
        Ok(Arc::new(Self {
            closed,
            stopped,
            sender,
            bytes: Arc::new(Semaphore::new(QUEUED_BYTES)),
        }))
    }

    pub(crate) fn capture(self: &Arc<Self>) -> Option<Capture> {
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).ok()?;
        let id = hex::encode(random);
        let state = Arc::new(Mutex::new(CaptureState::Pending));
        self.sender
            .try_send(Command::Begin {
                id: id.clone(),
                state: state.clone(),
            })
            .ok()?;
        Some(Capture {
            state,
            cache: self.clone(),
            id,
        })
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        self.closed.store(true, Ordering::Release);
        // Wake an idle receiver. A full queue already guarantees it will observe
        // the stop flag before it can block again; delivery never waits on I/O.
        let _ = self.sender.try_send(Command::Shutdown);
        let mut stopped = self.stopped.clone();
        stopped
            .wait_for(|stopped| *stopped)
            .await
            .map(|_| ())
            .map_err(|_| ProcessError::ShuttingDown)
    }
}

#[async_trait]
impl ProcessOutputCache for Cache {
    async fn read(&self, id: &str, offset: u64, limit: usize) -> Result<OutputPage> {
        validate_output_read(id, limit)?;
        if self.closed.load(Ordering::Acquire) {
            return Err(ProcessError::ShuttingDown);
        }
        let (done, receiver) = oneshot::channel();
        self.sender
            .try_send(Command::Read {
                id: id.into(),
                offset,
                limit,
                done,
            })
            .map_err(|_| ProcessError::Capacity)?;
        tokio::time::timeout(Duration::from_secs(5), receiver)
            .await
            .map_err(|_| ProcessError::Io("output cache read timed out".into()))?
            .map_err(|_| ProcessError::ShuttingDown)?
    }
}

#[derive(Debug)]
enum Command {
    Shutdown,
    #[cfg(test)]
    Block {
        entered: oneshot::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    },
    Begin {
        id: String,
        state: Arc<Mutex<CaptureState>>,
    },
    Write {
        id: String,
        bytes: Vec<u8>,
        _permit: OwnedSemaphorePermit,
    },
    Finish {
        id: String,
        done: oneshot::Sender<()>,
    },
    Read {
        id: String,
        offset: u64,
        limit: usize,
        done: oneshot::Sender<Result<OutputPage>>,
    },
}

struct Active {
    file: File,
    bytes: u64,
    state: Arc<Mutex<CaptureState>>,
}

struct Entry {
    id: String,
    bytes: u64,
    device: u64,
    inode: u64,
}

struct Worker {
    config: OutputCacheConfig,
    directory: File,
    lease: File,
    active: HashMap<String, Active>,
    completed: VecDeque<Entry>,
    reserved: u64,
    cleanup_failed: bool,
    #[cfg(test)]
    fail_removal: bool,
}

impl Drop for Worker {
    fn drop(&mut self) {
        // A concurrent fork can retain the open-file description until exec.
        // Actual worker I/O has ended; release the lock before acknowledging shutdown.
        let _ = self.lease.unlock();
    }
}

impl Worker {
    fn open(config: OutputCacheConfig) -> Result<Self> {
        let directory = open_directory(&config.directory)?;
        let lease = open_file(&directory, "owner.lock", OFlags::RDWR | OFlags::CREATE)?;
        validate_file(&lease)?;
        lease.try_lock().map_err(io_error)?;
        let mut worker = Self {
            config,
            directory,
            lease,
            active: HashMap::new(),
            completed: VecDeque::new(),
            reserved: 0,
            cleanup_failed: false,
            #[cfg(test)]
            fail_removal: false,
        };
        let mut entries = Vec::new();
        for (index, entry) in rustix::fs::Dir::read_from(&worker.directory)
            .map_err(io_error)?
            .enumerate()
        {
            if index >= MAXIMUM_SCAN_ENTRIES {
                return Err(ProcessError::Capacity);
            }
            let entry = entry.map_err(io_error)?;
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            let (id, partial) = if let Some(id) = name.strip_suffix(".log") {
                (id, false)
            } else if let Some(id) = name.strip_suffix(".part") {
                (id, true)
            } else {
                continue;
            };
            if validate_output_read(id, 1).is_err() {
                continue;
            }
            let Ok(file) = open_file(&worker.directory, name, OFlags::RDONLY) else {
                continue;
            };
            let Ok(metadata) = validate_private_file(&file) else {
                continue;
            };
            if partial
                || metadata.nlink() != 1
                || metadata.len() > worker.config.maximum_stream_bytes
            {
                worker.remove(name)?;
                continue;
            }
            entries.push((
                metadata.mtime(),
                metadata.mtime_nsec(),
                Entry {
                    id: id.into(),
                    bytes: metadata.len(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                },
            ));
        }
        entries.sort_by(|a, b| (a.0, a.1, &a.2.id).cmp(&(b.0, b.1, &b.2.id)));
        for (_, _, entry) in entries {
            worker.reserved += entry.bytes;
            worker.completed.push_back(entry);
        }
        while worker.reserved > worker.config.maximum_total_bytes
            || worker.completed.len() > worker.config.maximum_files
        {
            worker.evict()?;
        }
        Ok(worker)
    }

    fn run(mut self, mut receiver: mpsc::Receiver<Command>, stopping: &AtomicBool) {
        loop {
            if stopping.load(Ordering::Acquire) {
                receiver.close();
            }
            let Some(command) = receiver.blocking_recv() else {
                break;
            };
            // Dead/failed captures release their files only here, after actual I/O.
            let abandoned = self
                .active
                .iter()
                .filter(|(_, active)| !matches!(*lock(&active.state), CaptureState::Pending))
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            for id in abandoned {
                self.discard(&id);
            }
            match command {
                Command::Shutdown => {
                    receiver.close();
                }
                #[cfg(test)]
                Command::Block { entered, release } => {
                    let _ = entered.send(());
                    let _ = release.recv();
                }
                Command::Begin { id, state } => {
                    if self.begin(&id, state.clone()).is_err() {
                        *lock(&state) = CaptureState::Unavailable;
                    }
                }
                Command::Write { id, bytes, _permit } => {
                    let failed = self.active.get_mut(&id).is_some_and(|active| {
                        if active.bytes.saturating_add(bytes.len() as u64)
                            > self.config.maximum_stream_bytes
                        {
                            return true;
                        }
                        if active.file.write_all(&bytes).is_err() {
                            return true;
                        }
                        active.bytes += bytes.len() as u64;
                        false
                    });
                    if failed {
                        self.discard(&id);
                    }
                    // _permit includes the chunk currently in a blocking write.
                }
                Command::Finish { id, done } => {
                    self.finish(&id);
                    let _ = done.send(());
                }
                Command::Read {
                    id,
                    offset,
                    limit,
                    done,
                } => {
                    if !done.is_closed() {
                        let _ = done.send(self.read(&id, offset, limit));
                    }
                }
            }
        }
        for id in self.active.keys().cloned().collect::<Vec<_>>() {
            self.discard(&id);
        }
        drop(self);
    }

    fn begin(&mut self, id: &str, state: Arc<Mutex<CaptureState>>) -> Result<()> {
        if self.cleanup_failed {
            return Err(ProcessError::Capacity);
        }
        if !matches!(*lock(&state), CaptureState::Pending) {
            return Ok(());
        }
        if self.active.contains_key(id) || self.completed.iter().any(|entry| entry.id == id) {
            return Err(ProcessError::Capacity);
        }
        while self.reserved + self.config.maximum_stream_bytes > self.config.maximum_total_bytes
            || self.active.len() + self.completed.len() >= self.config.maximum_files
        {
            self.evict()?;
        }
        let file = open_file(
            &self.directory,
            &format!("{id}.part"),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
        )?;
        self.reserved += self.config.maximum_stream_bytes;
        self.active.insert(
            id.into(),
            Active {
                file,
                bytes: 0,
                state,
            },
        );
        Ok(())
    }

    fn discard(&mut self, id: &str) {
        if let Some(active) = self.active.remove(id) {
            *lock(&active.state) = CaptureState::Unavailable;
            drop(active.file);
            // Failed deletion keeps quota reserved for the unremoved file.
            if self.remove(&format!("{id}.part")).is_ok() {
                self.reserved -= self.config.maximum_stream_bytes;
            } else {
                self.cleanup_failed = true;
            }
        }
    }

    fn finish(&mut self, id: &str) {
        let Some(active) = self.active.remove(id) else {
            return;
        };
        let metadata = active.file.metadata();
        drop(active.file);
        let final_name = format!("{id}.log");
        let partial_name = format!("{id}.part");
        // Never hold the deadline state lock across filesystem I/O.
        let linked = !self.cleanup_failed
            && matches!(*lock(&active.state), CaptureState::Pending)
            && metadata.is_ok()
            && rustix::fs::linkat(
                &self.directory,
                partial_name.as_str(),
                &self.directory,
                final_name.as_str(),
                AtFlags::empty(),
            )
            .is_ok();
        let removed_partial = self.remove(&partial_name).is_ok();
        let mut state = lock(&active.state);
        let published = linked && removed_partial && matches!(*state, CaptureState::Pending);
        if published {
            let metadata = metadata.expect("checked metadata");
            self.completed.push_back(Entry {
                id: id.into(),
                bytes: active.bytes,
                device: metadata.dev(),
                inode: metadata.ino(),
            });
            *state = CaptureState::Complete(id.into());
        } else {
            *state = CaptureState::Unavailable;
        }
        drop(state);
        let removed_final = !linked || published || self.remove(&final_name).is_ok();
        if removed_partial && removed_final {
            self.reserved -= self.config.maximum_stream_bytes;
            if published {
                self.reserved += active.bytes;
            }
        } else {
            self.cleanup_failed = true;
        }
    }

    fn evict(&mut self) -> Result<()> {
        let entry = self.completed.front().ok_or(ProcessError::Capacity)?;
        self.remove(&format!("{}.log", entry.id))?;
        self.reserved -= entry.bytes;
        self.completed.pop_front();
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<()> {
        #[cfg(test)]
        if self.fail_removal {
            return Err(ProcessError::Io("injected cache removal failure".into()));
        }
        match rustix::fs::unlinkat(&self.directory, name, AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
            Err(error) => Err(io_error(error)),
        }
    }

    fn read(&self, id: &str, offset: u64, limit: usize) -> Result<OutputPage> {
        let entry = self
            .completed
            .iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| ProcessError::Io("output unavailable or evicted".into()))?;
        let mut file = open_file(&self.directory, &format!("{id}.log"), OFlags::RDONLY)?;
        let metadata = validate_file(&file)?;
        if (metadata.dev(), metadata.ino(), metadata.len())
            != (entry.device, entry.inode, entry.bytes)
        {
            return Err(ProcessError::Io("cached output identity changed".into()));
        }
        if offset > entry.bytes {
            return Err(ProcessError::InvalidInput(
                "output offset exceeds stream length".into(),
            ));
        }
        let count =
            usize::try_from((entry.bytes - offset).min(limit as u64)).expect("bounded page");
        file.seek(std::io::SeekFrom::Start(offset))
            .map_err(io_error)?;
        let mut bytes = vec![0; count];
        file.read_exact(&mut bytes).map_err(io_error)?;
        Ok(OutputPage {
            id: id.into(),
            offset,
            next_offset: offset + count as u64,
            total_bytes: entry.bytes,
            bytes,
        })
    }
}

fn io_error(error: impl std::fmt::Display) -> ProcessError {
    ProcessError::Io(error.to_string())
}

fn open_file(directory: &File, name: &str, flags: OFlags) -> Result<File> {
    rustix::fs::openat(
        directory,
        name,
        flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::RUSR | Mode::WUSR,
    )
    .map(File::from)
    .map_err(io_error)
}

fn validate_file(file: &File) -> Result<std::fs::Metadata> {
    let metadata = validate_private_file(file)?;
    if metadata.nlink() != 1 {
        return Err(ProcessError::Io(
            "output cache entry has multiple links".into(),
        ));
    }
    Ok(metadata)
}

fn validate_private_file(file: &File) -> Result<std::fs::Metadata> {
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(ProcessError::Io(
            "output cache entry is not a private regular file".into(),
        ));
    }
    Ok(metadata)
}

fn open_directory(path: &Path) -> Result<File> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = File::from(rustix::fs::open("/", flags, Mode::empty()).map_err(io_error)?);
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                match rustix::fs::mkdirat(&directory, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(io_error(error)),
                }
                directory = File::from(
                    rustix::fs::openat(&directory, name, flags, Mode::empty()).map_err(io_error)?,
                );
            }
            _ => {
                return Err(ProcessError::InvalidInput(
                    "output cache path must be normalized and absolute".into(),
                ));
            }
        }
    }
    let metadata = directory.metadata().map_err(io_error)?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
        return Err(ProcessError::Io(
            "output cache directory must be private and owner-owned".into(),
        ));
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    fn config(path: &Path) -> OutputCacheConfig {
        OutputCacheConfig {
            directory: path.join("output"),
            maximum_stream_bytes: 64,
            maximum_total_bytes: 128,
            maximum_files: 2,
        }
    }

    #[test]
    fn failed_unlink_keeps_reservations_and_closes_capture_admission() {
        for finish in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut worker = Worker::open(config(root.path())).unwrap();
            let id = "a".repeat(32);
            let state = Arc::new(Mutex::new(CaptureState::Pending));
            worker.begin(&id, state.clone()).unwrap();
            worker.fail_removal = true;
            if finish {
                worker.finish(&id);
            } else {
                worker.discard(&id);
            }
            assert!(matches!(*lock(&state), CaptureState::Unavailable));
            assert_eq!(worker.reserved, 64);
            assert!(worker.cleanup_failed);
            worker.fail_removal = false;
            assert!(matches!(
                worker.begin(&"b".repeat(32), Arc::new(Mutex::new(CaptureState::Pending))),
                Err(ProcessError::Capacity)
            ));
        }
    }

    async fn complete(cache: &Arc<Cache>, bytes: &[u8]) -> Capture {
        let capture = cache.capture().unwrap();
        capture.push(bytes);
        capture.finish().await;
        capture
    }

    async fn block(cache: &Cache) -> std::sync::mpsc::Sender<()> {
        let (entered, waiting) = oneshot::channel();
        let (release, receiver) = std::sync::mpsc::channel();
        cache
            .sender
            .send(Command::Block {
                entered,
                release: receiver,
            })
            .await
            .unwrap();
        waiting.await.unwrap();
        release
    }

    #[tokio::test]
    async fn cache_cleanup_uses_the_provider_deadline_instead_of_an_earlier_internal_timeout() {
        let temporary = tempfile::tempdir().unwrap();
        let config = config(temporary.path());
        let cache = Cache::open(config).unwrap();
        let release = block(&cache).await;
        let mut service = crate::Service::new(crate::ProcessLocalConfig::default());
        service.cache = Some(cache.clone());
        let cleanup = tokio::spawn(async move { service.shutdown().await });
        while !cache.closed.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(1100)).await;
        release.send(()).unwrap();
        assert_eq!(
            cleanup.await.unwrap(),
            Ok(()),
            "cache recovery within the provider deadline must permit clean retirement"
        );
    }

    #[tokio::test]
    async fn complete_raw_pages_survive_shutdown_and_restart_including_split_utf8() {
        let temporary = tempfile::tempdir().unwrap();
        let config = config(temporary.path());
        let cache = Cache::open(config.clone()).unwrap();
        let capture = complete(&cache, "a中b".as_bytes()).await;
        let id = capture.reference().unwrap();
        let first = cache.read(&id, 0, 2).await.unwrap();
        assert_eq!(first.bytes, [b'a', 0xe4]);
        assert_eq!(
            (first.offset, first.next_offset, first.total_bytes),
            (0, 2, 5)
        );
        cache.shutdown().await.unwrap();
        // Old capture handles remain readable but cannot keep the writer lease.
        assert_eq!(capture.reference(), Some(id.clone()));
        let reopened = Cache::open(config).unwrap();
        assert_eq!(
            reopened.read(&id, 2, 64).await.unwrap().bytes,
            [0xb8, 0xad, b'b']
        );
        assert!(reopened.read(&id, 5, 1).await.unwrap().bytes.is_empty());
        assert!(reopened.read(&id, 6, 1).await.is_err());
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn empty_streams_publish_but_overflow_and_dropped_streams_do_not() {
        let temporary = tempfile::tempdir().unwrap();
        let config = config(temporary.path());
        let cache = Cache::open(config.clone()).unwrap();
        let empty = complete(&cache, b"").await;
        assert_eq!(
            cache
                .read(&empty.reference().unwrap(), 0, 1)
                .await
                .unwrap()
                .total_bytes,
            0
        );
        let overflow = complete(&cache, &[7; 65]).await;
        assert!(overflow.reference().is_none());
        let dropped = cache.capture().unwrap();
        let id = dropped.id.clone();
        dropped.push(b"partial");
        drop(dropped);
        assert!(cache.read(&id, 0, 1).await.is_err());
        cache.shutdown().await.unwrap();
        assert!(!std::fs::read_dir(config.directory).unwrap().any(|entry| {
            entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|extension| extension == "part")
        }));
    }

    #[tokio::test]
    async fn fifo_eviction_never_evicts_an_active_reservation() {
        let temporary = tempfile::tempdir().unwrap();
        let config = config(temporary.path());
        let cache = Cache::open(config).unwrap();
        let first = complete(&cache, &[1; 64]).await;
        let second = complete(&cache, &[2; 64]).await;
        let third = complete(&cache, &[3; 64]).await;
        assert!(cache.read(&first.reference().unwrap(), 0, 1).await.is_err());
        assert_eq!(
            cache
                .read(&second.reference().unwrap(), 0, 1)
                .await
                .unwrap()
                .bytes,
            [2]
        );
        assert_eq!(
            cache
                .read(&third.reference().unwrap(), 0, 1)
                .await
                .unwrap()
                .bytes,
            [3]
        );
        let active1 = cache.capture().unwrap();
        let active2 = cache.capture().unwrap();
        let rejected = complete(&cache, b"no slot").await;
        assert!(rejected.reference().is_none());
        active1.push(b"kept");
        active1.finish().await;
        assert_eq!(
            cache
                .read(&active1.reference().unwrap(), 0, 64)
                .await
                .unwrap()
                .bytes,
            b"kept"
        );
        drop(active2);
        cache.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn blocked_writer_has_bounded_queue_and_never_delays_pipe_pushes() {
        let temporary = tempfile::tempdir().unwrap();
        let mut config = config(temporary.path());
        config.maximum_stream_bytes = 64 * 1024 * 1024;
        config.maximum_total_bytes = 64 * 1024 * 1024;
        let cache = Cache::open(config).unwrap();
        let release = block(&cache).await;
        let capture = cache.capture().unwrap();
        for _ in 0..2048 {
            capture.push(&[b'x'; 8192]);
        }
        assert!(matches!(*lock(&capture.state), CaptureState::Unavailable));
        assert!(cache.bytes.available_permits() <= QUEUED_BYTES);
        // Finishing cannot block on an already saturated worker queue.
        tokio::time::timeout(Duration::from_secs(1), capture.finish())
            .await
            .unwrap();
        release.send(()).unwrap();
        cache.shutdown().await.unwrap();
        assert_eq!(cache.bytes.available_permits(), QUEUED_BYTES);
    }

    #[tokio::test]
    async fn eof_timeout_prevents_late_publication_and_retains_the_writer_lease() {
        let temporary = tempfile::tempdir().unwrap();
        let config = config(temporary.path());
        let cache = Cache::open(config.clone()).unwrap();
        let capture = cache.capture().unwrap();
        capture.push(b"complete bytes but not yet published");
        let release = block(&cache).await;
        capture.finish().await;
        assert!(matches!(*lock(&capture.state), CaptureState::Unavailable));
        assert!(Cache::open(config.clone()).is_err());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), cache.shutdown())
                .await
                .is_err()
        );
        assert!(Cache::open(config.clone()).is_err());
        release.send(()).unwrap();
        // The retained worker must actually finish before another can open.
        let reopened = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(cache) = Cache::open(config.clone()) {
                    break cache;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(capture.reference().is_none());
        assert!(reopened.read(&capture.id, 0, 64).await.is_err());
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn saturated_shutdown_survives_the_waiter_deadline_and_releases_its_lease() {
        let temporary = tempfile::tempdir().unwrap();
        let config = config(temporary.path());
        let cache = Cache::open(config.clone()).unwrap();
        let capture = cache.capture().unwrap();
        let release = block(&cache).await;
        while cache.sender.capacity() > 0 {
            let (done, _) = oneshot::channel();
            cache
                .sender
                .try_send(Command::Read {
                    id: capture.id.clone(),
                    offset: 0,
                    limit: 1,
                    done,
                })
                .unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(1), cache.shutdown())
                .await
                .is_err()
        );
        assert!(Cache::open(config.clone()).is_err());
        release.send(()).unwrap();
        let reopened = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(cache) = Cache::open(config.clone()) {
                    break cache;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("saturated queue lost the shutdown request");
        cache.shutdown().await.unwrap();
        cache.shutdown().await.unwrap();
        assert!(capture.reference().is_none());
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn reads_reject_paths_links_changed_identity_and_unbounded_pages() {
        let temporary = tempfile::tempdir().unwrap();
        let config = config(temporary.path());
        let cache = Cache::open(config.clone()).unwrap();
        for id in [
            "../secret",
            "/etc/passwd",
            "",
            "0123456789012345678901234567890G",
        ] {
            assert!(matches!(
                cache.read(id, 0, 1).await,
                Err(ProcessError::InvalidInput(_))
            ));
        }
        let capture = complete(&cache, b"abc").await;
        let id = capture.reference().unwrap();
        for limit in [0, 65537, usize::MAX] {
            assert!(cache.read(&id, 0, limit).await.is_err());
        }
        let path = config.directory.join(format!("{id}.log"));
        let original = config.directory.join("original");
        std::fs::rename(&path, &original).unwrap();
        symlink(&original, &path).unwrap();
        assert!(cache.read(&id, 0, 1).await.is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"abc").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(cache.read(&id, 0, 1).await.is_err());
        cache.shutdown().await.unwrap();
    }

    #[test]
    fn worker_releases_its_lock_even_while_a_duplicate_descriptor_remains_open() {
        let temporary = tempfile::tempdir().unwrap();
        let config = config(temporary.path());
        let worker = Worker::open(config.clone()).unwrap();
        // A concurrent fork temporarily retains the same open-file description until exec.
        let duplicate = worker.lease.try_clone().unwrap();
        drop(worker);
        let next = Worker::open(config).unwrap();
        drop(next);
        drop(duplicate);
    }

    #[tokio::test]
    async fn startup_reclaims_crash_left_hard_links_without_touching_external_names() {
        for external_link in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let config = config(temporary.path());
            let cache = Cache::open(config.clone()).unwrap();
            cache.shutdown().await.unwrap();
            let id = "c".repeat(32);
            let partial = config.directory.join(format!("{id}.part"));
            let completed = config.directory.join(format!("{id}.log"));
            std::fs::write(&partial, b"crash").unwrap();
            std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::hard_link(&partial, &completed).unwrap();
            let external = temporary.path().join("external-link");
            if external_link {
                std::fs::hard_link(&partial, &external).unwrap();
            }
            let reopened = Cache::open(config.clone()).unwrap();
            assert!(!partial.exists(), "crash-left partial was leaked");
            let page = reopened.read(&id, 0, 16).await;
            if external_link {
                assert!(!completed.exists(), "multiply-linked output was leaked");
                assert_eq!(std::fs::read(&external).unwrap(), b"crash");
                assert!(page.is_err());
            } else if completed.exists() {
                // Directory enumeration may encounter the partial first. Its
                // removal leaves a safe single-link completed file to account.
                assert_eq!(page.unwrap().bytes, b"crash");
            } else {
                assert!(page.is_err());
            }
            let capture = complete(&reopened, b"healthy").await;
            assert!(capture.reference().is_some());
            reopened.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn startup_cleans_only_own_valid_partials_and_rejects_unsafe_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let config = config(temporary.path());
        let cache = Cache::open(config.clone()).unwrap();
        cache.shutdown().await.unwrap();
        let partial = config.directory.join(format!("{}.part", "a".repeat(32)));
        std::fs::write(&partial, b"partial").unwrap();
        std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o600)).unwrap();
        let unknown = config.directory.join("unknown.part");
        std::fs::write(&unknown, b"not ours").unwrap();
        let reopened = Cache::open(config.clone()).unwrap();
        assert!(!partial.exists());
        assert!(unknown.exists());
        reopened.shutdown().await.unwrap();
        let alias = temporary.path().join("alias");
        symlink(&config.directory, &alias).unwrap();
        let mut bad = config.clone();
        bad.directory = alias.join("nested");
        assert!(Cache::open(bad).is_err());
        std::fs::set_permissions(&config.directory, std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(Cache::open(config).is_err());
    }
}
