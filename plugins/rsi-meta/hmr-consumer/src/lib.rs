use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use rsi_meta_frame_contract::{
    DurableCommand, EVENT_CANCEL, EVENT_CREDIT, EVENT_DATA, EVENT_END, Frame, FrameBody,
    LifecyclePhase, OP_CANCEL, OP_CREDIT, OP_OPEN, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE,
};
use rsi_meta_plugin::sdk::{Host, Plugin};
use rsi_meta_plugin::{Lane, PostFrameOutcome};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const WATCH_STREAM_CREDIT: u64 = 16 * 1024 * 1024;
const MAX_WATCH_PATHS: usize = 4_096;
const MAX_PENDING_WATCH_ACTIONS: usize = MAX_WATCH_PATHS * 2;
const MAX_WATCH_PLAN_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PLUGIN_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_WATCH_CONTENT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_WATCH_PLAN_CONTENT_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HmrConfig {
    manifest_path: PathBuf,
    lock_path: PathBuf,
    watch_request_id: String,
}

#[derive(Debug)]
struct Candidate {
    generation: u64,
    config: HmrConfig,
    watch_plan: WatchPlan,
}

#[derive(Debug)]
struct WatchPlan {
    paths: Vec<PathBuf>,
    content_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WatchState {
    Opening,
    Crediting,
    Active,
    Cancelling,
}

#[derive(Debug)]
struct WatchSubscription {
    path: PathBuf,
    ready: bool,
    state: WatchState,
}

#[derive(Debug)]
struct Active {
    generation: u64,
    config: HmrConfig,
    subscriptions: BTreeMap<String, WatchSubscription>,
    request_by_path: BTreeMap<PathBuf, String>,
    pending: VecDeque<String>,
    pending_ids: BTreeSet<String>,
    next_watch: u64,
    dirty: bool,
    plan_stale: bool,
    desired_content_id: String,
    last_apply_tick: Option<u64>,
    apply_in_flight: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LockDocument {
    target: String,
    packages: Vec<LockedPackage>,
}

#[derive(Debug, Deserialize)]
struct LockedPackage {
    path: PathBuf,
    #[serde(default, rename = "config_schema_sha256")]
    _config_schema_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PackageDocument {
    config_schema: Option<PathBuf>,
    artifacts: Vec<PackageArtifact>,
}

#[derive(Debug, Deserialize)]
struct PackageArtifact {
    target: String,
    path: PathBuf,
}

struct HmrConsumer {
    host: Host,
    candidate: Option<Candidate>,
    active: Option<Active>,
    pending_retired: Option<u64>,
}

#[derive(Debug)]
struct HmrError(&'static str);

impl fmt::Display for HmrError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Active {
    fn enqueue(&mut self, request_id: String) -> Result<(), HmrError> {
        if !self.pending_ids.insert(request_id.clone()) {
            return Ok(());
        }
        if self.pending.len() >= MAX_PENDING_WATCH_ACTIONS {
            self.pending_ids.remove(&request_id);
            return Err(HmrError("pending watch action limit exceeded"));
        }
        self.pending.push_back(request_id);
        Ok(())
    }

    fn enqueue_front(&mut self, request_id: String) -> Result<(), HmrError> {
        if !self.pending_ids.insert(request_id.clone()) {
            return Ok(());
        }
        if self.pending.len() >= MAX_PENDING_WATCH_ACTIONS {
            self.pending_ids.remove(&request_id);
            return Err(HmrError("pending watch action limit exceeded"));
        }
        self.pending.push_front(request_id);
        Ok(())
    }

    fn next_request_id(&mut self) -> Result<String, HmrError> {
        let ordinal = self.next_watch;
        self.next_watch = self
            .next_watch
            .checked_add(1)
            .ok_or(HmrError("watch request sequence overflow"))?;
        if ordinal == 0 {
            Ok(self.config.watch_request_id.clone())
        } else {
            Ok(format!("{}:{ordinal}", self.config.watch_request_id))
        }
    }
}

impl HmrConsumer {
    fn flush_retired(&mut self) -> Result<(), HmrError> {
        let Some(generation) = self.pending_retired else {
            return Ok(());
        };
        match self.post_outcome(
            Lane::Control,
            &Frame::lifecycle(LifecyclePhase::Retired, generation, None),
        )? {
            PostFrameOutcome::Accepted => {
                self.pending_retired = None;
                Ok(())
            }
            PostFrameOutcome::WouldBlock => Ok(()),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => Err(HmrError("host closed")),
        }
    }

    fn post_outcome(&self, lane: Lane, frame: &Frame) -> Result<PostFrameOutcome, HmrError> {
        let bytes = frame.encode().map_err(|_| HmrError("encode frame"))?;
        self.host
            .post_frame(lane, &bytes)
            .map_err(|_| HmrError("host unavailable"))
    }

    fn post_required(&self, lane: Lane, frame: &Frame) -> Result<(), HmrError> {
        match self.post_outcome(lane, frame)? {
            PostFrameOutcome::Accepted => Ok(()),
            PostFrameOutcome::WouldBlock => Err(HmrError("host backpressure")),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => Err(HmrError("host closed")),
        }
    }

    fn apply_watch_plan(&mut self, plan: WatchPlan) -> Result<(), HmrError> {
        let active = self
            .active
            .as_mut()
            .ok_or(HmrError("consumer is not committed"))?;
        let desired = plan.paths.into_iter().collect::<BTreeSet<_>>();
        let removed = active
            .request_by_path
            .keys()
            .filter(|path| !desired.contains(*path))
            .cloned()
            .collect::<Vec<_>>();
        for path in removed {
            let Some(request_id) = active.request_by_path.remove(&path) else {
                continue;
            };
            let state = active
                .subscriptions
                .get(&request_id)
                .map(|subscription| subscription.state);
            match state {
                Some(WatchState::Opening) => {
                    active.subscriptions.remove(&request_id);
                    active.pending.retain(|pending| pending != &request_id);
                    active.pending_ids.remove(&request_id);
                }
                Some(WatchState::Crediting | WatchState::Active) => {
                    if let Some(subscription) = active.subscriptions.get_mut(&request_id) {
                        subscription.ready = false;
                        subscription.state = WatchState::Cancelling;
                    }
                    active.enqueue(request_id)?;
                }
                Some(WatchState::Cancelling) | None => {}
            }
        }
        for path in desired {
            if active.request_by_path.contains_key(&path) {
                continue;
            }
            let request_id = active.next_request_id()?;
            active
                .request_by_path
                .insert(path.clone(), request_id.clone());
            active.subscriptions.insert(
                request_id.clone(),
                WatchSubscription {
                    path,
                    ready: false,
                    state: WatchState::Opening,
                },
            );
            active.enqueue(request_id)?;
        }
        active.desired_content_id = plan.content_id;
        active.plan_stale = false;
        Ok(())
    }

    fn refresh_watch_plan(&mut self) -> Result<(), HmrError> {
        let Some(config) = self.active.as_ref().map(|active| active.config.clone()) else {
            return Err(HmrError("consumer is not committed"));
        };
        if let Ok(plan) = derive_watch_plan(&config) {
            self.apply_watch_plan(plan)
        } else {
            if let Some(active) = self.active.as_mut() {
                active.plan_stale = true;
                active.dirty = true;
            }
            Ok(())
        }
    }

    fn drain_pending(&mut self) -> Result<(), HmrError> {
        loop {
            let Some((request_id, state, path)) = ({
                let active = self
                    .active
                    .as_mut()
                    .ok_or(HmrError("consumer is not committed"))?;
                let mut next = None;
                while let Some(request_id) = active.pending.pop_front() {
                    active.pending_ids.remove(&request_id);
                    if let Some(subscription) = active.subscriptions.get(&request_id) {
                        next = Some((request_id, subscription.state, subscription.path.clone()));
                        break;
                    }
                }
                next
            }) else {
                return Ok(());
            };
            let frame = match state {
                WatchState::Opening => Frame::service_request(
                    request_id.clone(),
                    "fs.watch",
                    OP_OPEN,
                    json!({
                        "consumer": "hmr.watch-consumer",
                        "sequence": 0,
                        "path": path,
                    }),
                ),
                WatchState::Crediting => Frame::service_request(
                    request_id.clone(),
                    "fs.watch",
                    OP_CREDIT,
                    json!({"bytes": WATCH_STREAM_CREDIT}),
                ),
                WatchState::Cancelling => Frame::service_request(
                    request_id.clone(),
                    "fs.watch",
                    OP_CANCEL,
                    json!({"reason": "watch_plan_changed"}),
                ),
                WatchState::Active => continue,
            };
            match self.post_outcome(Lane::Data, &frame)? {
                PostFrameOutcome::Accepted => {
                    let active = self
                        .active
                        .as_mut()
                        .ok_or(HmrError("consumer is not committed"))?;
                    match state {
                        WatchState::Opening => {
                            if let Some(subscription) = active.subscriptions.get_mut(&request_id) {
                                subscription.state = WatchState::Crediting;
                            }
                            active.enqueue_front(request_id)?;
                        }
                        WatchState::Crediting => {
                            if let Some(subscription) = active.subscriptions.get_mut(&request_id) {
                                subscription.state = WatchState::Active;
                            }
                        }
                        WatchState::Cancelling => {
                            active.subscriptions.remove(&request_id);
                        }
                        WatchState::Active => {}
                    }
                }
                PostFrameOutcome::WouldBlock => {
                    self.active
                        .as_mut()
                        .ok_or(HmrError("consumer is not committed"))?
                        .enqueue_front(request_id)?;
                    return Ok(());
                }
                PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                    return Err(HmrError("host closed"));
                }
            }
        }
    }

    fn flush_dirty(&mut self, tick: u64) -> Result<(), HmrError> {
        let (command_id, command) = {
            let active = self
                .active
                .as_ref()
                .ok_or(HmrError("consumer is not committed"))?;
            if !active.dirty
                || active.plan_stale
                || active.apply_in_flight.is_some()
                || active.last_apply_tick == Some(tick)
            {
                return Ok(());
            }
            (
                format!("hmr-v0-{}", active.desired_content_id),
                DurableCommand::ApplyManifestPath {
                    manifest_path: active.config.manifest_path.clone(),
                    lock_path: active.config.lock_path.clone(),
                },
            )
        };
        match self.post_outcome(Lane::Control, &Frame::durable_command(command_id, command))? {
            PostFrameOutcome::WouldBlock => return Ok(()),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                return Err(HmrError("host closed"));
            }
            PostFrameOutcome::Accepted => {}
        }
        let active = self
            .active
            .as_mut()
            .ok_or(HmrError("consumer is not committed"))?;
        active.apply_in_flight = Some(format!("hmr-v0-{}", active.desired_content_id));
        active.last_apply_tick = Some(tick);
        Ok(())
    }
}

fn derive_watch_plan(config: &HmrConfig) -> Result<WatchPlan, HmrError> {
    let manifest_path = canonicalize_following_symlinks(&config.manifest_path)?;
    let lock_path = canonicalize_following_symlinks(&config.lock_path)?;
    let _: toml::Value = read_toml(&manifest_path, MAX_WATCH_PLAN_DOCUMENT_BYTES)?;
    let lock: LockDocument = read_toml(&lock_path, MAX_WATCH_PLAN_DOCUMENT_BYTES)?;
    let lock_base = lock_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut seen = BTreeSet::new();
    seen.insert(manifest_path);
    seen.insert(lock_path);

    for locked in lock.packages {
        let package_manifest = canonicalize_no_follow(&resolve_from(&lock_base, &locked.path))?;
        if !seen.insert(package_manifest.clone()) {
            continue;
        }
        let package: PackageDocument = read_toml(&package_manifest, MAX_PLUGIN_MANIFEST_BYTES)?;
        let package_base = package_manifest
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if let Some(schema) = package.config_schema {
            seen.insert(canonicalize_no_follow(&resolve_from(
                package_base,
                &schema,
            ))?);
        }
        let mut selected = package
            .artifacts
            .into_iter()
            .filter(|artifact| artifact.target == lock.target);
        let artifact = selected
            .next()
            .ok_or(HmrError("package has no artifact for locked target"))?;
        if selected.next().is_some() {
            return Err(HmrError(
                "package has duplicate artifacts for locked target",
            ));
        }
        seen.insert(canonicalize_no_follow(&resolve_from(
            package_base,
            &artifact.path,
        ))?);
    }
    if seen.len() > MAX_WATCH_PATHS {
        return Err(HmrError("watch path limit exceeded"));
    }
    let paths = seen.into_iter().collect::<Vec<_>>();
    let content_id = watch_content_id(&paths)?;
    Ok(WatchPlan { paths, content_id })
}

fn read_toml<T: serde::de::DeserializeOwned>(
    path: &Path,
    maximum_bytes: usize,
) -> Result<T, HmrError> {
    let bytes = read_bounded_regular(path, maximum_bytes)?;
    let source =
        std::str::from_utf8(&bytes).map_err(|_| HmrError("watch-plan input is not UTF-8"))?;
    toml::from_str(source).map_err(|_| HmrError("parse watch-plan input"))
}

fn resolve_from(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}

fn canonicalize_no_follow(path: &Path) -> Result<PathBuf, HmrError> {
    open_regular_no_follow(path)?;
    fs::canonicalize(path).map_err(|_| HmrError("canonicalize watch-plan input"))
}

fn canonicalize_following_symlinks(path: &Path) -> Result<PathBuf, HmrError> {
    open_regular_follow(path)?;
    fs::canonicalize(path).map_err(|_| HmrError("canonicalize watch-plan input"))
}

fn watch_content_id(paths: &[PathBuf]) -> Result<String, HmrError> {
    let mut hash = Sha256::new();
    hash.update(b"rsi-meta.hmr-watch-plan.v0\0");
    let mut total_bytes = 0_u64;
    for path in paths {
        let path_bytes = path.to_string_lossy();
        let mut file = open_regular_no_follow(path)?;
        let length = file
            .metadata()
            .map_err(|_| HmrError("inspect desired watch content"))?
            .len();
        total_bytes = total_bytes
            .checked_add(length)
            .filter(|total| {
                length <= MAX_WATCH_CONTENT_BYTES && *total <= MAX_WATCH_PLAN_CONTENT_BYTES
            })
            .ok_or(HmrError("desired watch content exceeds size limit"))?;
        hash.update((path_bytes.len() as u64).to_be_bytes());
        hash.update(path_bytes.as_bytes());
        hash.update(length.to_be_bytes());
        let mut observed = 0_u64;
        let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
        loop {
            let read = file
                .by_ref()
                .take(length.saturating_add(1).saturating_sub(observed))
                .read(&mut buffer)
                .map_err(|_| HmrError("read desired watch content"))?;
            if read == 0 {
                break;
            }
            observed = observed
                .checked_add(read as u64)
                .ok_or(HmrError("desired watch content length overflow"))?;
            hash.update(&buffer[..read]);
        }
        if observed != length {
            return Err(HmrError("desired watch content changed while hashing"));
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(unix)]
fn open_regular_no_follow(path: &Path) -> Result<fs::File, HmrError> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| HmrError("open regular watch-plan input"))?;
    if !file
        .metadata()
        .map_err(|_| HmrError("inspect watch-plan input"))?
        .file_type()
        .is_file()
    {
        return Err(HmrError("watch-plan input is not a regular file"));
    }
    Ok(file)
}

#[cfg(unix)]
fn open_regular_follow(path: &Path) -> Result<fs::File, HmrError> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| HmrError("open regular watch-plan input"))?;
    if !file
        .metadata()
        .map_err(|_| HmrError("inspect watch-plan input"))?
        .file_type()
        .is_file()
    {
        return Err(HmrError("watch-plan input is not a regular file"));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_regular_no_follow(path: &Path) -> Result<fs::File, HmrError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| HmrError("inspect watch-plan input"))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(HmrError(
            "watch-plan input is not a regular non-symlink file",
        ));
    }
    fs::File::open(path).map_err(|_| HmrError("open regular watch-plan input"))
}

