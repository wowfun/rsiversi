use super::{Config, load};
use rsi_api_http::{HttpAsset, HttpAssets};
use rsi_api_protocol::{ApiError, ByteBudget};
use rsi_meta::{Execution, LocalContract};
use rsi_ui_protocol::RendererCatalog;
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::{oneshot, watch};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const MANIFEST: &str = "ui-renderers.json";
const BOOTSTRAP: &[&str] = &[
    "index.html",
    "app.js",
    "mounts.js",
    "drafts.js",
    "worker.js",
    "styles.css",
    "rsi_web.js",
    "rsi_web_bg.wasm",
];

/// Renderer publication rejection, distinct from HTTP transport failures.
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    /// Invalid source metadata, digest or import graph.
    #[error("invalid renderer bundle: {0}")]
    Invalid(String),
    /// Current/candidate/retiring ownership or shared byte admission is full.
    #[error("renderer publication capacity exhausted")]
    Capacity,
    /// The expected current generation or candidate no longer matches.
    #[error("renderer publication changed")]
    Conflict,
    /// Bootstrap, Worker or a non-renderer file changed.
    #[error("bootstrap or Worker changes require restart")]
    RestartRequired,
    /// Owning plugin has retired.
    #[error("Web asset owner retired")]
    Closed,
}
/// Renderer publication result.
pub type AssetResult<T> = std::result::Result<T, AssetError>;

