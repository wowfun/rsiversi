use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use notify::EventKind;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use rsi_meta_plugin::sdk::{Host, Plugin};
use rsi_meta_plugin::{
    EVENT_CANCEL, EVENT_END, Frame, FrameBody, LifecyclePhase, OP_CANCEL, OP_CREDIT, OP_HALF_CLOSE,
    OP_OPEN, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE,
};
use rsi_meta_plugin::{Lane, PostFrameOutcome};
use serde::Deserialize;
use serde_json::{Value, json};

const WORKER_QUEUE_CAPACITY: usize = 256;
const MAX_PENDING_EVENTS: usize = 256;
const MAX_STREAM_CREDIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeConfig {
    #[serde(default)]
    recursive: bool,
}

#[derive(Debug, Deserialize)]
struct OpenRequest {
    path: PathBuf,
    consumer: String,
    sequence: u64,
}

#[derive(Debug)]
struct Candidate {
    generation: u64,
    config: NativeConfig,
}

#[derive(Debug)]
struct Active {
    generation: u64,
    config: NativeConfig,
}

#[derive(Debug)]
struct WatchStream {
    path: PathBuf,
    fingerprint: PathFingerprint,
    output_credit: u64,
    pending: VecDeque<Value>,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PathFingerprint {
    exists: bool,
    is_file: bool,
    is_directory: bool,
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedPath {
    path: PathBuf,
    includes_descendants: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WatchRoot {
    path: PathBuf,
    recursive: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NativeWatchPlan {
    observed_paths: Vec<ObservedPath>,
    roots: Vec<WatchRoot>,
}

struct WatchRegistration {
    _watcher: RecommendedWatcher,
    path: PathBuf,
    recursive: bool,
    plan: NativeWatchPlan,
}

impl WatchStream {
    fn enqueue(&mut self, event: Value) {
        if self.pending.len() < MAX_PENDING_EVENTS {
            self.pending.push_back(event);
        } else {
            self.overflowed = true;
        }
    }
}

type SharedStreams = Arc<Mutex<BTreeMap<String, WatchStream>>>;
// The worker replaces registrations after path topology changes. Keeping the
// table shared avoids waiting for a later host callback to repair a dead watch.
type SharedWatchers = Arc<Mutex<BTreeMap<String, WatchRegistration>>>;

enum WorkerMessage {
    Event {
        subscription_id: String,
        watched_path: PathBuf,
        result: notify::Result<Event>,
    },
    Stop,
}

#[derive(Clone)]
struct WorkerIngress {
    sender: mpsc::SyncSender<WorkerMessage>,
    overflowed: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EnqueueOutcome {
    Accepted,
    Overflowed,
    Closed,
}

impl WorkerIngress {
    fn bounded(capacity: usize) -> (Self, mpsc::Receiver<WorkerMessage>) {
        assert!(capacity > 0, "worker queue capacity must be non-zero");
        let (sender, receiver) = mpsc::sync_channel(capacity);
        (
            Self {
                sender,
                overflowed: Arc::new(AtomicBool::new(false)),
                stopping: Arc::new(AtomicBool::new(false)),
            },
            receiver,
        )
    }

    fn try_event(&self, event: WorkerMessage) -> EnqueueOutcome {
        debug_assert!(matches!(event, WorkerMessage::Event { .. }));
        match self.sender.try_send(event) {
            Ok(()) => EnqueueOutcome::Accepted,
            Err(mpsc::TrySendError::Full(_)) => {
                self.overflowed.store(true, Ordering::Release);
                EnqueueOutcome::Overflowed
            }
            Err(mpsc::TrySendError::Disconnected(_)) => EnqueueOutcome::Closed,
        }
    }

    fn request_stop(&self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self.sender.try_send(WorkerMessage::Stop);
    }

    fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    fn take_overflow(&self) -> bool {
        self.overflowed.swap(false, Ordering::AcqRel)
    }

    fn restore_overflow(&self) {
        self.overflowed.store(true, Ordering::Release);
    }
}

struct NativeWatcher {
    host: Host,
    candidate: Option<Candidate>,
    active: Option<Active>,
    watchers: SharedWatchers,
    streams: SharedStreams,
    worker_ingress: Option<WorkerIngress>,
    worker: Option<JoinHandle<()>>,
    pending_terminals: VecDeque<Frame>,
    pending_retired: Option<u64>,
}

#[derive(Debug)]
struct WatchError(&'static str);

impl fmt::Display for WatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl NativeWatcher {
    fn flush_terminals(&mut self) -> Result<(), WatchError> {
        while let Some(frame) = self.pending_terminals.front().cloned() {
            match post_outcome(&self.host, Lane::Data, &frame)? {
                PostFrameOutcome::Accepted => {
                    self.pending_terminals.pop_front();
                }
                PostFrameOutcome::WouldBlock => return Ok(()),
                PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                    return Err(WatchError("host closed"));
                }
            }
        }
        Ok(())
    }

    fn flush_retired(&mut self) -> Result<(), WatchError> {
        if !self.pending_terminals.is_empty() {
            return Ok(());
        }
        let Some(generation) = self.pending_retired else {
            return Ok(());
        };
        match post_outcome(
            &self.host,
            Lane::Control,
            &Frame::lifecycle(LifecyclePhase::Retired, generation, None),
        )? {
            PostFrameOutcome::Accepted => {
                self.pending_retired = None;
                Ok(())
            }
            PostFrameOutcome::WouldBlock => Ok(()),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                Err(WatchError("host closed"))
            }
        }
    }

    fn tick(&mut self, _tick: u64) -> Result<(), WatchError> {
        self.flush_terminals()?;
        if self.pending_retired.is_some() {
            return self.flush_retired();
        }
        let mut streams = self
            .streams
            .lock()
            .map_err(|_| WatchError("watch stream lock poisoned"))?;
        for (request_id, stream) in &mut *streams {
            flush_stream(&self.host, request_id, stream)?;
        }
        Ok(())
    }

    fn open(&mut self, request_id: &str, payload: Value) -> Result<(), WatchError> {
        let request: OpenRequest =
            serde_json::from_value(payload).map_err(|_| WatchError("invalid stream open"))?;
        if request.consumer.is_empty() || request.sequence != 0 {
            return Err(WatchError("invalid stream open"));
        }
        let recursive = self
            .active
            .as_ref()
            .ok_or(WatchError("watcher is not committed"))?
            .config
            .recursive;
        if self
            .watchers
            .lock()
            .map_err(|_| WatchError("watch registration lock poisoned"))?
            .contains_key(request_id)
        {
            return Err(WatchError("duplicate stream open"));
        }
        let ingress = self
            .worker_ingress
            .as_ref()
            .ok_or(WatchError("watch worker stopped"))?
            .clone();
        fs::metadata(&request.path).map_err(|_| WatchError("inspect watch path"))?;

        {
            let mut streams = self
                .streams
                .lock()
                .map_err(|_| WatchError("watch stream lock poisoned"))?;
            if streams.contains_key(request_id) {
                return Err(WatchError("duplicate stream open"));
            }
            let mut stream = WatchStream {
                path: request.path.clone(),
                fingerprint: path_fingerprint(&request.path),
                output_credit: 0,
                pending: VecDeque::new(),
                overflowed: false,
            };
            stream.enqueue(json!({
                "type": "ready",
                "path": request.path,
                "backend": "native",
            }));
            streams.insert(request_id.to_owned(), stream);
        }
        let registration =
            match create_watch_registration(request_id, request.path, recursive, ingress) {
                Ok(registration) => registration,
                Err(error) => {
                    self.streams
                        .lock()
                        .map_err(|_| WatchError("watch stream lock poisoned"))?
                        .remove(request_id);
                    return Err(error);
                }
            };
        self.watchers
            .lock()
            .map_err(|_| WatchError("watch registration lock poisoned"))?
            .insert(request_id.to_owned(), registration);

        // The first fingerprint is captured before the OS watcher exists. A
        // second snapshot after registration closes that setup gap: changes
        // before registration are found here, while later changes are covered
        // by the watcher callback.
        let mut streams = self
            .streams
            .lock()
            .map_err(|_| WatchError("watch stream lock poisoned"))?;
        let stream = streams
            .get_mut(request_id)
            .ok_or(WatchError("watch stream disappeared during open"))?;
        let current = path_fingerprint(&stream.path);
        if let Some(change) = fingerprint_change(&stream.fingerprint, &current) {
            stream.fingerprint = current;
            stream.enqueue(json!({
                "type": "changed",
                "path": stream.path,
                "change": change,
                "backend": "native",
            }));
        }
        Ok(())
    }

    fn grant_credit(&self, request_id: &str, payload: &Value) -> Result<(), WatchError> {
        let bytes = payload
            .get("bytes")
            .and_then(Value::as_u64)
            .ok_or(WatchError("credit bytes missing"))?;
        let mut streams = self
            .streams
            .lock()
            .map_err(|_| WatchError("watch stream lock poisoned"))?;
        let stream = streams
            .get_mut(request_id)
            .ok_or(WatchError("unknown stream"))?;
        stream.output_credit = stream
            .output_credit
            .checked_add(bytes)
            .filter(|credit| *credit <= MAX_STREAM_CREDIT)
            .ok_or(WatchError("credit overflow"))?;
        flush_stream(&self.host, request_id, stream)
    }

    fn finish(
        &mut self,
        request_id: &str,
        event: &'static str,
        payload: Value,
    ) -> Result<(), WatchError> {
        let removed = self
            .streams
            .lock()
            .map_err(|_| WatchError("watch stream lock poisoned"))?
            .remove(request_id);
        if removed.is_none() {
            return Err(WatchError("unknown stream"));
        }
        self.watchers
            .lock()
            .map_err(|_| WatchError("watch registration lock poisoned"))?
            .remove(request_id);
        self.pending_terminals.push_back(Frame::service_event(
            Some(request_id.to_owned()),
            "fs.watch",
            event,
            payload,
        ));
        self.flush_terminals()
    }

    fn stop_worker(&mut self) -> Result<(), WatchError> {
        self.watchers
            .lock()
            .map_err(|_| WatchError("watch registration lock poisoned"))?
            .clear();
        if let Some(ingress) = self.worker_ingress.take() {
            ingress.request_stop();
        }
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| WatchError("watch worker panicked"))?;
        }
        Ok(())
    }
}

impl Plugin for NativeWatcher {
    type Error = WatchError;

    fn create(host: Host) -> Result<Self, Self::Error> {
        let (worker_ingress, receiver) = WorkerIngress::bounded(WORKER_QUEUE_CAPACITY);
        let streams = Arc::new(Mutex::new(BTreeMap::new()));
        let watchers = Arc::new(Mutex::new(BTreeMap::new()));
        let worker_host = host;
        let worker_signals = worker_ingress.clone();
        let worker_streams = streams.clone();
        let worker_watchers = watchers.clone();
        let worker = thread::Builder::new()
            .name("rsi-meta-fs-watch-native".to_owned())
            .spawn(move || {
                worker_loop(
                    worker_host,
                    receiver,
                    worker_signals,
                    worker_streams,
                    worker_watchers,
                );
            })
            .map_err(|_| WatchError("start watch worker"))?;
        Ok(Self {
            host,
            candidate: None,
            active: None,
            watchers,
            streams,
            worker_ingress: Some(worker_ingress),
            worker: Some(worker),
            pending_terminals: VecDeque::new(),
            pending_retired: None,
        })
    }

    #[allow(clippy::too_many_lines)] // One exhaustive watcher protocol-state transition table.
    fn on_frame(&mut self, lane: Lane, payload: &[u8]) -> Result<(), Self::Error> {
        let frame = Frame::decode(payload).map_err(|_| WatchError("invalid frame"))?;
        match frame.body {
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Prepare,
                generation,
                config: Some(config),
            } if lane == Lane::Control => {
                let config = serde_json::from_value(config)
                    .map_err(|_| WatchError("invalid native watcher config"))?;
                self.candidate = Some(Candidate { generation, config });
                if let Err(error) = post(
                    &self.host,
                    Lane::Control,
                    &Frame::lifecycle(LifecyclePhase::Prepared, generation, None),
                ) {
                    self.candidate = None;
                    return Err(error);
                }
                Ok(())
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Abort,
                generation,
                ..
            } if lane == Lane::Control => {
                if self
                    .candidate
                    .as_ref()
                    .is_some_and(|candidate| candidate.generation == generation)
                {
                    self.candidate = None;
                }
                Ok(())
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Committed,
                generation,
                ..
            } if lane == Lane::Control => {
                let candidate = self
                    .candidate
                    .take()
                    .filter(|candidate| candidate.generation == generation)
                    .ok_or(WatchError("generation was not prepared"))?;
                self.active = Some(Active {
                    generation,
                    config: candidate.config,
                });
                Ok(())
            }
            FrameBody::ServiceRequest {
                request_id,
                service,
                operation,
                payload,
            } if lane == Lane::Data && service == "fs.watch" => match operation.as_str() {
                OP_OPEN => self.open(&request_id, payload),
                OP_CREDIT => self.grant_credit(&request_id, &payload),
                OP_HALF_CLOSE => self.finish(&request_id, EVENT_END, json!({})),
                OP_CANCEL => self.finish(&request_id, EVENT_CANCEL, payload),
                _ => Err(WatchError("unknown stream operation")),
            },
            FrameBody::ServiceEvent {
                service,
                event,
                payload,
                ..
            } if matches!(lane, Lane::Control | Lane::Data)
                && service == RUNTIME_TICK_SERVICE
                && event == RUNTIME_TICK_EVENT =>
            {
                let tick = payload
                    .get("tick")
                    .and_then(Value::as_u64)
                    .ok_or(WatchError("tick must be a u64"))?;
                self.tick(tick)
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Retire,
                generation,
                ..
            } if lane == Lane::Control
                && self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.generation == generation) =>
            {
                self.watchers
                    .lock()
                    .map_err(|_| WatchError("watch registration lock poisoned"))?
                    .clear();
                let streams = std::mem::take(
                    &mut *self
                        .streams
                        .lock()
                        .map_err(|_| WatchError("watch stream lock poisoned"))?,
                );
                for request_id in streams.keys() {
                    self.pending_terminals.push_back(Frame::service_event(
                        Some(request_id.clone()),
                        "fs.watch",
                        EVENT_CANCEL,
                        json!({"reason": "provider_retired"}),
                    ));
                }
                self.active = None;
                self.pending_retired = Some(generation);
                self.flush_terminals()?;
                self.flush_retired()
            }
            _ => Err(WatchError("frame rejected in current lifecycle state")),
        }
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        self.active = None;
        self.pending_retired = None;
        self.pending_terminals.clear();
        self.stop_worker()?;
        self.streams
            .lock()
            .map_err(|_| WatchError("watch stream lock poisoned"))?
            .clear();
        Ok(())
    }
}