#[cfg(not(unix))]
fn open_regular_follow(path: &Path) -> Result<fs::File, HmrError> {
    let file = fs::File::open(path).map_err(|_| HmrError("open regular watch-plan input"))?;
    if !file
        .metadata()
        .map_err(|_| HmrError("inspect watch-plan input"))?
        .file_type()
        .is_file()
    {
        return Err(HmrError("watch-plan input is not a regular file"));
    }
    Ok(file)
}

fn read_bounded_regular(path: &Path, maximum_bytes: usize) -> Result<Vec<u8>, HmrError> {
    let mut file = open_regular_no_follow(path)?;
    let length = file
        .metadata()
        .map_err(|_| HmrError("inspect watch-plan input"))?
        .len();
    if length > maximum_bytes as u64 {
        return Err(HmrError("watch-plan input exceeds size limit"));
    }
    let capacity = usize::try_from(length).map_err(|_| HmrError("watch-plan input too large"))?;
    let mut bytes = Vec::with_capacity(capacity);
    file.by_ref()
        .take(maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| HmrError("read watch-plan input"))?;
    if bytes.len() > maximum_bytes {
        return Err(HmrError("watch-plan input exceeds size limit"));
    }
    Ok(bytes)
}

impl Plugin for HmrConsumer {
    type Error = HmrError;

