use crate::{BrowserSession, NativeRuntime, OpenError, SessionPolicy};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rsi_agent_session_protocol::{SessionHeader, SessionId};
use rsi_media_protocol::{Media, MediaRef};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{OnceCell, Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(super) const MAXIMUM_SCREENSHOT_BYTES: usize = 4 * 1024 * 1024;
const MAXIMUM_IMPORTED_BYTES: u64 = 32 * 1024 * 1024;

fn identity() -> Result<String, String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|_| "browser entropy unavailable")?;
    Ok(hex::encode(bytes))
}
/// Trusted current Session authority; never serializable or supplied by page text.
#[derive(Clone, Debug)]
pub enum SessionAuthority {
    Human(Arc<rsi_session_protocol::SessionSourceLease>),
    Agent(Arc<rsi_agent_turn_protocol::AgentCallerAuthority>),
}
impl SessionAuthority {
    fn header(&self) -> &SessionHeader {
        match self {
            Self::Human(s) => s.header(),
            Self::Agent(a) => a.header(),
        }
    }
}

/// A live browser identity, separate from document and UI presentation versions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserBinding {
    pub service_epoch: String,
    pub browser_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionOperation {
    Status {},
    Open {
        policy: SessionPolicy,
        url: String,
    },
    Navigate {
        binding: BrowserBinding,
        url: String,
    },
    Observe {
        binding: BrowserBinding,
    },
    Click {
        binding: BrowserBinding,
        document_version: String,
        observation_id: String,
        node: String,
    },
    Fill {
        binding: BrowserBinding,
        document_version: String,
        observation_id: String,
        node: String,
        text: String,
    },
    Scroll {
        binding: BrowserBinding,
        direction: String,
    },
    Screenshot {
        binding: BrowserBinding,
    },
    Close {
        binding: BrowserBinding,
    },
}
impl SessionOperation {
    fn binding(&self) -> Option<&BrowserBinding> {
        match self {
            Self::Status {} | Self::Open { .. } => None,
            Self::Navigate { binding, .. }
            | Self::Observe { binding }
            | Self::Click { binding, .. }
            | Self::Fill { binding, .. }
            | Self::Scroll { binding, .. }
            | Self::Screenshot { binding }
            | Self::Close { binding } => Some(binding),
        }
    }
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(match self {
            Self::Navigate { .. } => 25,
            Self::Observe { .. } | Self::Screenshot { .. } => 15,
            Self::Click { .. } | Self::Fill { .. } | Self::Scroll { .. } => 10,
            Self::Open { .. } | Self::Status { .. } | Self::Close { .. } => return None,
        }))
    }
    /// # Errors
    /// Rejects malformed bindings and inputs exceeding operation bounds.
    pub fn validate(&self) -> Result<(), String> {
        if serde_json::to_vec(self)
            .map_err(|_| "invalid browser operation")?
            .len()
            > 128 * 1024
        {
            return Err("browser operation exceeds bound".into());
        }
        if let Some(binding) = self.binding() {
            for id in [&binding.service_epoch, &binding.browser_id] {
                if id.len() != 32
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err("invalid browser binding".into());
                }
            }
        }
        match self {
            Self::Open { policy, url } => {
                policy.navigate(url)?;
            }
            Self::Navigate { url, .. } if url.is_empty() || url.len() > 8192 => {
                return Err("navigation URL must contain 1..8192 UTF-8 bytes".into());
            }
            Self::Fill { text, .. } if text.len() > 16384 => {
                return Err("fill exceeds 16384 UTF-8 bytes".into());
            }
            Self::Scroll { direction, .. } if !matches!(direction.as_str(), "up" | "down") => {
                return Err("scroll must be one viewport up or down".into());
            }
            _ => {}
        }
        if let Self::Click {
            document_version,
            observation_id,
            node,
            ..
        }
        | Self::Fill {
            document_version,
            observation_id,
            node,
            ..
        } = self
        {
            let canonical = |s: &str, maximum: u64| {
                s.parse::<u64>()
                    .is_ok_and(|n| n > 0 && n <= maximum && n.to_string() == s)
            };
            if !canonical(document_version, u64::MAX)
                || !canonical(node, 256)
                || observation_id.len() != 32
                || !observation_id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err("invalid bounded node observation".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionResult {
    pub service_epoch: String,
    pub binding: Option<BrowserBinding>,
    pub state: String,
    pub result: Value,
    pub screenshot: Option<MediaRef>,
    pub imported_bytes: u64,
}
#[derive(Debug)]
struct Entry {
    binding: BrowserBinding,
    header: String,
    authority: SessionAuthority,
    policy: SessionPolicy,
    native: OnceCell<Arc<BrowserSession>>,
    ready: AtomicBool,
    stop: CancellationToken,
    launch_settled: CancellationToken,
    retirement: OnceCell<Result<(), String>>,
    admitted: Instant,
    idle: Mutex<Instant>,
    lanes: Arc<Semaphore>,
    active: Arc<Semaphore>,
    imported: AtomicU64,
    screenshot: Mutex<Option<MediaRef>>,
    snapshot: Mutex<Value>,
}
#[derive(Debug)]
pub struct SessionBrowser {
    epoch: String,
    runtime: Arc<NativeRuntime>,
    media: Arc<dyn Media>,
    entries: Mutex<BTreeMap<SessionId, Arc<Entry>>>,
    stop: CancellationToken,
    tasks: TaskTracker,
}
impl SessionAuthority {
    fn admit(&self) -> Result<rsi_execution::ExecutionOperation, String> {
        let header = self.header();
        if header.protection().is_some()
            || *header.coordinates().location() != rsi_execution::ExecutionLocation::Local
        {
            return Err("browser requires an unprotected Local native Session".into());
        }
        let execution = match self {
            Self::Human(s) => {
                if s.retiring().is_cancelled() {
                    return Err("Session authority retired".into());
                }
                Some(s.execution())
            }
            Self::Agent(a) => a.execution(),
        };
        execution
            .ok_or("browser requires current execution authority")?
            .admit()
            .map_err(|_| "browser authority withdrawn".into())
    }
}
impl SessionBrowser {
    /// # Errors
    /// Fails when a fresh Service epoch cannot be generated.
    pub fn new(runtime: Arc<NativeRuntime>, media: Arc<dyn Media>) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            epoch: identity()?,
            runtime,
            media,
            entries: Mutex::new(BTreeMap::new()),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
        }))
    }
    fn report(&self, entry: Option<&Entry>, result: Value) -> SessionResult {
        let mut result = result;
        if let Some(entry) = entry
            && result.get("snapshot").is_none()
        {
            result.as_object_mut().expect("typed result").insert(
                "snapshot".into(),
                entry.snapshot.lock().expect("browser snapshot").clone(),
            );
        }
        SessionResult {
            service_epoch: self.epoch.clone(),
            binding: entry.map(|e| e.binding.clone()),
            state: entry
                .map_or("closed", |e| {
                    if e.stop.is_cancelled() {
                        "retiring"
                    } else if !e.ready.load(Ordering::SeqCst) {
                        "opening"
                    } else {
                        "live"
                    }
                })
                .into(),
            result,
            screenshot: entry
                .and_then(|e| e.screenshot.lock().expect("browser screenshot").clone()),
            imported_bytes: entry.map_or(0, |e| e.imported.load(Ordering::SeqCst)),
        }
    }
    /// Keeps dispatched operations and process capacity owned after a waiter drops.
    /// # Errors
    /// Rejects withdrawn authority, invalid input and unavailable or uncertain work.
    /// # Panics
    /// Panics if an internal owner lock was poisoned by a prior panic.
    pub async fn call(
        self: &Arc<Self>,
        authority: SessionAuthority,
        operation: SessionOperation,
        cancellation: CancellationToken,
    ) -> Result<SessionResult, String> {
        operation.validate()?;
        let permission = authority.admit()?;
        if self.stop.is_cancelled() {
            return Err("browser service retired".into());
        }
        if matches!(operation, SessionOperation::Status {}) {
            let entries = self.entries.lock().expect("browser entries");
            let entry = entries.get(authority.header().session_id());
            return Ok(self.report(entry.map(Arc::as_ref),json!({"status":"completed","snapshot":entry.map(|e|e.snapshot.lock().expect("browser snapshot").clone())})));
        }
        let stop = cancellation.child_token();
        let guard = stop.clone().drop_guard();
        let owner = self.clone();
        let task = self.tasks.spawn(async move {
            let result = owner.execute(authority, operation, stop).await;
            drop(permission);
            result
        });
        let result = task.await.map_err(|_| "browser owner task failed")?;
        guard.disarm();
        result
    }
    #[expect(
        clippy::too_many_lines,
        reason = "One owner transition retains opening, mutation and retirement until actual settlement."
    )]
    async fn execute(
        self: &Arc<Self>,
        authority: SessionAuthority,
        operation: SessionOperation,
        stop: CancellationToken,
    ) -> Result<SessionResult, String> {
        let session = authority.header().session_id().clone();
        let header = authority
            .header()
            .fingerprint()
            .map_err(|e| e.to_string())?;
        if let SessionOperation::Open { policy, url } = &operation {
            self.runtime.require_ready()?;
            let entry = {
                let mut entries = self.entries.lock().expect("browser entries");
                if entries.contains_key(&session) {
                    return Ok(self.report(
                        entries.get(&session).map(Arc::as_ref),
                        json!({"status":"not_started","reason":"already_open"}),
                    ));
                }
                if entries.len() == 2 {
                    return Ok(
                        self.report(None, json!({"status":"not_started","reason":"capacity"}))
                    );
                }
                let entry = Arc::new(Entry {
                    binding: BrowserBinding {
                        service_epoch: self.epoch.clone(),
                        browser_id: identity()?,
                    },
                    header,
                    authority: authority.clone(),
                    policy: policy.clone(),
                    native: OnceCell::new(),
                    ready: AtomicBool::new(false),
                    stop: self.stop.child_token(),
                    launch_settled: CancellationToken::new(),
                    retirement: OnceCell::new(),
                    admitted: Instant::now(),
                    idle: Mutex::new(Instant::now()),
                    lanes: Arc::new(Semaphore::new(2)),
                    active: Arc::new(Semaphore::new(1)),
                    imported: AtomicU64::new(0),
                    screenshot: Mutex::new(None),
                    snapshot: Mutex::new(Value::Null),
                });
                entries.insert(session.clone(), entry.clone());
                entry
            };
            let _lane = entry
                .lanes
                .clone()
                .try_acquire_owned()
                .expect("fresh browser lane");
            let _active = entry
                .active
                .clone()
                .try_acquire_owned()
                .expect("fresh browser active operation");
            self.watch(session.clone(), entry.clone());
            let opening = self.runtime.open_session(
                policy.clone(),
                &entry.binding.browser_id,
                entry.stop.clone(),
            );
            tokio::pin!(opening);
            let opened = tokio::select! {biased;
                ()=stop.cancelled()=>{entry.stop.cancel();opening.await},
                ()=entry.stop.cancelled()=>opening.await,
                ()=tokio::time::sleep(Duration::from_mins(1))=>{entry.stop.cancel();opening.await},
                result=&mut opening=>result,
            };
            if let Ok(native) = &opened {
                entry
                    .native
                    .set(native.clone())
                    .map_err(|_| "duplicate native browser")?;
            }
            entry.launch_settled.cancel();
            match opened {
                Ok(native) => {
                    if authority.admit().is_err() {
                        entry.stop.cancel();
                    }
                    if entry.stop.is_cancelled() {
                        self.retire(&session, &entry).await?;
                        return Err("open retired".into());
                    }
                    let duration = SessionOperation::Navigate {
                        binding: entry.binding.clone(),
                        url: url.clone(),
                    }
                    .duration()
                    .expect("navigation budget");
                    let result = tokio::select! {biased;()=stop.cancelled()=>{entry.stop.cancel();Err("open navigation cancelled; do not replay".into())},result=native.session_command(json!({"operation":"navigate","url":url}),duration)=>result};
                    match result {
                        Ok(result) => {
                            entry.ready.store(true, Ordering::SeqCst);
                            Self::commit(&entry, &result);
                            Ok(self.report(Some(&entry), result))
                        }
                        Err(error) => {
                            self.retire(&session, &entry).await?;
                            Err(error)
                        }
                    }
                }
                Err(error) => {
                    self.retire(&session, &entry).await?;
                    match error {
                        OpenError::Capacity => {
                            Ok(self
                                .report(None, json!({"status":"not_started","reason":"capacity"})))
                        }
                        OpenError::Unavailable(detail) => Err(detail),
                    }
                }
            }
        } else {
            let entry = self
                .entries
                .lock()
                .expect("browser entries")
                .get(&session)
                .cloned()
                .ok_or("browser closed; open a new instance")?;
            if operation.binding() != Some(&entry.binding) || entry.header != header {
                return Ok(self.report(
                    Some(&entry),
                    json!({"status":"not_started","reason":"stale_browser"}),
                ));
            }
            if matches!(operation, SessionOperation::Close { .. }) {
                self.retire(&session, &entry).await?;
                return Ok(self.report(None, json!({"status":"completed"})));
            }
            let Ok(_lane) = entry.lanes.clone().try_acquire_owned() else {
                return Ok(self.report(
                    Some(&entry),
                    json!({"status":"not_started","reason":"busy","retry_after_ms":1000}),
                ));
            };
            let deadline = tokio::time::Instant::now()
                + operation.duration().ok_or("invalid operation phase")?;
            let queued = tokio::select! {biased;()=stop.cancelled()=>return Ok(self.report(Some(&entry),json!({"status":"not_started","reason":"cancelled"}))),()=entry.stop.cancelled()=>return Err("browser retired".into()),result=tokio::time::timeout(Duration::from_secs(5),entry.active.clone().acquire_owned())=>result};
            let Ok(Ok(_active)) = queued else {
                return Ok(self.report(
                    Some(&entry),
                    json!({"status":"not_started","reason":"busy","retry_after_ms":1000}),
                ));
            };
            let _fresh = authority.admit()?;
            let _owner = entry.authority.admit()?;
            let native = entry.native.get().ok_or("browser still opening")?;
            if let SessionOperation::Navigate { url, .. } = &operation
                && entry.policy.navigate(url).is_err()
            {
                return Ok(self.report(
                    Some(&entry),
                    json!({"status":"not_started","reason":"policy_blocked"}),
                ));
            }
            let mut packet = serde_json::to_value(&operation).map_err(|e| e.to_string())?;
            packet
                .as_object_mut()
                .expect("typed operation")
                .remove("binding");
            let result = tokio::select! {biased;()=stop.cancelled()=>{entry.stop.cancel();Err("browser operation cancelled after dispatch; do not replay".into())},result=native.session_command(packet,deadline.saturating_duration_since(tokio::time::Instant::now()))=>result};
            match result {
                Ok(mut result) => {
                    if matches!(operation, SessionOperation::Screenshot { .. })
                        && !self
                            .import_before_deadline(&entry, &mut result, &stop, deadline)
                            .await
                    {
                        return Err("screenshot publication exceeded its budget or was cancelled; do not replay".into());
                    }
                    if authority.admit().is_err() || entry.authority.admit().is_err() {
                        self.retire(&session, &entry).await?;
                        return Err("browser authority withdrawn".into());
                    }
                    Self::commit(&entry, &result);
                    Ok(self.report(Some(&entry), result))
                }
                Err(error) => {
                    self.retire(&session, &entry).await?;
                    Err(error)
                }
            }
        }
    }
    fn commit(entry: &Entry, result: &Value) {
        if result["status"] == "completed" {
            *entry.idle.lock().expect("browser idle") = Instant::now();
            if let Some(snapshot) = result.get("snapshot") {
                *entry.snapshot.lock().expect("browser snapshot") = snapshot.clone();
            }
        }
    }
    async fn import_before_deadline(
        &self,
        entry: &Entry,
        result: &mut Value,
        stop: &CancellationToken,
        deadline: tokio::time::Instant,
    ) -> bool {
        let importing = self.import(entry, result);
        tokio::pin!(importing);
        let settled = tokio::select! {biased;()=stop.cancelled()=>false,()=entry.stop.cancelled()=>false,()=tokio::time::sleep_until(deadline)=>false,()=&mut importing=>true};
        if !settled {
            entry.stop.cancel();
        }
        settled
    }
    async fn import(&self, entry: &Entry, result: &mut Value) {
        let imported = async {
            let encoded = result["png"].as_str().ok_or("screenshot unavailable")?;
            let png = STANDARD
                .decode(encoded)
                .map_err(|_| "invalid screenshot encoding")?;
            if png.len() > MAXIMUM_SCREENSHOT_BYTES
                || png.len() < 24
                || &png[..8] != b"\x89PNG\r\n\x1a\n"
                || &png[12..16] != b"IHDR"
                || u32::from_be_bytes(png[16..20].try_into().expect("width")) != 1280
                || u32::from_be_bytes(png[20..24].try_into().expect("height")) != 720
            {
                return Err("invalid viewport PNG");
            }
            entry
                .imported
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                    let next = n.checked_add(MAXIMUM_SCREENSHOT_BYTES as u64)?;
                    (next <= MAXIMUM_IMPORTED_BYTES).then_some(next)
                })
                .map_err(|_| "screenshot import capacity")?;
            let media = self
                .media
                .import_image_with_options(
                    png.into(),
                    rsi_media_protocol::ImageImportOptions {
                        maximum_output_bytes: MAXIMUM_SCREENSHOT_BYTES as u64,
                        source_mime: Some("image/png".into()),
                    },
                )
                .await
                .map_err(|error| {
                    if matches!(
                        error,
                        rsi_media_protocol::MediaError::InvalidInput(_)
                            | rsi_media_protocol::MediaError::Codec(_)
                            | rsi_media_protocol::MediaError::AdmissionFull(_)
                    ) {
                        entry
                            .imported
                            .fetch_sub(MAXIMUM_SCREENSHOT_BYTES as u64, Ordering::SeqCst);
                    }
                    "screenshot import unavailable"
                })?;
            media
                .validate()
                .map_err(|_| "invalid screenshot identity")?;
            if media.bytes > MAXIMUM_SCREENSHOT_BYTES as u64
                || media.width != 1280
                || media.height != 720
                || media.mime != "image/png"
            {
                return Err("invalid canonical viewport PNG");
            }
            entry.imported.fetch_sub(
                MAXIMUM_SCREENSHOT_BYTES as u64 - media.bytes,
                Ordering::SeqCst,
            );
            *entry.screenshot.lock().expect("browser screenshot") = Some(media);
            Ok(())
        }
        .await;
        result.as_object_mut().expect("helper result").remove("png");
        if let Err(reason) = imported {
            *result = json!({"status":"screenshot_unavailable","reason":reason});
        }
    }
    fn watch(self: &Arc<Self>, session: SessionId, entry: Arc<Entry>) {
        let owner = self.clone();
        self.tasks.spawn(async move{loop{tokio::select!{()=entry.stop.cancelled()=>break,()=tokio::time::sleep(Duration::from_secs(1))=>{}}
        if entry.authority.admit().is_err()||entry.admitted.elapsed()>=Duration::from_mins(10)||entry.idle.lock().expect("browser idle").elapsed()>=Duration::from_mins(5)||entry.native.get().is_some_and(|n|n.is_retired()){entry.stop.cancel();break;}
    }let _=owner.retire(&session,&entry).await;});
    }
    async fn retire(&self, session: &SessionId, entry: &Arc<Entry>) -> Result<(), String> {
        entry.stop.cancel();
        entry.active.close();
        entry.lanes.close();
        let result = entry
            .retirement
            .get_or_init(|| async {
                entry.launch_settled.cancelled().await;
                if let Some(native) = entry.native.get() {
                    native.close().await
                } else {
                    Ok(())
                }
            })
            .await
            .clone();
        let mut entries = self.entries.lock().expect("browser entries");
        if entries.get(session).is_some_and(|e| Arc::ptr_eq(e, entry)) {
            entries.remove(session);
        }
        result
    }
    pub async fn close(&self) {
        self.stop.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
    /// # Errors
    /// Rejects stale identity, withdrawal, invalid bounds and failed Media reads.
    /// # Panics
    /// Panics if an internal owner lock was poisoned by a prior panic.
    pub async fn screenshot_bytes(
        &self,
        authority: &SessionAuthority,
        binding: &BrowserBinding,
        media: &MediaRef,
        offset: u64,
        maximum: usize,
    ) -> Result<Vec<u8>, String> {
        let _permission = authority.admit()?;
        let entry = self
            .entries
            .lock()
            .expect("browser entries")
            .get(authority.header().session_id())
            .cloned()
            .ok_or("browser closed")?;
        if entry.stop.is_cancelled()
            || entry.header
                != authority
                    .header()
                    .fingerprint()
                    .map_err(|e| e.to_string())?
            || &entry.binding != binding
            || entry
                .screenshot
                .lock()
                .expect("browser screenshot")
                .as_ref()
                != Some(media)
            || maximum > 65536
        {
            return Err("screenshot source expired".into());
        }
        let _owner = entry.authority.admit()?;
        let stored = self
            .media
            .read(media)
            .await
            .map_err(|_| "screenshot bytes unavailable")?;
        let _fresh = authority.admit()?;
        if entry.stop.is_cancelled() {
            return Err("screenshot source expired".into());
        }
        let start = usize::try_from(offset)
            .map_err(|_| "invalid screenshot offset")?
            .min(stored.bytes.len());
        Ok(stored.bytes[start..(start + maximum).min(stored.bytes.len())].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_entry() -> Arc<Entry> {
        let authority = test_authority();
        Arc::new(Entry {
            binding: BrowserBinding {
                service_epoch: "a".repeat(32),
                browser_id: "b".repeat(32),
            },
            header: authority.header().fingerprint().unwrap(),
            authority,
            policy: SessionPolicy::PublicWeb {},
            native: OnceCell::new(),
            ready: AtomicBool::new(true),
            stop: CancellationToken::new(),
            launch_settled: CancellationToken::new(),
            retirement: OnceCell::new(),
            admitted: Instant::now(),
            idle: Mutex::new(Instant::now()),
            lanes: Arc::new(Semaphore::new(2)),
            active: Arc::new(Semaphore::new(1)),
            imported: AtomicU64::new(0),
            screenshot: Mutex::new(None),
            snapshot: Mutex::new(Value::Null),
        })
    }
    #[derive(Debug)]
    struct ImportMedia {
        result: rsi_media_protocol::Result<MediaRef>,
        pending: bool,
        entered: CancellationToken,
        calls: AtomicU64,
    }
    #[async_trait::async_trait]
    impl Media for ImportMedia {
        async fn import_image_with_options(
            &self,
            _: bytes::Bytes,
            options: rsi_media_protocol::ImageImportOptions,
        ) -> rsi_media_protocol::Result<MediaRef> {
            assert_eq!(
                options.maximum_output_bytes,
                MAXIMUM_SCREENSHOT_BYTES as u64
            );
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.cancel();
            if self.pending {
                std::future::pending::<()>().await;
            }
            self.result.clone()
        }
        async fn read(
            &self,
            _: &MediaRef,
        ) -> rsi_media_protocol::Result<rsi_media_protocol::StoredMedia> {
            unreachable!()
        }
    }
    fn image_result() -> Value {
        let mut png = Vec::from(&b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR"[..]);
        png.extend_from_slice(&1280_u32.to_be_bytes());
        png.extend_from_slice(&720_u32.to_be_bytes());
        json!({"status":"completed","png":STANDARD.encode(png)})
    }
    fn canonical_image() -> MediaRef {
        MediaRef {
            id: rsi_media_protocol::MediaId::new("a".repeat(64)).unwrap(),
            mime: "image/png".into(),
            bytes: 1024,
            width: 1280,
            height: 720,
        }
    }
    #[cfg(target_os = "linux")]
    #[tokio::test(start_paused = true)]
    async fn screenshot_deadline_or_cancellation_drops_the_wait_without_losing_its_charge() {
        for cancelled in [false, true] {
            let (runtime, _permits) = crate::runtime::ownership_tests::full_ready_runtime();
            let media = Arc::new(ImportMedia {
                result: Ok(canonical_image()),
                pending: true,
                entered: CancellationToken::new(),
                calls: AtomicU64::new(0),
            });
            let owner = SessionBrowser::new(runtime, media.clone()).unwrap();
            let entry = test_entry();
            let stop = CancellationToken::new();
            let mut result = image_result();
            let mut import = Box::pin(owner.import_before_deadline(
                &entry,
                &mut result,
                &stop,
                tokio::time::Instant::now() + Duration::from_secs(15),
            ));
            assert!(futures_util::poll!(&mut import).is_pending());
            assert!(media.entered.is_cancelled());
            if cancelled {
                stop.cancel();
            }
            assert!(!import.await);
            assert!(entry.stop.is_cancelled());
            assert_eq!(
                entry.imported.load(Ordering::SeqCst),
                MAXIMUM_SCREENSHOT_BYTES as u64
            );
            owner.close().await;
        }
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn image_budget_refunds_prepublication_rejections_and_keeps_uncertain_receipts() {
        use rsi_media_protocol::MediaError;
        for (reply, expected) in [
            (Err(MediaError::InvalidInput("input".into())), 0),
            (Err(MediaError::Codec("codec".into())), 0),
            (Err(MediaError::AdmissionFull("pressure".into())), 0),
            (
                Err(MediaError::Api(rsi_api_protocol::ApiError::OutcomeUnknown)),
                MAXIMUM_SCREENSHOT_BYTES as u64,
            ),
            (
                Ok(MediaRef {
                    width: 1,
                    ..canonical_image()
                }),
                MAXIMUM_SCREENSHOT_BYTES as u64,
            ),
            (Ok(canonical_image()), 1024),
        ] {
            let (runtime, _permits) = crate::runtime::ownership_tests::full_ready_runtime();
            let media = Arc::new(ImportMedia {
                result: reply,
                pending: false,
                entered: CancellationToken::new(),
                calls: AtomicU64::new(0),
            });
            let owner = SessionBrowser::new(runtime, media.clone()).unwrap();
            let entry = test_entry();
            let mut result = image_result();
            owner.import(&entry, &mut result).await;
            assert_eq!(entry.imported.load(Ordering::SeqCst), expected);
            assert!(result.get("png").is_none());
            entry
                .imported
                .store(MAXIMUM_IMPORTED_BYTES, Ordering::SeqCst);
            owner.import(&entry, &mut image_result()).await;
            assert_eq!(media.calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                entry.imported.load(Ordering::SeqCst),
                MAXIMUM_IMPORTED_BYTES
            );
            owner.close().await;
        }
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn failed_retirement_removes_the_binding_and_all_waiters_share_the_failure() {
        let (runtime, _permits) = crate::runtime::ownership_tests::full_ready_runtime();
        let owner = SessionBrowser::new(runtime, Arc::new(UnusedMedia)).unwrap();
        let entry = test_entry();
        let session = entry.authority.header().session_id().clone();
        entry
            .native
            .set(crate::runtime::ownership_tests::session_with_retirement(
                Err("failed native receipt".into()),
            ))
            .unwrap();
        entry.launch_settled.cancel();
        owner
            .entries
            .lock()
            .unwrap()
            .insert(session.clone(), entry.clone());
        let (first, second) = tokio::join!(
            owner.retire(&session, &entry),
            owner.retire(&session, &entry)
        );
        assert_eq!(first, Err("failed native receipt".into()));
        assert_eq!(first, second);
        assert!(!owner.entries.lock().unwrap().contains_key(&session));
        owner.close().await;
    }
    fn test_authority() -> SessionAuthority {
        let header = Arc::new(
            SessionHeader::new_local(
                SessionId::new("capacity-caller").unwrap(),
                1,
                "/capacity-test",
                serde_json::from_value(json!("fixture")).unwrap(),
                rsi_agent_session_protocol::FrozenAgentSettings::new(
                    "fixture",
                    "system",
                    serde_json::from_value(json!({"deployment":"fixture","model":"model"}))
                        .unwrap(),
                    rsi_sandbox::SandboxMode::ReadOnly,
                    false,
                )
                .unwrap(),
            )
            .unwrap(),
        );
        let issuer = rsi_agent_turn_protocol::TurnClaimIssuer::new();
        let claim = issuer.issue(
            "fixture".into(),
            1,
            header.session_id().clone(),
            rsi_agent_session_protocol::TurnId::new("turn").unwrap(),
            header,
            1,
            1,
            1,
        );
        SessionAuthority::Agent(Arc::new(issuer.agent_caller(&claim).unwrap()))
    }
    #[cfg(target_os = "linux")]
    #[derive(Debug)]
    struct UnusedMedia;
    #[cfg(target_os = "linux")]
    #[async_trait::async_trait]
    impl Media for UnusedMedia {
        async fn import_image_with_options(
            &self,
            _: bytes::Bytes,
            _: rsi_media_protocol::ImageImportOptions,
        ) -> rsi_media_protocol::Result<MediaRef> {
            panic!("capacity refusal must not import an image")
        }
        async fn read(
            &self,
            _: &MediaRef,
        ) -> rsi_media_protocol::Result<rsi_media_protocol::StoredMedia> {
            unreachable!()
        }
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn shared_pool_capacity_returns_not_started_and_removes_the_unlaunched_session_entry() {
        let (runtime, _permits) = crate::runtime::ownership_tests::full_ready_runtime();
        let owner = SessionBrowser::new(runtime, Arc::new(UnusedMedia)).unwrap();
        let authority = test_authority();
        let session = authority.header().session_id().clone();
        let result = owner
            .execute(
                authority.clone(),
                SessionOperation::Open {
                    policy: SessionPolicy::PublicWeb {},
                    url: "https://public.example/".into(),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            result.result,
            json!({"status":"not_started","reason":"capacity"})
        );
        assert_eq!(result.state, "closed");
        assert!(result.binding.is_none());
        assert!(!owner.entries.lock().unwrap().contains_key(&session));
        owner.close().await;
    }
    #[test]
    fn node_actions_require_canonical_bounded_observation_coordinates() {
        let binding = BrowserBinding {
            service_epoch: "a".repeat(32),
            browser_id: "b".repeat(32),
        };
        let valid = SessionOperation::Click {
            binding: binding.clone(),
            document_version: "1".into(),
            observation_id: "c".repeat(32),
            node: "256".into(),
        };
        assert!(valid.validate().is_ok());
        for (version, observation, node) in [
            ("01", "c".repeat(32), "1"),
            ("0", "c".repeat(32), "1"),
            ("1", "x".repeat(32), "1"),
            ("1", "c".repeat(32), "257"),
            ("1", "c".repeat(32), "001"),
        ] {
            assert!(
                SessionOperation::Click {
                    binding: binding.clone(),
                    document_version: version.into(),
                    observation_id: observation,
                    node: node.into()
                }
                .validate()
                .is_err()
            );
        }
    }
}