#[allow(clippy::needless_pass_by_value)] // The worker thread owns all channel and shared-state handles.
fn worker_loop(
    host: Host,
    receiver: mpsc::Receiver<WorkerMessage>,
    ingress: WorkerIngress,
    streams: SharedStreams,
    watchers: SharedWatchers,
) {
    while let Ok(message) = receiver.recv() {
        if ingress.is_stopping() || matches!(&message, WorkerMessage::Stop) {
            break;
        }
        match message {
            WorkerMessage::Event {
                subscription_id,
                watched_path,
                result: Ok(event),
            } => {
                // Publish the change only after its new target/root is watched,
                // so receiving the frame is also a re-registration gate.
                let _ = reconcile_watch_registration(&subscription_id, &ingress, &watchers);
                if let Ok(mut streams) = streams.lock()
                    && let Some(stream) = streams.get_mut(&subscription_id)
                {
                    let fingerprint = path_fingerprint(&stream.path);
                    if let Some(change) = fingerprint_change(&stream.fingerprint, &fingerprint)
                        .or_else(|| notification_change(&event, &stream.path, &fingerprint))
                    {
                        stream.fingerprint = fingerprint;
                        stream.enqueue(json!({
                            "type": "changed",
                            "path": watched_path,
                            "change": change,
                            "backend": "native",
                        }));
                        let _ = flush_stream(&host, &subscription_id, stream);
                    }
                }
            }
            WorkerMessage::Event {
                subscription_id,
                watched_path,
                result: Err(error),
            } => {
                let _ = reconcile_watch_registration(&subscription_id, &ingress, &watchers);
                if let Ok(mut streams) = streams.lock()
                    && let Some(stream) = streams.get_mut(&subscription_id)
                {
                    stream.enqueue(json!({
                        "type": "error",
                        "path": watched_path,
                        "message": error.to_string(),
                        "backend": "native",
                    }));
                    let _ = flush_stream(&host, &subscription_id, stream);
                }
            }
            WorkerMessage::Stop => unreachable!("stop handled before event dispatch"),
        }
        if ingress.take_overflow() {
            let request_ids = watchers
                .lock()
                .map(|watchers| watchers.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            for request_id in request_ids {
                let _ = reconcile_watch_registration(&request_id, &ingress, &watchers);
            }
            if let Ok(mut streams) = streams.lock() {
                for (request_id, stream) in &mut *streams {
                    stream.enqueue(json!({
                        "type": "overflow",
                        "path": stream.path,
                        "reason": "worker_queue_full",
                        "backend": "native",
                    }));
                    if flush_stream(&host, request_id, stream).is_err() {
                        ingress.restore_overflow();
                    }
                }
            } else {
                ingress.restore_overflow();
            }
        }
    }
}

fn flush_stream(host: &Host, request_id: &str, stream: &mut WatchStream) -> Result<(), WatchError> {
    loop {
        if stream.pending.is_empty() && stream.overflowed {
            stream.overflowed = false;
            stream.pending.push_back(json!({
                "type": "overflow",
                "path": stream.path,
                "reason": "pending_event_queue_full",
                "backend": "native",
            }));
        }
        let Some(event) = stream.pending.front() else {
            return Ok(());
        };
        let payload = encode_data(event)?;
        let raw_bytes = payload.len() as u64;
        if stream.output_credit < raw_bytes {
            return Ok(());
        }
        let frame = Frame::service_data_event(request_id, "fs.watch", payload);
        match post_outcome(host, Lane::Data, &frame)? {
            PostFrameOutcome::Accepted => {
                stream.output_credit -= raw_bytes;
                stream.pending.pop_front();
            }
            PostFrameOutcome::WouldBlock => return Ok(()),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                return Err(WatchError("host closed"));
            }
        }
    }
}

