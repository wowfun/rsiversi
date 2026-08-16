use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rsi_meta_frame_contract::{
    EVENT_CANCEL, EVENT_DATA, EVENT_END, Frame, FrameBody, LifecyclePhase, OP_CANCEL, OP_CREDIT,
    OP_HALF_CLOSE, OP_OPEN, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE,
};
use rsi_meta_plugin::sdk::{Host, Plugin};
use rsi_meta_plugin::{Lane, PostFrameOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MAX_PENDING_EVENTS: usize = 256;
const MAX_STREAM_CREDIT: u64 = 16 * 1024 * 1024;
const MAX_HASHED_WATCH_FILE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PollingConfig {
    #[serde(default = "default_true")]
    hash_contents: bool,
}

const fn default_true() -> bool {
    true
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
    path: PathBuf,
    fingerprint: Fingerprint,
    output_credit: u64,
    pending: VecDeque<Value>,
    overflowed: bool,
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
    streams: BTreeMap<String, WatchStream>,
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

    fn open(&mut self, request_id: String, payload: Value) -> Result<(), WatchError> {
        let request: OpenRequest =
            serde_json::from_value(payload).map_err(|_| WatchError("invalid stream open"))?;
        if request.consumer.is_empty()
            || request.sequence != 0
            || self.streams.contains_key(&request_id)
        {
            return Err(WatchError("invalid stream open"));
        }
        let hash_contents = self
            .active
            .as_ref()
            .ok_or(WatchError("watcher is not committed"))?
            .config
            .hash_contents;
        let fingerprint = fingerprint(&request.path, hash_contents)?;
        let mut stream = WatchStream {
            path: request.path.clone(),
            fingerprint: fingerprint.clone(),
            output_credit: 0,
            pending: VecDeque::new(),
            overflowed: false,
        };
        stream.enqueue(json!({
            "type": "ready",
            "path": request.path,
            "snapshot": fingerprint,
        }));
        self.streams.insert(request_id, stream);
        Ok(())
    }

    fn grant_credit(&mut self, request_id: &str, payload: &Value) -> Result<(), WatchError> {
        let bytes = payload
            .get("bytes")
            .and_then(Value::as_u64)
            .ok_or(WatchError("credit bytes missing"))?;
        let stream = self
            .streams
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
        for (request_id, stream) in &mut self.streams {
            match fingerprint(&stream.path, hash_contents) {
                Ok(current) if current != stream.fingerprint => {
                    let change = match (stream.fingerprint.exists, current.exists) {
                        (false, true) => "created",
                        (true, false) => "removed",
                        _ => "modified",
                    };
                    stream.enqueue(json!({
                        "type": "changed",
                        "path": stream.path,
                        "change": change,
                        "tick": tick,
                        "previous": stream.fingerprint,
                        "current": current,
                    }));
                    stream.fingerprint = current;
                }
                Ok(_) => {}
                Err(error) => stream.enqueue(json!({
                    "type": "error",
                    "path": stream.path,
                    "message": error.to_string(),
                    "backend": "polling",
                    "tick": tick,
                })),
            }
            flush_stream(&self.host, request_id, stream)?;
        }
        Ok(())
    }

    fn finish(
        &mut self,
        request_id: &str,
        event: &'static str,
        payload: Value,
    ) -> Result<(), WatchError> {
        if self.streams.remove(request_id).is_none() {
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
        let encoded_bytes = serde_json::to_vec(&payload)
            .map_err(|_| WatchError("encode DATA payload"))?
            .len() as u64;
        if stream.output_credit < encoded_bytes {
            return Ok(());
        }
        let frame =
            Frame::service_event(Some(request_id.to_owned()), "fs.watch", EVENT_DATA, payload);
        match post_outcome(host, Lane::Data, &frame)? {
            PostFrameOutcome::Accepted => {
                stream.output_credit -= encoded_bytes;
                stream.pending.pop_front();
            }
            PostFrameOutcome::WouldBlock => return Ok(()),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                return Err(WatchError("host closed"));
            }
        }
    }
}

fn encode_data(event: &Value) -> Result<Value, WatchError> {
    let bytes = serde_json::to_vec(event).map_err(|_| WatchError("encode watch event"))?;
    Ok(Value::Array(bytes.into_iter().map(Value::from).collect()))
}

impl Plugin for PollingWatcher {
    type Error = WatchError;

    fn create(host: Host) -> Result<Self, Self::Error> {
        Ok(Self {
            host,
            candidate: None,
            active: None,
            streams: BTreeMap::new(),
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
                OP_OPEN => self.open(request_id, payload),
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
                let streams = std::mem::take(&mut self.streams);
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
        self.streams.clear();
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

fn fingerprint(path: &Path, hash_contents: bool) -> Result<Fingerprint, WatchError> {
    let mut metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Fingerprint {
                exists: false,
                is_file: false,
                len: 0,
                modified_unix_nanos: None,
                content_sha256: None,
            });
        }
        Err(_) => return Err(WatchError("read file metadata")),
    };
    let content_sha256 = if hash_contents && metadata.is_file() {
        let mut file = open_watch_file(path)?;
        metadata = file
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
        Some(hex::encode(digest.finalize()))
    } else {
        None
    };
    let modified_unix_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX));
    Ok(Fingerprint {
        exists: true,
        is_file: metadata.is_file(),
        len: metadata.len(),
        modified_unix_nanos,
        content_sha256,
    })
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
