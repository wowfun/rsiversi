use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::UNIX_EPOCH;

use rsi_meta_plugin::sdk::{Host, Plugin};
use rsi_meta_plugin::{
    EVENT_CANCEL, EVENT_END, Frame, FrameBody, LifecyclePhase, OP_CANCEL, OP_CREDIT, OP_HALF_CLOSE,
    OP_OPEN, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE,
};
use rsi_meta_plugin::{Lane, PostFrameOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MAX_PENDING_EVENTS: usize = 256;
const MAX_WATCH_STREAMS: usize = 128;
const WORKER_QUEUE_CAPACITY: usize = MAX_WATCH_STREAMS;
const MAX_STREAM_CREDIT: u64 = 16 * 1024 * 1024;
const MAX_HASHED_WATCH_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_HASHED_BYTES_PER_TICK: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PollingConfig {
    #[serde(default)]
    hash_contents: bool,
}

#[derive(Debug, Deserialize)]
struct OpenRequest {
    path: PathBuf,
    consumer: String,
    sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct Fingerprint {
    exists: bool,
    is_file: bool,
    len: u64,
    modified_unix_nanos: Option<u64>,
    content_sha256: Option<String>,
}

#[derive(Debug)]
struct WatchStream {
    identity: u64,
    path: PathBuf,
    fingerprint: Option<Fingerprint>,
    scan_pending: bool,
    deferred: bool,
    output_credit: u64,
    pending: VecDeque<Value>,
    overflowed: bool,
}

type SharedStreams = Arc<Mutex<BTreeMap<String, WatchStream>>>;

enum WorkerCommand {
    Scan {
        request_id: String,
        stream_identity: u64,
        tick: u64,
        hash_contents: bool,
    },
    Stop,
}

#[derive(Clone)]
struct WorkerIngress {
    sender: mpsc::SyncSender<WorkerCommand>,
    stopping: Arc<AtomicBool>,
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

#[derive(Debug)]
struct Candidate {
    generation: u64,
    config: PollingConfig,
}

#[derive(Debug)]
struct Active {
    generation: u64,
    config: PollingConfig,
}

struct PollingWatcher {
    host: Host,
    candidate: Option<Candidate>,
    active: Option<Active>,
    streams: SharedStreams,
    worker_ingress: WorkerIngress,
    worker: Option<JoinHandle<()>>,
    next_stream_identity: u64,
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

impl PollingWatcher {
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

    fn open(&mut self, request_id: &str, payload: Value) -> Result<(), WatchError> {
        let request: OpenRequest =
            serde_json::from_value(payload).map_err(|_| WatchError("invalid stream open"))?;
        if request.consumer.is_empty() || request.sequence != 0 {
            return Err(WatchError("invalid stream open"));
        }
        let hash_contents = self
            .active
            .as_ref()
            .ok_or(WatchError("watcher is not committed"))?
            .config
            .hash_contents;
        validate_watch_path(&request.path, hash_contents)?;
        let mut streams = self
            .streams
            .lock()
            .map_err(|_| WatchError("watch stream lock poisoned"))?;
        if streams.contains_key(request_id) || streams.len() >= MAX_WATCH_STREAMS {
            return Err(WatchError("invalid stream open"));
        }
        let stream_identity = self.next_stream_identity;
        self.next_stream_identity = self
            .next_stream_identity
            .checked_add(1)
            .ok_or(WatchError("watch stream identity exhausted"))?;
        streams.insert(
            request_id.to_owned(),
            WatchStream {
                identity: stream_identity,
                path: request.path.clone(),
                fingerprint: None,
                scan_pending: true,
                deferred: false,
                output_credit: 0,
                pending: VecDeque::new(),
                overflowed: false,
            },
        );
        drop(streams);
        if self
            .worker_ingress
            .sender
            .try_send(WorkerCommand::Scan {
                request_id: request_id.to_owned(),
                stream_identity,
                tick: 0,
                hash_contents,
            })
            .is_err()
        {
            self.streams
                .lock()
                .map_err(|_| WatchError("watch stream lock poisoned"))?
                .remove(request_id);
            return Err(WatchError("polling worker queue full"));
        }
        Ok(())
    }

    fn grant_credit(&mut self, request_id: &str, payload: &Value) -> Result<(), WatchError> {
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

    fn tick(&mut self, tick: u64) -> Result<(), WatchError> {
        self.flush_terminals()?;
        if self.pending_retired.is_some() {
            return self.flush_retired();
        }
        let hash_contents = self
            .active
            .as_ref()
            .ok_or(WatchError("watcher is not committed"))?
            .config
            .hash_contents;
        let mut streams = self
            .streams
            .lock()
            .map_err(|_| WatchError("watch stream lock poisoned"))?;
        for (request_id, stream) in &mut *streams {
            flush_stream(&self.host, request_id, stream)?;
        }
        let candidates = scan_candidates(&streams);
        for (request_id, stream_identity) in candidates {
            match self.worker_ingress.sender.try_send(WorkerCommand::Scan {
                request_id: request_id.clone(),
                stream_identity,
                tick,
                hash_contents,
            }) {
                Ok(()) => {
                    if let Some(stream) = streams.get_mut(&request_id)
                        && stream.identity == stream_identity
                    {
                        stream.scan_pending = true;
                    }
                }
                Err(mpsc::TrySendError::Full(_)) => break,
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(WatchError("polling worker closed"));
                }
            }
        }
        Ok(())
    }

    fn finish(
        &mut self,
        request_id: &str,
        event: &'static str,
        payload: Value,
    ) -> Result<(), WatchError> {
        if self
            .streams
            .lock()
            .map_err(|_| WatchError("watch stream lock poisoned"))?
            .remove(request_id)
            .is_none()
        {
            return Err(WatchError("unknown stream"));
        }
        self.pending_terminals.push_back(Frame::service_event(
            Some(request_id.to_owned()),
            "fs.watch",
            event,
            payload,
        ));
        self.flush_terminals()
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

impl Plugin for PollingWatcher {
    type Error = WatchError;

    fn create(host: Host) -> Result<Self, Self::Error> {
        let streams = Arc::new(Mutex::new(BTreeMap::new()));
        let (sender, receiver) = mpsc::sync_channel(WORKER_QUEUE_CAPACITY);
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_ingress = WorkerIngress { sender, stopping };
        let worker = thread::Builder::new()
            .name("rsi-meta-fs-watch-polling".to_owned())
            .spawn({
                let worker_host = host;
                let worker_streams = Arc::clone(&streams);
                let worker_ingress = worker_ingress.clone();
                move || {
                    polling_worker_loop(&worker_host, &receiver, &worker_ingress, &worker_streams);
                }
            })
            .map_err(|_| WatchError("start polling worker"))?;
        Ok(Self {
            host,
            candidate: None,
            active: None,
            streams,
            worker_ingress,
            worker: Some(worker),
            next_stream_identity: 1,
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
                    .map_err(|_| WatchError("invalid polling config"))?;
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
        self.worker_ingress.stopping.store(true, Ordering::Release);
        let _ = self.worker_ingress.sender.try_send(WorkerCommand::Stop);
        if self
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            return Err(WatchError("polling worker panicked"));
        }
        self.streams
            .lock()
            .map_err(|_| WatchError("watch stream lock poisoned"))?
            .clear();
        self.pending_terminals.clear();
        self.active = None;
        self.pending_retired = None;
        Ok(())
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

enum ScanOutcome {
    Complete(Fingerprint),
    Deferred,
    Failed(WatchError),
}

fn polling_worker_loop(
    host: &Host,
    receiver: &mpsc::Receiver<WorkerCommand>,
    ingress: &WorkerIngress,
    streams: &SharedStreams,
) {
    let mut budget_tick = None;
    let mut remaining = MAX_HASHED_BYTES_PER_TICK;
    while let Ok(command) = receiver.recv() {
        if ingress.stopping.load(Ordering::Acquire) || matches!(command, WorkerCommand::Stop) {
            break;
        }
        let WorkerCommand::Scan {
            request_id,
            stream_identity,
            tick,
            hash_contents,
        } = command
        else {
            unreachable!("stop handled before scan dispatch")
        };
        if budget_tick != Some(tick) {
            budget_tick = Some(tick);
            remaining = MAX_HASHED_BYTES_PER_TICK;
        }
        let Some(path) = streams.lock().ok().and_then(|streams| {
            streams
                .get(&request_id)
                .filter(|stream| stream.identity == stream_identity)
                .map(|stream| stream.path.clone())
        }) else {
            continue;
        };
        let outcome = fingerprint(&path, hash_contents, &ingress.stopping, &mut remaining);
        let Ok(mut streams) = streams.lock() else {
            break;
        };
        let Some(stream) = streams.get_mut(&request_id) else {
            continue;
        };
        if !apply_scan_outcome(stream, stream_identity, tick, outcome) {
            continue;
        }
        let _ = flush_stream(host, &request_id, stream);
    }
}

fn scan_candidates(streams: &BTreeMap<String, WatchStream>) -> Vec<(String, u64)> {
    streams
        .iter()
        .filter(|(_, stream)| stream.deferred && !stream.scan_pending)
        .chain(
            streams
                .iter()
                .filter(|(_, stream)| !stream.deferred && !stream.scan_pending),
        )
        .map(|(request_id, stream)| (request_id.clone(), stream.identity))
        .collect()
}

fn apply_scan_outcome(
    stream: &mut WatchStream,
    stream_identity: u64,
    tick: u64,
    outcome: ScanOutcome,
) -> bool {
    if stream.identity != stream_identity {
        return false;
    }
    stream.scan_pending = false;
    stream.deferred = matches!(&outcome, ScanOutcome::Deferred);
    match outcome {
        ScanOutcome::Complete(current) => match stream.fingerprint.replace(current.clone()) {
            None => stream.enqueue(json!({
                "type": "ready",
                "path": stream.path,
                "snapshot": current,
            })),
            Some(previous) if previous != current => {
                let change = match (previous.exists, current.exists) {
                    (false, true) => "created",
                    (true, false) => "removed",
                    _ => "modified",
                };
                stream.enqueue(json!({
                    "type": "changed",
                    "path": stream.path,
                    "change": change,
                    "tick": tick,
                    "previous": previous,
                    "current": current,
                }));
            }
            Some(_) => {}
        },
        ScanOutcome::Deferred => {}
        ScanOutcome::Failed(error) => stream.enqueue(json!({
            "type": "error",
            "path": stream.path,
            "message": error.to_string(),
            "backend": "polling",
            "tick": tick,
        })),
    }
    true
}

fn validate_watch_path(path: &Path, hash_contents: bool) -> Result<(), WatchError> {
    match fs::metadata(path) {
        Ok(metadata)
            if hash_contents
                && metadata.is_file()
                && metadata.len() > MAX_HASHED_WATCH_FILE_BYTES =>
        {
            Err(WatchError("watched file exceeds content hash limit"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(WatchError("read file metadata")),
    }
}

fn fingerprint(
    path: &Path,
    hash_contents: bool,
    stopping: &AtomicBool,
    remaining: &mut u64,
) -> ScanOutcome {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ScanOutcome::Complete(Fingerprint {
                exists: false,
                is_file: false,
                len: 0,
                modified_unix_nanos: None,
                content_sha256: None,
            });
        }
        Err(_) => return ScanOutcome::Failed(WatchError("read file metadata")),
    };
    let mut current = Fingerprint {
        exists: true,
        is_file: metadata.is_file(),
        len: metadata.len(),
        modified_unix_nanos: metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)),
        content_sha256: None,
    };
    if !hash_contents || !metadata.is_file() {
        return ScanOutcome::Complete(current);
    }
    if current.len > MAX_HASHED_WATCH_FILE_BYTES {
        return ScanOutcome::Failed(WatchError("watched file exceeds content hash limit"));
    }
    if current.len > *remaining {
        return ScanOutcome::Deferred;
    }
    *remaining -= current.len;
    let result = (|| {
        let mut file = open_watch_file(path)?;
        let metadata = file
            .metadata()
            .map_err(|_| WatchError("read file metadata"))?;
        if !metadata.file_type().is_file() {
            return Err(WatchError("watched path changed file type while hashing"));
        }
        let expected_length = metadata.len();
        if expected_length > MAX_HASHED_WATCH_FILE_BYTES {
            return Err(WatchError("watched file exceeds content hash limit"));
        }
        let mut observed_length = 0_u64;
        let mut digest = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
        loop {
            if stopping.load(Ordering::Acquire) {
                return Err(WatchError("polling worker stopping"));
            }
            let read = file
                .read(&mut buffer)
                .map_err(|_| WatchError("read watched file"))?;
            if read == 0 {
                break;
            }
            observed_length = observed_length
                .checked_add(read as u64)
                .filter(|length| *length <= MAX_HASHED_WATCH_FILE_BYTES)
                .ok_or(WatchError("watched file length overflow"))?;
            digest.update(&buffer[..read]);
        }
        if observed_length != expected_length {
            return Err(WatchError("watched file changed while hashing"));
        }
        current.content_sha256 = Some(hex::encode(digest.finalize()));
        Ok(current)
    })();
    match result {
        Ok(fingerprint) => ScanOutcome::Complete(fingerprint),
        Err(error) => ScanOutcome::Failed(error),
    }
}

#[cfg(unix)]
fn open_watch_file(path: &Path) -> Result<fs::File, WatchError> {
    use std::os::unix::fs::OpenOptionsExt;

    // fs.watch follows symlinks by contract. O_NONBLOCK still prevents a
    // regular-file-to-FIFO replacement from parking the polling thread before
    // the opened handle can be checked with fstat.
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| WatchError("open watched file"))
}

#[cfg(not(unix))]
fn open_watch_file(path: &Path) -> Result<fs::File, WatchError> {
    fs::File::open(path).map_err(|_| WatchError("open watched file"))
}

rsi_meta_plugin::export_plugin!(PollingWatcher);

#[cfg(test)]
mod tests {
    use super::*;

    fn test_stream(identity: u64, deferred: bool) -> WatchStream {
        WatchStream {
            identity,
            path: PathBuf::from("watched.bin"),
            fingerprint: None,
            scan_pending: false,
            deferred,
            output_credit: 0,
            pending: VecDeque::new(),
            overflowed: false,
        }
    }

    #[test]
    fn deferred_scans_are_scheduled_before_fresh_scans() {
        let streams = BTreeMap::from([
            ("a-fresh".to_owned(), test_stream(1, false)),
            ("z-deferred".to_owned(), test_stream(2, true)),
        ]);

        assert_eq!(
            scan_candidates(&streams),
            vec![("z-deferred".to_owned(), 2), ("a-fresh".to_owned(), 1)]
        );
    }

    #[test]
    fn stale_scan_result_cannot_mutate_a_reopened_request_id() {
        let mut reopened = test_stream(2, false);
        let stale = Fingerprint {
            exists: true,
            is_file: true,
            len: 5,
            modified_unix_nanos: Some(1),
            content_sha256: Some("stale".to_owned()),
        };

        assert!(!apply_scan_outcome(
            &mut reopened,
            1,
            7,
            ScanOutcome::Complete(stale),
        ));
        assert_eq!(reopened.fingerprint, None);
        assert!(!reopened.scan_pending);
        assert!(reopened.pending.is_empty());
    }

    #[test]
    fn content_hash_mode_rechecks_equal_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watched.bin");
        fs::write(&path, b"bravo").unwrap();
        let stopping = AtomicBool::new(false);
        let metadata = fs::metadata(&path).unwrap();
        let previous = Fingerprint {
            exists: true,
            is_file: true,
            len: metadata.len(),
            modified_unix_nanos: metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)),
            content_sha256: Some(hex::encode(Sha256::digest(b"alpha"))),
        };
        assert_eq!(previous.len, metadata.len());
        assert_eq!(
            previous.modified_unix_nanos,
            metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
        );

        let mut budget = MAX_HASHED_BYTES_PER_TICK;
        let ScanOutcome::Complete(current) = fingerprint(&path, true, &stopping, &mut budget)
        else {
            panic!("content fingerprint failed")
        };
        assert_eq!(
            current.content_sha256,
            Some(hex::encode(Sha256::digest(b"bravo")))
        );
        assert_eq!(budget, MAX_HASHED_BYTES_PER_TICK - 5);
    }

    #[test]
    fn content_scan_is_deferred_before_reading_when_tick_budget_is_exhausted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watched.bin");
        fs::write(&path, b"changed content").unwrap();
        let stopping = AtomicBool::new(false);
        let mut budget = 0;
        assert!(matches!(
            fingerprint(&path, true, &stopping, &mut budget),
            ScanOutcome::Deferred
        ));
        assert_eq!(budget, 0);
    }

    #[test]
    fn content_hashing_observes_shutdown_without_finishing_the_large_scan() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.bin");
        fs::File::create(&path)
            .unwrap()
            .set_len(MAX_HASHED_WATCH_FILE_BYTES)
            .unwrap();
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut budget = MAX_HASHED_BYTES_PER_TICK;
            sender
                .send(fingerprint(&path, true, &worker_stopping, &mut budget))
                .unwrap();
        });
        stopping.store(true, Ordering::Release);
        assert!(matches!(
            receiver.recv_timeout(std::time::Duration::from_secs(1)),
            Ok(ScanOutcome::Failed(WatchError("polling worker stopping")))
        ));
        worker.join().unwrap();
    }
}