fn encode_data(event: &Value) -> Result<Vec<u8>, WatchError> {
    serde_json::to_vec(event).map_err(|_| WatchError("encode watch event"))
}

fn fingerprint_change(
    previous: &PathFingerprint,
    current: &PathFingerprint,
) -> Option<&'static str> {
    if previous == current {
        return None;
    }
    match (previous.exists, current.exists) {
        (false, true) => Some("created"),
        (true, false) => Some("removed"),
        _ => Some("modified"),
    }
}

fn notification_change(
    event: &Event,
    target: &Path,
    current: &PathFingerprint,
) -> Option<&'static str> {
    match event.kind {
        EventKind::Modify(_)
            if current.exists && event_directly_affects(event, target, current) =>
        {
            Some("modified")
        }
        EventKind::Any
        | EventKind::Access(_)
        | EventKind::Create(_)
        | EventKind::Modify(_)
        | EventKind::Remove(_)
        | EventKind::Other => None,
    }
}

fn event_directly_affects(event: &Event, target: &Path, current: &PathFingerprint) -> bool {
    let target_paths = lexical_path_variants(target);
    let canonical = fs::canonicalize(target).ok();
    event.paths.iter().any(|event_path| {
        lexical_path_variants(event_path).iter().any(|event_path| {
            let matches = |candidate: &Path| {
                event_path == candidate
                    || (current.is_directory && event_path.starts_with(candidate))
            };
            target_paths.iter().any(|target| matches(target))
                || canonical.as_deref().is_some_and(matches)
        })
    })
}