#[derive(Debug)]
struct Bundle {
    revision: String,
    files: BTreeMap<String, HttpAsset>,
    renderer_files: BTreeSet<String>,
    catalog: Option<RendererCatalog>,
    leases: AtomicUsize,
}
impl Bundle {
    fn new(files: BTreeMap<String, HttpAsset>) -> AssetResult<Arc<Self>> {
        let mut digest = Sha256::new();
        for (name, asset) in &files {
            digest.update((name.len() as u64).to_le_bytes());
            digest.update(name);
            digest.update((asset.bytes.len() as u64).to_le_bytes());
            digest.update(asset.bytes.as_bytes());
        }
        let mut renderer_files = BTreeSet::new();
        let catalog = files
            .get(&format!("/{MANIFEST}"))
            .map(|manifest| {
                if manifest.bytes.len() > rsi_ui_protocol::MAXIMUM_VIEW_BYTES {
                    return Err(AssetError::Invalid(
                        "renderer manifest exceeds limit".into(),
                    ));
                }
                let catalog: RendererCatalog = serde_json::from_slice(manifest.bytes.as_bytes())
                    .map_err(|_| AssetError::Invalid("renderer manifest JSON".into()))?;
                catalog
                    .validate()
                    .map_err(|error| AssetError::Invalid(error.to_string()))?;
                for renderer in &catalog.renderers {
                    for file in &renderer.files {
                        if BOOTSTRAP.contains(&file.name.as_str()) || file.name == MANIFEST {
                            return Err(AssetError::Invalid(
                                "renderer graph includes bootstrap".into(),
                            ));
                        }
                        let name = format!("/{}", file.name);
                        let asset = files
                            .get(&name)
                            .ok_or_else(|| AssetError::Invalid("renderer file is absent".into()))?;
                        if hex::encode(Sha256::digest(asset.bytes.as_bytes())) != file.sha256 {
                            return Err(AssetError::Invalid(
                                "renderer file digest mismatch".into(),
                            ));
                        }
                        renderer_files.insert(name);
                    }
                }
                renderer_files.insert(format!("/{MANIFEST}"));
                Ok(catalog)
            })
            .transpose()?;
        Ok(Arc::new(Self {
            revision: hex::encode(digest.finalize()),
            files,
            renderer_files,
            catalog,
            leases: AtomicUsize::new(0),
        }))
    }
}
#[derive(Debug, Default)]
struct State {
    current: Option<Arc<Bundle>>,
    retiring: Option<Weak<Bundle>>,
    candidate: Option<(u64, Arc<Bundle>)>,
    pending: Option<u64>,
    next: u64,
}
/// The same ordinary plugin owner serves HTTP bytes and renderer publication.
#[derive(Debug)]
pub struct WebAssetControl {
    state: Mutex<State>,
    budget: ByteBudget,
    stop: CancellationToken,
    tasks: TaskTracker,
    execution: Execution,
    changed: watch::Sender<String>,
    diagnostic: Mutex<Option<String>>,
}
/// Local management authority; remote lease admission is a consuming API concern.
#[derive(Debug)]
pub struct WebAssetControlContract;
impl LocalContract for WebAssetControlContract {
    const KEY: &'static str = "rsi.web.assets.control";
    type Service = WebAssetControl;
}
impl WebAssetControl {
    pub(super) fn new(execution: Execution) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            budget: ByteBudget::default(),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            execution,
            changed: watch::channel(String::new()).0,
            diagnostic: Mutex::new(None),
        })
    }
    pub(super) async fn initialize(self: &Arc<Self>, config: Config) -> AssetResult<()> {
        let initial_stamp = if config.watch {
            Some(self.stamp(config.clone()).await?)
        } else {
            None
        };
        let bundle = self.read(config.clone()).await?;
        let mut state = self.state.lock().expect("Web assets poisoned");
        if self.stop.is_cancelled() {
            return Err(AssetError::Closed);
        }
        self.changed.send_replace(bundle.revision.clone());
        state.current = Some(bundle);
        drop(state);
        if let Some(stamp) = initial_stamp {
            self.start_watch(config, stamp);
        }
        Ok(())
    }
    async fn read(self: &Arc<Self>, config: Config) -> AssetResult<Arc<Bundle>> {
        let token = self.tasks.token();
        let owner = self.clone();
        let previous = self
            .state
            .lock()
            .expect("Web assets poisoned")
            .current
            .clone();
        self.execution
            .prepare(move || {
                let _token = token;
                let files = load(
                    &config,
                    &owner.stop,
                    &owner.budget,
                    previous.as_ref().map(|bundle| &bundle.files),
                )?;
                Bundle::new(files)
            })
            .await
            .map_err(|_| AssetError::Invalid("bundle reader stopped".into()))?
    }
    /// Current exact bundle digest, or Closed after retirement.
    ///
    /// # Panics
    /// Panics if the asset state mutex was poisoned by an earlier panic.
    pub fn revision(&self) -> AssetResult<String> {
        if self.stop.is_cancelled() {
            return Err(AssetError::Closed);
        }
        self.state
            .lock()
            .expect("Web assets poisoned")
            .current
            .as_ref()
            .map(|bundle| bundle.revision.clone())
            .ok_or(AssetError::Closed)
    }
    /// Coalesced successful publications, with no source-file polling.
    pub fn changes(&self) -> watch::Receiver<String> {
        self.changed.subscribe()
    }
    /// Shared retained file capacity, including escaped responses and complete leases.
    pub fn retained_bytes(&self) -> usize {
        self.budget.used()
    }
    /// Acquires a complete graph before the caller fetches any module or manifest.
    ///
    /// # Panics
    /// Panics if the asset state mutex was poisoned by an earlier panic.
    pub fn acquire(self: &Arc<Self>, expected: &str) -> AssetResult<BundleLease> {
        let state = self.state.lock().expect("Web assets poisoned");
        if self.stop.is_cancelled() {
            return Err(AssetError::Closed);
        }
        let bundle = state.current.as_ref().ok_or(AssetError::Closed)?;
        if bundle.revision != expected {
            return Err(AssetError::Conflict);
        }
        bundle
            .leases
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map_err(|_| AssetError::Capacity)?;
        Ok(BundleLease {
            owner: Arc::downgrade(self),
            bundle: bundle.clone(),
        })
    }
    /// Admits one owned read before returning. Dropping its waiter cannot orphan
    /// a candidate; unclaimed completed candidates are discarded by their handles.
    ///
    /// # Panics
    /// Panics if the asset state mutex was poisoned by an earlier panic.
    pub fn stage(
        self: &Arc<Self>,
        directory: PathBuf,
        files: Vec<String>,
    ) -> AssetResult<StageTicket> {
        let config = Config {
            directory,
            files,
            watch: false,
        };
        config
            .validate()
            .map_err(|error| AssetError::Invalid(error.to_string()))?;
        let mut state = self.state.lock().expect("Web assets poisoned");
        if self.stop.is_cancelled() {
            return Err(AssetError::Closed);
        }
        if state.pending.is_some() || state.candidate.is_some() {
            return Err(AssetError::Capacity);
        }
        state.next = state.next.checked_add(1).ok_or(AssetError::Capacity)?;
        let id = state.next;
        state.pending = Some(id);
        let token = self.tasks.token();
        let (send, receive) = oneshot::channel();
        let owner = self.clone();
        drop(state);
        drop(self.execution.spawn(async move {
            let _token = token;
            let result = owner.read(config).await.and_then(|bundle| {
                let mut state = owner.state.lock().expect("Web assets poisoned");
                if owner.stop.is_cancelled() || state.pending != Some(id) {
                    return Err(AssetError::Closed);
                }
                state.candidate = Some((id, bundle));
                Ok(AssetCandidate {
                    owner: Arc::downgrade(&owner),
                    id,
                })
            });
            {
                let mut state = owner.state.lock().expect("Web assets poisoned");
                if state.pending == Some(id) {
                    state.pending = None;
                }
            }
            let _ = send.send(result);
        }));
        Ok(StageTicket(receive))
    }
    pub(super) async fn close(&self) {
        {
            let mut state = self.state.lock().expect("Web assets poisoned");
            self.stop.cancel();
            state.current.take();
            state.candidate.take();
            state.pending.take();
            state.retiring.take();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
}
impl HttpAssets for WebAssetControl {
    fn get(&self, path: &str) -> rsi_api_protocol::Result<Option<HttpAsset>> {
        let state = self.state.lock().expect("Web assets poisoned");
        if self.stop.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        let current = state.current.as_ref().ok_or(ApiError::Unavailable)?;
        if let Some(path) = path.strip_prefix("/rsi-renderers/") {
            let Some((revision, file)) = path.split_once('/') else {
                return Ok(None);
            };
            if !rsi_ui_protocol::digest_valid(revision) || !rsi_ui_protocol::asset_name_valid(file)
            {
                return Ok(None);
            }
            let bundle = if revision == current.revision
                && current.leases.load(Ordering::Acquire) != 0
            {
                Some(current.clone())
            } else {
                state
                    .retiring
                    .as_ref()
                    .and_then(Weak::upgrade)
                    .filter(|bundle| {
                        bundle.revision == revision && bundle.leases.load(Ordering::Acquire) != 0
                    })
            };
            let name = format!("/{file}");
            return Ok(bundle
                .filter(|bundle| bundle.renderer_files.contains(&name))
                .and_then(|bundle| bundle.files.get(&name).cloned()));
        }
        let name = if path == "/" { "/index.html" } else { path };
        if current.renderer_files.contains(name) {
            return Ok(None);
        }
        Ok(current.files.get(name).cloned())
    }
}
/// Complete immutable renderer graph, independently of individual HTTP response bytes.
#[derive(Debug)]
pub struct BundleLease {
    owner: Weak<WebAssetControl>,
    bundle: Arc<Bundle>,
}
impl Drop for BundleLease {
    fn drop(&mut self) {
        self.bundle.leases.fetch_sub(1, Ordering::AcqRel);
    }
}
impl BundleLease {
    /// Exact digest embedded into every renderer asset URL.
    pub fn revision(&self) -> &str {
        &self.bundle.revision
    }
    /// Admitted manifest metadata; absent for a static-only bundle.
    pub fn catalog(&self) -> Option<&RendererCatalog> {
        self.bundle.catalog.as_ref()
    }
    /// Constructs an exact same-generation URL for a declared renderer file.
    pub fn url(&self, name: &str) -> AssetResult<String> {
        let owner = self.owner.upgrade().ok_or(AssetError::Closed)?;
        if owner.stop.is_cancelled() {
            return Err(AssetError::Closed);
        }
        if !self.bundle.renderer_files.contains(&format!("/{name}")) {
            return Err(AssetError::Invalid("file is not in renderer graph".into()));
        }
        Ok(format!("/rsi-renderers/{}/{name}", self.bundle.revision))
    }
}
/// Completion waiter for one owned preparation. Its Drop does not cancel reading.
#[derive(Debug)]
pub struct StageTicket(oneshot::Receiver<AssetResult<AssetCandidate>>);
impl StageTicket {
    /// Receives one prepared candidate after complete digest and manifest validation.
    pub async fn wait(self) -> AssetResult<AssetCandidate> {
        self.0.await.map_err(|_| AssetError::Closed)?
    }
}
/// One bounded candidate held in its active owner; Drop releases unpublished storage.
#[derive(Debug)]
pub struct AssetCandidate {
    owner: Weak<WebAssetControl>,
    id: u64,
}
impl AssetCandidate {
    /// Publishes only if current still matches and at most one retiring lease remains.
    /// An unleased current may be replaced without releasing the older displayed graph.
    /// Capacity/conflict errors preserve the current generation and the candidate.
    ///
    /// # Panics
    /// Panics if the asset state mutex was poisoned by an earlier panic.
    pub fn publish(&self, expected: &str) -> AssetResult<String> {
        let owner = self.owner.upgrade().ok_or(AssetError::Closed)?;
        let mut state = owner.state.lock().expect("Web assets poisoned");
        if owner.stop.is_cancelled() {
            return Err(AssetError::Closed);
        }
        let current = state.current.as_ref().ok_or(AssetError::Closed)?;
        if current.revision != expected {
            return Err(AssetError::Conflict);
        }
        let (_, candidate) = state
            .candidate
            .as_ref()
            .filter(|(id, _)| *id == self.id)
            .ok_or(AssetError::Conflict)?;
        if candidate.revision == current.revision {
            let result = current.revision.clone();
            state.candidate.take();
            return Ok(result);
        }
        let retiring_is_leased = state
            .retiring
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|bundle| bundle.leases.load(Ordering::Acquire) != 0);
        if retiring_is_leased && current.leases.load(Ordering::Acquire) != 0 {
            return Err(AssetError::Capacity);
        }
        if candidate.catalog.is_none() {
            return Err(AssetError::Invalid(
                "candidate has no renderer catalog".into(),
            ));
        }
        for (base, other) in [(current, candidate), (candidate, current)] {
            for (name, asset) in &base.files {
                if !base.renderer_files.contains(name)
                    && Some(&asset.bytes) != other.files.get(name).map(|asset| &asset.bytes)
                {
                    return Err(AssetError::RestartRequired);
                }
            }
        }
        if !retiring_is_leased {
            state.retiring = Some(Arc::downgrade(current));
        }
        let (_, candidate) = state.candidate.take().expect("validated candidate");
        let revision = candidate.revision.clone();
        state.current = Some(candidate);
        owner.changed.send_replace(revision.clone());
        Ok(revision)
    }
}
impl Drop for AssetCandidate {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            let mut state = owner.state.lock().expect("Web assets poisoned");
            if state
                .candidate
                .as_ref()
                .is_some_and(|(id, _)| *id == self.id)
            {
                state.candidate.take();
            }
        }
    }
}

