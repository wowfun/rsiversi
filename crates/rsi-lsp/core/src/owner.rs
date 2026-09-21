use crate::{Config, Error, Output, Query, Result, protocol, source, wire::Connection};
use rsi_files_protocol::Files;
use rsi_meta::Execution;
use rsi_process::{DuplexProcess, DuplexProcessSpec};
use rsi_sandbox::{ProcessRequest, ProcessStdio, Sandbox, SandboxMode};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Mutex as AsyncMutex, Semaphore},
    time::{Instant, timeout, timeout_at},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
type Slot = Arc<AsyncMutex<Option<Connection>>>;
#[derive(Debug, Default)]
struct State {
    closed: bool,
    slots: BTreeMap<PathBuf, Slot>,
    live: BTreeSet<PathBuf>,
}
struct Source {
    text: String,
    position: crate::Position,
    uri: String,
}
/// One exact provider generation with bounded, retained per-workspace processes.
#[derive(Debug)]
pub struct LanguageService {
    config: Config,
    process: Arc<dyn DuplexProcess>,
    sandbox: Arc<dyn Sandbox>,
    files: Arc<dyn Files>,
    execution: Execution,
    state: Mutex<State>,
    reads: Arc<Semaphore>,
    stop: CancellationToken,
    tasks: TaskTracker,
}
impl LanguageService {
    /// Creates an idle provider. No process is started before a valid source query.
    pub fn new(
        config: Config,
        process: Arc<dyn DuplexProcess>,
        sandbox: Arc<dyn Sandbox>,
        files: Arc<dyn Files>,
        execution: Execution,
    ) -> Result<Arc<Self>> {
        config.validate()?;
        Ok(Arc::new(Self {
            config,
            process,
            sandbox,
            files,
            execution,
            state: Mutex::new(State::default()),
            reads: Arc::new(Semaphore::new(4)),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
        }))
    }
    /// Query under a canonical workspace already authorized by the caller's owner.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned provider state.
    pub async fn query(
        self: &Arc<Self>,
        workspace: PathBuf,
        query: Query,
        cancellation: CancellationToken,
    ) -> Result<Output> {
        query.validate()?;
        if !workspace.is_absolute() {
            return Err(Error::Invalid);
        }
        let extension = std::path::Path::new(&query.path)
            .extension()
            .and_then(|s| s.to_str())
            .ok_or(Error::Unsupported)?;
        let language = self
            .config
            .languages
            .get(&format!(".{}", extension.to_ascii_lowercase()))
            .ok_or(Error::Unsupported)?
            .clone();
        let stop = self.stop.child_token();
        let _cancel = stop.clone().drop_guard();
        let owner = self.clone();
        let work_stop = stop.clone();
        let task = {
            let mut state = self.state.lock().expect("language admission");
            if state.closed {
                return Err(Error::Retired);
            }
            let permit = self
                .reads
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::Capacity)?;
            let slot = state.slots.entry(workspace.clone()).or_default().clone();
            let mut connection = slot.try_lock_owned().map_err(|_| Error::Capacity)?;
            self.execution.spawn(self.tasks.track_future(async move{
                let _permit=permit;
                let deadline=Instant::now()+Duration::from_secs(30);
                let source=tokio::select!{biased;()=work_stop.cancelled()=>Err(Error::Cancelled),result=timeout_at(deadline,owner.source(&workspace,&query,&work_stop))=>result.unwrap_or(Err(Error::Deadline))};
                let source=match source {Ok(source)=>source,Err(error)=>{
                    if connection.is_none(){owner.state.lock().expect("remove unused language slot").slots.remove(&workspace);}
                    return Err(error);
                }};
                // A validated source may evict idle processes. Cleanup remains
                // retained even when the query's wait budget has expired.
                let retired=match owner.evict_idle(&workspace){Ok(retired)=>retired,Err(error)=>{
                    owner.state.lock().expect("remove rejected language slot").slots.remove(&workspace);
                    return Err(error);
                }};
                for previous in retired {if let Err(error)=previous.close().await {
                    let mut state=owner.state.lock().expect("remove failed replacement slot");state.slots.remove(&workspace);state.live.remove(&workspace);
                    return Err(error);
                }}
                let work=owner.run(&workspace,&query,&language,&mut connection,&source);
                let result=tokio::select!{biased;()=work_stop.cancelled()=>Err(Error::Cancelled),result=timeout_at(deadline,work)=>result.unwrap_or(Err(Error::Deadline))};
                if result.is_err(){
                    let cleanup=if let Some(failed)=connection.take(){failed.close().await}else{Ok(())};
                    {let mut state=owner.state.lock().expect("remove failed language slot");state.slots.remove(&workspace);state.live.remove(&workspace);}
                    cleanup?;
                }
                result
            }))
        };
        tokio::select! {biased;()=cancellation.cancelled()=>Err(Error::Cancelled),()=self.stop.cancelled()=>Err(Error::Retired),result=task=>result.map_err(|_|Error::Unavailable)?}
    }
    fn evict_idle(&self, workspace: &std::path::Path) -> Result<Vec<Connection>> {
        let mut state = self.state.lock().expect("language pool replacement");
        let mut retired = Vec::new();
        if state.live.contains(workspace) {
            return Ok(retired);
        }
        if state.live.len() == 4 {
            let Some((path, mut guard)) = state.live.iter().find_map(|path| {
                state.slots[path]
                    .clone()
                    .try_lock_owned()
                    .ok()
                    .map(|guard| (path.clone(), guard))
            }) else {
                return Err(Error::Capacity);
            };
            if let Some(connection) = guard.take() {
                retired.push(connection);
            }
            state.slots.remove(&path);
            state.live.remove(&path);
        }
        state.live.insert(workspace.to_owned());
        Ok(retired)
    }
    async fn source(
        &self,
        workspace: &std::path::Path,
        query: &Query,
        stop: &CancellationToken,
    ) -> Result<Source> {
        let text = source::read(
            &self.files,
            &self.sandbox,
            workspace,
            &query.path,
            stop.clone(),
        )
        .await?;
        Ok(Source {
            position: protocol::position(&text, query.line, query.column)?,
            uri: protocol::file_uri(workspace, &query.path)?,
            text,
        })
    }
    async fn run(
        &self,
        workspace: &std::path::Path,
        query: &Query,
        language: &str,
        connection: &mut Option<Connection>,
        source: &Source,
    ) -> Result<Output> {
        if connection.is_none() {
            let confined = self
                .sandbox
                .confine(ProcessRequest {
                    program: self.config.program.clone(),
                    arguments: self.config.arguments.clone(),
                    cwd: workspace.to_owned(),
                    workspace: workspace.to_owned(),
                    stdio: ProcessStdio::Pipes,
                    mode: SandboxMode::ReadOnly,
                })
                .await
                .map_err(|_| Error::Unavailable)?;
            let process = self
                .process
                .spawn(DuplexProcessSpec {
                    process: confined,
                    environment: self
                        .config
                        .environment
                        .iter()
                        .map(|(k, v)| (k.into(), v.into()))
                        .collect(),
                    stdout_buffer_bytes: 1024 * 1024,
                    stderr_max_bytes: 32768,
                    termination_grace_ms: 200,
                })
                .map_err(|_| Error::Unavailable)?;
            *connection = Some(Connection::new(process, &self.config));
            connection
                .as_mut()
                .expect("inserted server")
                .initialize(workspace, &self.config)
                .await?;
        } else {
            connection.as_mut().expect("existing server").reset_budget();
        }
        let value = connection
            .as_mut()
            .expect("ready server")
            .query(
                query.operation,
                &source.uri,
                language,
                &source.text,
                source.position,
            )
            .await?;
        Ok(Output {
            query: query.clone(),
            result: protocol::normalize(workspace, query.operation, value)?,
        })
    }
    /// Read current source for a location viewer; this never spawns a language server.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned provider state.
    pub async fn current_file(
        self: &Arc<Self>,
        workspace: PathBuf,
        path: String,
        cancellation: CancellationToken,
    ) -> Result<String> {
        protocol::relative(&path)?;
        if !workspace.is_absolute() {
            return Err(Error::Invalid);
        }
        let stop = self.stop.child_token();
        let _cancel = stop.clone().drop_guard();
        let owner = self.clone();
        let worker_stop = stop.clone();
        let task = {
            let state = self.state.lock().expect("language viewer admission");
            if state.closed {
                return Err(Error::Retired);
            }
            let permit = self
                .reads
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::Capacity)?;
            self.execution.spawn(self.tasks.track_future(async move{let _permit=permit;tokio::select!{biased;()=worker_stop.cancelled()=>Err(Error::Cancelled),result=timeout(Duration::from_secs(30),source::read(&owner.files,&owner.sandbox,&workspace,&path,worker_stop.clone()))=>result.unwrap_or(Err(Error::Deadline))}}))
        };
        tokio::select! {biased;()=cancellation.cancelled()=>Err(Error::Cancelled),()=self.stop.cancelled()=>Err(Error::Retired),result=task=>result.map_err(|_|Error::Unavailable)?}
    }
    /// Whether this exact provider generation has withdrawn admission.
    pub fn retired(&self) -> bool {
        self.stop.is_cancelled()
    }
    /// Retire admission and join actual query tasks and subprocess settlement.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned provider state.
    pub async fn close(&self) -> Result<()> {
        {
            let mut state = self.state.lock().expect("language retirement");
            state.closed = true;
            self.stop.cancel();
            self.reads.close();
            self.tasks.close();
        }
        self.tasks.wait().await;
        let slots = {
            let mut state = self.state.lock().expect("language pool cleanup");
            state.live.clear();
            std::mem::take(&mut state.slots)
        };
        let mut result = Ok(());
        for slot in slots.into_values() {
            if let Some(connection) = slot.lock().await.take()
                && let Err(error) = connection.close().await
            {
                result = Err(error);
            }
        }
        result
    }
}