#[cfg(test)]
fn event_affects_target(event: &Event, target: &Path, target_is_directory: bool) -> bool {
    event_affects_observed_paths(event, &observed_paths(target, target_is_directory))
}

fn event_affects_observed_paths(event: &Event, observed_paths: &[ObservedPath]) -> bool {
    if event.paths.is_empty() && event.kind == EventKind::Other {
        // notify maps inotify IN_Q_OVERFLOW to an unscoped Other event. The
        // only safe recovery is to rescan every subscription on this watcher.
        return true;
    }
    event.paths.iter().any(|event_path| {
        lexical_path_variants(event_path).iter().any(|event_path| {
            observed_paths.iter().any(|observed| {
                event_path.as_path() == observed.path.as_path()
                    || (observed.includes_descendants && event_path.starts_with(&observed.path))
                    || observed.path.starts_with(event_path)
            })
        })
    })
}

fn observed_paths(path: &Path, target_is_directory: bool) -> Vec<ObservedPath> {
    let resolved = fs::canonicalize(path).ok();
    let mut observed = lexical_path_variants(path)
        .into_iter()
        .map(|path| ObservedPath {
            path,
            includes_descendants: target_is_directory,
        })
        .collect::<Vec<_>>();
    if let Some(resolved) = resolved {
        let candidate = ObservedPath {
            path: resolved,
            includes_descendants: target_is_directory,
        };
        if let Some(existing) = observed
            .iter_mut()
            .find(|existing| existing.path == candidate.path)
        {
            existing.includes_descendants |= candidate.includes_descendants;
        } else {
            observed.push(candidate);
        }
    }
    observed
}

