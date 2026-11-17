use super::{CaptureStatus, Command, QUEUED_BYTES, SpillLocalConfig, State, io_error, native};
use rsi_files_native_fs::Directory;
use rsi_spill::{MAXIMUM_SPILL_BYTES, SpillLocator, SpillPage, SpillRef, SpillScope};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read as _, Seek as _, Write as _},
    path::Path,
    sync::{Arc, Mutex, atomic::Ordering, mpsc},
    time::{Duration, Instant, SystemTime},
};

const SPACE_SAMPLE_BYTES: u64 = 1024 * 1024;
const SCAN_INTERVAL: Duration = Duration::from_hours(1);
const SCAN_ENTRIES: usize = 32;
const SCAN_SLICE: Duration = Duration::from_millis(2);
const COMMANDS_PER_SLICE: usize = 64;
mod cleanup;
mod scan;

pub(super) struct Worker {
    config: SpillLocalConfig,
    directory: Directory,
    lease: File,
    #[cfg(windows)]
    _namespace: rsi_files_native_fs::NamespaceLease,
    state: Arc<State>,
    active: BTreeMap<Arc<SpillLocator>, Active>,
    remaining_space: u64,
    since_sample: u64,
    scan: Option<scan::Scan>,
    cleanup: Option<(
        cleanup::Cleanup,
        tokio::sync::oneshot::Sender<rsi_spill::Result<bool>>,
    )>,
}
struct Active {
    storage: Option<(Directory, File)>,
    bytes: u64,
    status: Arc<Mutex<CaptureStatus>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.lease.unlock();
    }
}
impl Worker {
    pub(super) fn open(config: SpillLocalConfig, state: Arc<State>) -> io::Result<Self> {
        let directory =
            rsi_files_native_fs::create_absolute_directory_no_follow(&config.directory)?;
        native::private_directory(&directory)?;
        #[cfg(windows)]
        let namespace = directory.pin_namespace()?;
        let lease = match native::create_file(&directory, Path::new(".writer")) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                native::open_file(&directory, Path::new(".writer"))?
            }
            Err(error) => return Err(error),
        };
        if lease.metadata()?.len() != 0 {
            return Err(io::Error::other("spill lease must be empty"));
        }
        lease
            .try_lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let mut worker = Self {
            config,
            directory,
            lease,
            #[cfg(windows)]
            _namespace: namespace,
            state,
            active: BTreeMap::new(),
            remaining_space: 0,
            since_sample: SPACE_SAMPLE_BYTES,
            scan: None,
            cleanup: None,
        };
        worker.start_scan(SystemTime::now(), true);
        Ok(worker)
    }
    pub(super) fn run(mut self, receiver: &mpsc::Receiver<Command>) {
        let mut next_scan = Instant::now() + SCAN_INTERVAL;
        let mut next_reap = Instant::now() + Duration::from_secs(1);
        'running: while !self.state.closed.load(Ordering::Acquire) {
            let now = Instant::now();
            if now >= next_scan && self.scan.is_none() {
                self.start_scan(SystemTime::now(), false);
                next_scan = Instant::now() + SCAN_INTERVAL;
            }
            if now >= next_reap {
                self.reap_abandoned();
                next_reap = Instant::now() + Duration::from_secs(1);
            }
            for _ in 0..COMMANDS_PER_SLICE {
                if self.state.closed.load(Ordering::Acquire) {
                    break 'running;
                }
                match receiver.try_recv() {
                    Ok(Command::Stop) | Err(mpsc::TryRecvError::Disconnected) => break 'running,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Ok(command) => self.command(command),
                }
            }
            if self.cleanup.is_some() {
                self.cleanup_slice();
                continue;
            }
            if self.scan.is_some() {
                self.scan_slice();
                if self.scan.is_none() {
                    next_scan = Instant::now() + SCAN_INTERVAL;
                }
                continue;
            }
            let deadline = if self.active.is_empty() {
                next_scan
            } else {
                next_scan.min(next_reap)
            };
            match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Ok(command) => self.command(command),
            }
        }
        for (locator, active) in std::mem::take(&mut self.active) {
            discard(&locator, active);
        }
    }
    fn command(&mut self, command: Command) {
        match command {
            Command::Stop => {}
            #[cfg(test)]
            Command::Block { entered, release } => {
                let _ = entered.send(());
                let _ = release.recv();
            }
            Command::Begin { locator, status } => {
                if let Err(_error) = self.begin(locator, status.clone()) {
                    *status.lock().unwrap() = CaptureStatus::Unavailable;
                }
            }
            Command::Write {
                locator,
                bytes,
                _permit,
            } => self.write(&locator, &bytes),
            Command::Finish { locator, done } => {
                self.finish(&locator);
                let _ = done.send(());
            }
            Command::Read {
                locator,
                offset,
                limit,
                done,
            } => {
                if !done.is_closed() {
                    let _ = done.send(self.read(locator, offset, limit).map_err(io_error));
                }
            }
            Command::Cleanup {
                scope,
                remove,
                done,
            } => {
                if let Some((active, _)) = &self.cleanup {
                    let result = if !remove && active.removing(&scope) {
                        Ok(false)
                    } else {
                        Err(rsi_spill::SpillError::Capacity)
                    };
                    let _ = done.send(result);
                } else if remove || !done.is_closed() {
                    match cleanup::Cleanup::open(self, &scope, remove) {
                        Ok(Some(cursor)) => self.cleanup = Some((cursor, done)),
                        Ok(None) => {
                            let _ = done.send(Ok(true));
                        }
                        Err(error) => {
                            let _ = done.send(Err(io_error(error)));
                        }
                    }
                }
            }
        }
    }
    fn sample_space(&mut self) -> io::Result<()> {
        self.remaining_space = native::free_bytes(&self.directory, &self.config.directory)?;
        self.since_sample = 0;
        self.admit_space()
    }
    fn admit_space(&self) -> io::Result<()> {
        let pending = (QUEUED_BYTES - self.state.bytes.available_permits()) as u64;
        if self
            .remaining_space
            .checked_sub(pending)
            .is_none_or(|free| free < self.config.minimum_free_bytes)
        {
            return Err(io::Error::other("spill free-space floor reached"));
        }
        Ok(())
    }
    fn begin(
        &mut self,
        locator: Arc<SpillLocator>,
        status: Arc<Mutex<CaptureStatus>>,
    ) -> io::Result<()> {
        if !self.state.admits(locator.scope_name())
            || !matches!(*status.lock().unwrap(), CaptureStatus::Pending)
        {
            return Err(io::Error::other("capture scope is fenced"));
        }
        self.active.insert(
            locator,
            Active {
                storage: None,
                bytes: 0,
                status,
            },
        );
        Ok(())
    }
    fn capture_directory(&self, scope: &str, create: bool) -> io::Result<Directory> {
        let directory = native::scope_directory(&self.directory, scope, create)?;
        match directory.child_kind(Path::new(".purged")) {
            Ok(_) => {
                self.state.fenced.lock().unwrap().insert(scope.into());
                return Err(io::Error::other("scope cleanup is pending"));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(directory)
    }
    fn write(&mut self, locator: &SpillLocator, bytes: &[u8]) {
        if bytes.is_empty() || !self.active.contains_key(locator) {
            return;
        }
        let result = (|| {
            let active = &self.active[locator];
            if !self.state.admits(locator.scope_name())
                || !matches!(*active.status.lock().unwrap(), CaptureStatus::Pending)
            {
                return Err(io::Error::other("capture was abandoned"));
            }
            let next = active.bytes + bytes.len() as u64;
            if next > self.config.maximum_object_bytes {
                return Err(io::Error::other("complete output exceeds its bound"));
            }
            if self.since_sample >= SPACE_SAMPLE_BYTES || active.storage.is_none() {
                self.sample_space()?;
            } else {
                self.admit_space()?;
            }
            if self.active[locator].storage.is_none() {
                let directory = self.capture_directory(locator.scope_name(), true)?;
                let file = native::create_file(&directory, &part(locator))?;
                self.active.get_mut(locator).unwrap().storage = Some((directory, file));
            }
            let active = self.active.get_mut(locator).unwrap();
            active
                .storage
                .as_mut()
                .expect("materialized capture")
                .1
                .write_all(bytes)?;
            active.bytes = next;
            Ok::<_, io::Error>(())
        })();
        if result.is_ok() {
            self.remaining_space = self.remaining_space.saturating_sub(bytes.len() as u64);
            self.since_sample += bytes.len() as u64;
        } else {
            discard(locator, self.active.remove(locator).unwrap());
        }
    }
    fn finish(&mut self, locator: &SpillLocator) {
        let Some(mut active) = self.active.remove(locator) else {
            return;
        };
        if active
            .storage
            .as_mut()
            .is_none_or(|(_, file)| file.flush().is_err())
        {
            discard(locator, active);
            return;
        }
        let Active {
            storage,
            bytes,
            status,
        } = active;
        let (directory, file) = storage.expect("nonempty capture");
        drop(file);
        let partial = part(locator);
        let complete = complete(locator);
        let created = native::publish(&directory, &partial, &complete).is_ok();
        if created && native::remove(&directory, &partial).is_ok() {
            // Filesystem work happens before this short publication/fence critical
            // section. The worker serializes cleanup with these namespace effects.
            let fences = self.state.fenced.lock().unwrap();
            let mut state = status.lock().unwrap();
            if !self.state.closed.load(Ordering::Acquire)
                && !fences.contains(locator.scope_name())
                && matches!(*state, CaptureStatus::Pending)
            {
                *state = CaptureStatus::Complete(SpillRef {
                    locator: locator.clone(),
                    bytes,
                });
                return;
            }
        }
        *status.lock().unwrap() = CaptureStatus::Unavailable;
        if created {
            let _ = native::remove(&directory, &complete);
        }
        let _ = native::remove(&directory, &partial);
    }
    fn read(&self, locator: SpillLocator, offset: u64, limit: usize) -> io::Result<SpillPage> {
        if !self.state.admits(locator.scope_name()) {
            return Err(io::Error::other("scope is fenced"));
        }
        let directory = self.capture_directory(locator.scope_name(), false)?;
        let mut file = native::open_file(&directory, &complete(&locator))?;
        let total_bytes = file.metadata()?.len();
        if total_bytes > MAXIMUM_SPILL_BYTES {
            return Err(io::Error::other("oversized spill object"));
        }
        if offset > total_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "offset exceeds output",
            ));
        }
        let length = usize::try_from((total_bytes - offset).min(limit as u64))
            .expect("read length is bounded by the usize limit");
        file.seek(io::SeekFrom::Start(offset))?;
        let mut bytes = vec![0; length];
        file.read_exact(&mut bytes)?;
        Ok(SpillPage {
            locator,
            offset,
            next_offset: offset + length as u64,
            total_bytes,
            bytes: bytes.into(),
        })
    }
    fn reap_abandoned(&mut self) {
        let abandoned: Vec<_> = self
            .active
            .iter()
            .filter(|(locator, active)| {
                !self.state.admits(locator.scope_name())
                    || !matches!(*active.status.lock().unwrap(), CaptureStatus::Pending)
            })
            .map(|(locator, _)| locator.clone())
            .collect();
        for locator in abandoned {
            discard(&locator, self.active.remove(&locator).unwrap());
        }
    }
    fn cleanup_slice(&mut self) {
        let Some((mut cursor, done)) = self.cleanup.take() else {
            return;
        };
        if !cursor.remove && done.is_closed() {
            return;
        }
        let start = Instant::now();
        for _ in 0..SCAN_ENTRIES {
            if self.state.closed.load(Ordering::Acquire) {
                return;
            }
            if !cursor.step(self) {
                let _ = done.send(cursor.finish(self).map_err(io_error));
                return;
            }
            if start.elapsed() >= SCAN_SLICE {
                break;
            }
        }
        self.cleanup = Some((cursor, done));
    }
    fn start_scan(&mut self, now: SystemTime, startup: bool) {
        self.scan = scan::Scan::new(self, now, startup).ok();
    }
    fn scan_slice(&mut self) {
        let Some(mut scan) = self.scan.take() else {
            return;
        };
        let start = Instant::now();
        for _ in 0..SCAN_ENTRIES {
            if self.state.closed.load(Ordering::Acquire) || !scan.step(self) {
                return;
            }
            if start.elapsed() >= SCAN_SLICE {
                break;
            }
        }
        self.scan = Some(scan);
    }
}
fn part(locator: &SpillLocator) -> std::path::PathBuf {
    format!("{}.part", locator.object_name()).into()
}
fn complete(locator: &SpillLocator) -> std::path::PathBuf {
    format!("{}.log", locator.object_name()).into()
}
fn generated(name: &str) -> bool {
    name.strip_suffix(".log")
        .or_else(|| name.strip_suffix(".part"))
        .is_some_and(|id| {
            id.len() == 32
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}
fn discard(locator: &SpillLocator, active: Active) {
    *active.status.lock().unwrap() = CaptureStatus::Unavailable;
    if let Some((directory, file)) = active.storage {
        drop(file);
        let _ = native::remove(&directory, &part(locator));
    }
}

#[cfg(test)]
mod tests;