    fn create(host: Host) -> Result<Self, Self::Error> {
        Ok(Self {
            host,
            candidate: None,
            active: None,
            pending_retired: None,
        })
    }

    #[allow(clippy::too_many_lines)] // One exhaustive HMR protocol-state transition table.
    fn on_frame(&mut self, lane: Lane, payload: &[u8]) -> Result<(), Self::Error> {
        let frame = Frame::decode(payload).map_err(|_| HmrError("invalid frame"))?;
        match frame.body {
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Prepare,
                generation,
                config: Some(config),
            } if lane == Lane::Control => {
                let config =
                    serde_json::from_value(config).map_err(|_| HmrError("invalid HMR config"))?;
                let watch_plan = derive_watch_plan(&config)?;
                self.candidate = Some(Candidate {
                    generation,
                    config,
                    watch_plan,
                });
                if let Err(error) = self.post_required(
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
                    .ok_or(HmrError("generation was not prepared"))?;
                self.active = Some(Active {
                    generation,
                    config: candidate.config,
                    subscriptions: BTreeMap::new(),
                    request_by_path: BTreeMap::new(),
                    pending: VecDeque::new(),
                    pending_ids: BTreeSet::new(),
                    next_watch: 0,
                    dirty: false,
                    plan_stale: false,
                    desired_content_id: candidate.watch_plan.content_id.clone(),
                    last_apply_tick: None,
                    apply_in_flight: None,
                });
                self.apply_watch_plan(candidate.watch_plan)?;
                self.drain_pending()
            }
            FrameBody::ServiceEvent {
                request_id: Some(request_id),
                service,
                event,
                payload,
            } if lane == Lane::Data && service == "fs.watch" && event == EVENT_DATA => {
                let payload = decode_data(&payload)?;
                let refresh = {
                    let active = self
                        .active
                        .as_mut()
                        .ok_or(HmrError("consumer is not committed"))?;
                    let Some(subscription) = active.subscriptions.get_mut(&request_id) else {
                        return self.drain_pending();
                    };
                    match payload.get("type").and_then(Value::as_str) {
                        Some("ready") => {
                            if payload_path(&payload)? != subscription.path {
                                return Err(HmrError("mismatched watch-ready event"));
                            }
                            if subscription.state != WatchState::Active {
                                return Err(HmrError("watch ready before subscription credit"));
                            }
                            subscription.ready = true;
                            false
                        }
                        Some("changed") => {
                            if payload_path(&payload)? != subscription.path {
                                return self.drain_pending();
                            }
                            if !subscription.ready {
                                return Err(HmrError("watch changed before ready"));
                            }
                            active.dirty = true;
                            active.plan_stale = true;
                            true
                        }
                        Some("overflow" | "error") => {
                            active.dirty = true;
                            active.plan_stale = true;
                            true
                        }
                        _ => return Err(HmrError("unknown watch DATA event")),
                    }
                };
                if refresh {
                    self.refresh_watch_plan()?;
                }
                self.drain_pending()
            }
            FrameBody::ServiceEvent {
                request_id: Some(request_id),
                service,
                event,
                ..
            } if lane == Lane::Data && service == "fs.watch" && event == EVENT_CREDIT => {
                if self
                    .active
                    .as_ref()
                    .is_none_or(|active| !active.subscriptions.contains_key(&request_id))
                {
                    return self.drain_pending();
                }
                self.drain_pending()
            }
            FrameBody::ServiceEvent {
                request_id: Some(request_id),
                service,
                event,
                ..
            } if lane == Lane::Data
                && service == "fs.watch"
                && matches!(event.as_str(), EVENT_END | EVENT_CANCEL) =>
            {
                let refresh = {
                    let active = self
                        .active
                        .as_mut()
                        .ok_or(HmrError("consumer is not committed"))?;
                    let Some(subscription) = active.subscriptions.remove(&request_id) else {
                        return self.drain_pending();
                    };
                    if active.request_by_path.get(&subscription.path) == Some(&request_id) {
                        active.request_by_path.remove(&subscription.path);
                    }
                    active.pending.retain(|pending| pending != &request_id);
                    active.pending_ids.remove(&request_id);
                    if subscription.state == WatchState::Cancelling {
                        false
                    } else {
                        active.dirty = true;
                        active.plan_stale = true;
                        true
                    }
                };
                if refresh {
                    self.refresh_watch_plan()?;
                }
                self.drain_pending()
            }
            FrameBody::ServiceEvent {
                request_id: Some(request_id),
                service,
                event,
                ..
            } if lane == Lane::Control && service == "control.apply-manifest" => {
                let active = self
                    .active
                    .as_mut()
                    .ok_or(HmrError("consumer is not committed"))?;
                if active.apply_in_flight.as_deref() != Some(&request_id) {
                    return Err(HmrError("unexpected apply result"));
                }
                active.apply_in_flight = None;
                match event.as_str() {
                    "applied" | "unchanged" | "restart_required" | "rejected" => {
                        active.dirty =
                            request_id != format!("hmr-v0-{}", active.desired_content_id);
                    }
                    "failed" => active.dirty = true,
                    _ => return Err(HmrError("unknown apply result")),
                }
                Ok(())
            }
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
                    .ok_or(HmrError("tick must be a u64"))?;
                if self.pending_retired.is_some() {
                    return self.flush_retired();
                }
                if self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.plan_stale || active.dirty)
                {
                    self.refresh_watch_plan()?;
                }
                self.drain_pending()?;
                self.flush_dirty(tick)
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
                let active = self.active.take().expect("matched active generation");
                for request_id in active.subscriptions.keys() {
                    let _ = self.post_outcome(
                        Lane::Data,
                        &Frame::service_request(
                            request_id.clone(),
                            "fs.watch",
                            OP_CANCEL,
                            json!({"reason": "consumer_retired"}),
                        ),
                    );
                }
                self.pending_retired = Some(generation);
                self.flush_retired()
            }
            _ => Err(HmrError("frame rejected in current lifecycle state")),
        }
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        self.active = None;
        self.candidate = None;
        self.pending_retired = None;
        Ok(())
    }
}

fn payload_path(payload: &Value) -> Result<PathBuf, HmrError> {
    payload
        .get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or(HmrError("watch event path missing"))
}

fn decode_data(payload: &Value) -> Result<Value, HmrError> {
    let bytes = payload
        .as_array()
        .ok_or(HmrError("watch DATA is not a byte array"))?
        .iter()
        .map(|byte| {
            byte.as_u64()
                .and_then(|byte| u8::try_from(byte).ok())
                .ok_or(HmrError("watch DATA contains a non-byte"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    serde_json::from_slice(&bytes).map_err(|_| HmrError("watch DATA JSON is invalid"))
}

rsi_meta_plugin::export_plugin!(HmrConsumer);