fn lexical_path_variants(path: &Path) -> Vec<PathBuf> {
    let absolute = absolute_lexical_path(path).unwrap_or_else(|| path.to_path_buf());
    let mut variants = vec![absolute.clone()];
    if let Some(normalized) = normalized_parent_path(&absolute)
        && normalized != absolute
    {
        variants.push(normalized);
    }
    variants
}

fn normalized_parent_path(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    parent.ancestors().find_map(|ancestor| {
        let canonical = fs::canonicalize(ancestor).ok()?;
        let suffix = path.strip_prefix(ancestor).ok()?;
        Some(canonical.join(suffix))
    })
}

fn absolute_lexical_path(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        std::env::current_dir()
            .ok()
            .map(|current| current.join(path))
    }
}

fn nearest_existing_directory(path: &Path) -> Option<PathBuf> {
    path.ancestors().find_map(|ancestor| {
        fs::metadata(ancestor)
            .ok()
            .filter(std::fs::Metadata::is_dir)
            .and_then(|_| fs::canonicalize(ancestor).ok())
    })
}

fn insert_root(roots: &mut BTreeMap<PathBuf, bool>, path: PathBuf, recursive: bool) {
    roots
        .entry(path)
        .and_modify(|current| *current |= recursive)
        .or_insert(recursive);
}