impl WebAssetControl {
    /// Last bounded automatic publication failure; successful publication clears it.
    ///
    /// # Panics
    /// Panics if a prior task poisoned diagnostic storage.
    pub fn diagnostic(&self) -> Option<String> {
        self.diagnostic
            .lock()
            .expect("Web diagnostic poisoned")
            .clone()
    }
    fn start_watch(self: &Arc<Self>, config: Config, stamp: Vec<(u64, std::time::SystemTime)>) {
        let owner = self.clone();
        let token = self.tasks.token();
        self.execution.spawn(async move {
            let _token = token;
            let mut previous = Some(stamp);
            loop {
                tokio::select! { biased;
                    () = owner.stop.cancelled() => break,
                    () = owner.execution.sleep(std::time::Duration::from_millis(250)) => {}
                }
                let result = async {
                    let next = owner.stamp(config.clone()).await?;
                    if previous.as_ref() == Some(&next) {
                        return Ok(());
                    }
                    previous = Some(next);
                    let expected = owner.revision()?;
                    owner
                        .stage(config.directory.clone(), config.files.clone())?
                        .wait()
                        .await?
                        .publish(&expected)?;
                    *owner.diagnostic.lock().expect("Web diagnostic poisoned") = None;
                    Ok::<(), AssetError>(())
                }
                .await;
                if let Err(error) = result {
                    let message: String = error.to_string().chars().take(1024).collect();
                    *owner.diagnostic.lock().expect("Web diagnostic poisoned") = Some(message);
                }
            }
        });
    }
    async fn stamp(
        self: &Arc<Self>,
        config: Config,
    ) -> AssetResult<Vec<(u64, std::time::SystemTime)>> {
        let token = self.tasks.token();
        self.execution
            .prepare(move || {
                let _token = token;
                let directory =
                    super::open_directory(&config.directory).map_err(super::io_error)?;
                config
                    .files
                    .iter()
                    .map(|name| {
                        let file = super::open_file(&directory, &config.directory, name)
                            .map_err(super::io_error)?;
                        let metadata = file.metadata().map_err(super::io_error)?;
                        if !metadata.is_file() {
                            return Err(AssetError::Invalid(
                                "Web watch input is not a regular file".into(),
                            ));
                        }
                        Ok((
                            metadata.len(),
                            metadata.modified().map_err(super::io_error)?,
                        ))
                    })
                    .collect()
            })
            .await
            .map_err(|_| AssetError::Closed)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn staging_references_do_not_grant_renderer_access() {
        let owner = WebAssetControl::new(Execution::native(tokio::runtime::Handle::current()));
        let revision = "a".repeat(64);
        let bundle = Arc::new(Bundle {
            revision: revision.clone(),
            files: BTreeMap::from([(
                "/renderer.js".into(),
                HttpAsset {
                    kind: rsi_api_http::AssetType::JavaScript,
                    bytes: owner.budget.copy(b"renderer").unwrap(),
                },
            )]),
            renderer_files: BTreeSet::from(["/renderer.js".into()]),
            catalog: None,
            leases: AtomicUsize::new(0),
        });
        owner.state.lock().unwrap().current = Some(bundle.clone());
        // `read` retains this same extra Arc throughout candidate preparation.
        let url = format!("/rsi-renderers/{revision}/renderer.js");
        assert!(owner.get(&url).unwrap().is_none());
        let lease = owner.acquire(&revision).unwrap();
        let response = owner.get(&url).unwrap().unwrap();
        drop(lease);
        assert!(owner.get(&url).unwrap().is_none());
        {
            let mut state = owner.state.lock().unwrap();
            state.retiring = Some(Arc::downgrade(&bundle));
            state.current = Some(Bundle::new(BTreeMap::new()).unwrap());
        }
        assert!(owner.get(&url).unwrap().is_none());
        assert_eq!(response.bytes.as_bytes(), b"renderer");
        owner.close().await;
    }
}