fn insert_topology_root(roots: &mut BTreeMap<PathBuf, bool>, root: &Path) {
    // One stable ancestor observes deletion and recreation of the immediate
    // root. Never broaden an individual file watch to the filesystem root.
    let Some(parent) = root.parent().filter(|parent| parent.parent().is_some()) else {
        return;
    };
    if let Some(parent) = nearest_existing_directory(parent) {
        insert_root(roots, parent, false);
    }
}

fn native_watch_plan(path: &Path, recursive: bool) -> Result<NativeWatchPlan, WatchError> {
    let metadata = fs::metadata(path).ok();
    let target_is_directory = metadata.as_ref().is_some_and(fs::Metadata::is_dir);
    let observed_paths = observed_paths(path, target_is_directory);
    let mut roots = BTreeMap::<PathBuf, bool>::new();
    let requested =
        absolute_lexical_path(path).ok_or(WatchError("resolve requested watch path"))?;
    let requested_parent = requested
        .parent()
        .ok_or(WatchError("watch path has no parent"))?;
    if let Some(parent) = nearest_existing_directory(requested_parent) {
        insert_root(&mut roots, parent.clone(), false);
        insert_topology_root(&mut roots, &parent);
    }
    if let Ok(resolved) = fs::canonicalize(path) {
        let target_root = if target_is_directory {
            resolved
        } else {
            resolved
                .parent()
                .ok_or(WatchError("watch file has no parent"))?
                .to_path_buf()
        };
        insert_root(
            &mut roots,
            target_root.clone(),
            target_is_directory && recursive,
        );
        insert_topology_root(&mut roots, &target_root);
    }
    if roots.is_empty() {
        return Err(WatchError("watch path has no observable ancestor"));
    }
    Ok(NativeWatchPlan {
        observed_paths,
        roots: roots
            .into_iter()
            .map(|(path, recursive)| WatchRoot { path, recursive })
            .collect(),
    })
}

fn create_watch_registration(
    request_id: &str,
    path: PathBuf,
    recursive: bool,
    ingress: WorkerIngress,
) -> Result<WatchRegistration, WatchError> {
    let plan = native_watch_plan(&path, recursive)?;
    create_watch_registration_with_plan(request_id, path, recursive, ingress, plan)
}

fn create_watch_registration_with_plan(
    request_id: &str,
    path: PathBuf,
    recursive: bool,
    ingress: WorkerIngress,
    plan: NativeWatchPlan,
) -> Result<WatchRegistration, WatchError> {
    let callback_id = request_id.to_owned();
    let callback_path = path.clone();
    let observed_paths = plan.observed_paths.clone();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
        if result
            .as_ref()
            .is_ok_and(|event| !event_affects_observed_paths(event, &observed_paths))
        {
            return;
        }
        let _ = ingress.try_event(WorkerMessage::Event {
            subscription_id: callback_id.clone(),
            watched_path: callback_path.clone(),
            result,
        });
    })
    .map_err(|_| WatchError("create native watcher"))?;
    for root in &plan.roots {
        watcher
            .watch(
                &root.path,
                if root.recursive {
                    RecursiveMode::Recursive
                } else {
                    RecursiveMode::NonRecursive
                },
            )
            .map_err(|_| WatchError("register native watch"))?;
    }
    Ok(WatchRegistration {
        _watcher: watcher,
        path,
        recursive,
        plan,
    })
}

fn reconcile_watch_registration(
    request_id: &str,
    ingress: &WorkerIngress,
    watchers: &SharedWatchers,
) -> Result<(), WatchError> {
    let (path, recursive, previous_plan) = {
        let watchers = watchers
            .lock()
            .map_err(|_| WatchError("watch registration lock poisoned"))?;
        let Some(registration) = watchers.get(request_id) else {
            return Ok(());
        };
        (
            registration.path.clone(),
            registration.recursive,
            registration.plan.clone(),
        )
    };
    let plan = native_watch_plan(&path, recursive)?;
    if plan == previous_plan {
        return Ok(());
    }
    // Register first, then cut over the map entry. The original watcher remains
    // active until every replacement root has been installed successfully.
    let replacement =
        create_watch_registration_with_plan(request_id, path, recursive, ingress.clone(), plan)?;
    let mut watchers = watchers
        .lock()
        .map_err(|_| WatchError("watch registration lock poisoned"))?;
    if watchers
        .get(request_id)
        .is_some_and(|registration| registration.plan == previous_plan)
    {
        watchers.insert(request_id.to_owned(), replacement);
    }
    Ok(())
}

fn path_fingerprint(path: &std::path::Path) -> PathFingerprint {
    let Ok(metadata) = fs::metadata(path) else {
        return PathFingerprint {
            exists: false,
            is_file: false,
            is_directory: false,
            len: 0,
            modified: None,
            #[cfg(unix)]
            device: 0,
            #[cfg(unix)]
            inode: 0,
        };
    };
    PathFingerprint {
        exists: true,
        is_file: metadata.is_file(),
        is_directory: metadata.is_dir(),
        len: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    }
}

fn post(host: &Host, lane: Lane, frame: &Frame) -> Result<(), WatchError> {
    match post_outcome(host, lane, frame)? {
        PostFrameOutcome::Accepted => Ok(()),
        PostFrameOutcome::WouldBlock => Err(WatchError("host backpressure")),
        PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => Err(WatchError("host closed")),
    }
}

fn post_outcome(host: &Host, lane: Lane, frame: &Frame) -> Result<PostFrameOutcome, WatchError> {
    let bytes = frame.encode().map_err(|_| WatchError("encode frame"))?;
    host.post_frame(lane, &bytes)
        .map_err(|_| WatchError("host unavailable"))
}

rsi_meta_plugin::export_plugin!(NativeWatcher);

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, mpsc};
    use std::time::Duration;

    use notify::event::RemoveKind;

    use super::*;

    fn event(index: usize) -> WorkerMessage {
        WorkerMessage::Event {
            subscription_id: format!("watch-{index}"),
            watched_path: PathBuf::from(format!("/tmp/watch-{index}")),
            result: Ok(Event::new(EventKind::Any)),
        }
    }

    fn file_fingerprint(exists: bool, len: u64) -> PathFingerprint {
        PathFingerprint {
            exists,
            is_file: exists,
            is_directory: false,
            len,
            modified: None,
            #[cfg(unix)]
            device: u64::from(exists),
            #[cfg(unix)]
            inode: u64::from(exists),
        }
    }

    #[test]
    fn callback_ingress_is_bounded_nonblocking_and_reports_burst_overflow() {
        let (ingress, receiver) = WorkerIngress::bounded(1);
        assert_eq!(ingress.try_event(event(0)), EnqueueOutcome::Accepted);

        let start = Arc::new(Barrier::new(2));
        let thread_start = start.clone();
        let thread_ingress = ingress.clone();
        let (completed, completion) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            thread_start.wait();
            for index in 1..=10_000 {
                assert_eq!(
                    thread_ingress.try_event(event(index)),
                    EnqueueOutcome::Overflowed
                );
            }
            completed.send(()).unwrap();
        });
        start.wait();
        completion
            .recv_timeout(Duration::from_secs(1))
            .expect("notify callback path must never wait for worker capacity");

        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerMessage::Event { .. })
        ));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(ingress.take_overflow(), "saturated burst must be signalled");
        assert!(!ingress.take_overflow(), "overflow is coalesced once");
    }

    #[test]
    fn change_labels_follow_fingerprint_transitions_not_delayed_notify_kinds() {
        let missing = file_fingerprint(false, 0);
        let original = file_fingerprint(true, 6);
        let recreated = file_fingerprint(true, 9);

        assert_eq!(fingerprint_change(&original, &missing), Some("removed"));
        assert_eq!(fingerprint_change(&missing, &recreated), Some("created"));
        assert_eq!(fingerprint_change(&original, &recreated), Some("modified"));
        assert_eq!(fingerprint_change(&recreated, &recreated), None);
    }

    #[test]
    fn modify_notification_recovers_only_an_existing_same_fingerprint_target() {
        let existing = file_fingerprint(true, 6);
        let missing = file_fingerprint(false, 0);
        let target = PathBuf::from("/tmp/rsi-meta.toml");
        let modified =
            Event::new(EventKind::Modify(notify::event::ModifyKind::Any)).add_path(target.clone());

        assert_eq!(
            notification_change(&modified, &target, &existing),
            Some("modified")
        );
        assert_eq!(notification_change(&modified, &target, &missing), None);
        assert_eq!(
            notification_change(
                &Event::new(EventKind::Remove(notify::event::RemoveKind::Any))
                    .add_path(target.clone()),
                &target,
                &missing,
            ),
            None
        );
        let parent_modified = Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
            .add_path(PathBuf::from("/tmp"));
        assert_eq!(
            notification_change(&parent_modified, &target, &existing),
            None,
            "an ancestor metadata notification must not fabricate a file change"
        );
    }

    #[test]
    fn parent_directory_events_are_filtered_to_the_requested_file() {
        let target = PathBuf::from("/tmp/rsi-meta.toml");
        let neighbor = Event::new(EventKind::Any).add_path(PathBuf::from("/tmp/unrelated.toml"));
        assert!(!event_affects_target(&neighbor, &target, false));

        let atomic_rename = Event::new(EventKind::Any)
            .add_path(PathBuf::from("/tmp/rsi-meta.toml.replace"))
            .add_path(target.clone());
        assert!(event_affects_target(&atomic_rename, &target, false));
    }

    #[test]
    fn unscoped_other_event_forces_a_rescan_after_kernel_overflow() {
        let target = PathBuf::from("/tmp/rsi-meta.toml");
        let overflow = Event::new(EventKind::Other);
        assert!(event_affects_target(&overflow, &target, false));
    }

    #[test]
    fn removing_a_watched_parent_affects_its_requested_file() {
        let target = PathBuf::from("/tmp/rsi-meta-parent/rsi-meta.toml");
        let removed_parent = Event::new(EventKind::Remove(RemoveKind::Folder))
            .add_path(PathBuf::from("/tmp/rsi-meta-parent"));
        assert!(event_affects_target(&removed_parent, &target, false));
    }

    #[cfg(unix)]
    #[test]
    fn target_events_affect_a_requested_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().unwrap();
        let physical_root = temp.path().join("physical");
        let alias_root = temp.path().join("alias");
        fs::create_dir(&physical_root).unwrap();
        symlink(&physical_root, &alias_root).unwrap();
        let target_dir = alias_root.join("target");
        let link_dir = alias_root.join("link");
        fs::create_dir_all(&target_dir).unwrap();
        fs::create_dir_all(&link_dir).unwrap();
        let target = target_dir.join("rsi-meta.toml");
        let link = link_dir.join("rsi-meta.toml");
        fs::write(&target, b"before").unwrap();
        symlink(&target, &link).unwrap();

        let target_event = Event::new(EventKind::Any).add_path(target);
        assert!(event_affects_target(&target_event, &link, false));

        let missing = alias_root.join("missing-parent").join("rsi-meta.toml");
        let recreated_parent =
            Event::new(EventKind::Any).add_path(physical_root.join("missing-parent"));
        assert!(event_affects_target(&recreated_parent, &missing, false));
    }
}
