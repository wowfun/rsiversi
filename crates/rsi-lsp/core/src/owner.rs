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
    retiring: BTreeMap<PathBuf, Arc<AsyncMutex<Connection>>>,
}
struct Source {
    text: String,
    position: crate::Position,
    uri: String,
}
struct QueryWork {
    workspace: PathBuf,
    query: Query,
    language: String,
    stop: CancellationToken,
    deadline: Instant,
}
impl QueryWork {
    fn check(&self) -> Result<()> {
        if self.stop.is_cancelled() {
            Err(Error::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(Error::Deadline)
        } else {
            Ok(())
        }
    }
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
        let deadline = Instant::now() + Duration::from_secs(30);
        let task = {
            let mut state = self.state.lock().expect("language admission");
            if state.closed {
                return Err(Error::Retired);
            }
            if state.retiring.contains_key(&workspace) {
                return Err(Error::Capacity);
            }
            let permit = self
                .reads
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::Capacity)?;
            let slot = state.slots.entry(workspace.clone()).or_default().clone();
            let mut connection = slot.try_lock_owned().map_err(|_| Error::Capacity)?;
            self.execution.spawn(self.tasks.track_future(async move {
                let _permit = permit;
                owner
                    .execute_query(
                        QueryWork {
                            workspace,
                            query,
                            language,
                            stop: work_stop,
                            deadline,
                        },
                        &mut connection,
                    )
                    .await
            }))
        };
        tokio::select! {biased;()=cancellation.cancelled()=>Err(Error::Cancelled),()=self.stop.cancelled()=>Err(Error::Retired),result=timeout_at(deadline, task)=>result.map_err(|_|Error::Deadline)?.map_err(|_|Error::Unavailable)?}
    }
    fn check_work(&self, work: &QueryWork) -> Result<()> {
        work.check()?;
        if self.retired() {
            Err(Error::Retired)
        } else {
            Ok(())
        }
    }
    fn forget(&self, workspace: &std::path::Path) {
        let mut state = self.state.lock().expect("language slot cleanup");
        state.slots.remove(workspace);
        state.live.remove(workspace);
    }

    async fn execute_query(
        &self,
        work: QueryWork,
        connection: &mut Option<Connection>,
    ) -> Result<Output> {
        self.retire_failed(&work.workspace, connection).await?;
        let source = tokio::select! { biased;
            () = work.stop.cancelled() => Err(Error::Cancelled),
            result = timeout_at(work.deadline, self.source(&work.workspace, &work.query, &work.stop)) => result.unwrap_or(Err(Error::Deadline)),
        };
        let source = match source {
            Ok(source) => source,
            Err(error) => {
                // Preserve the source error while still joining an idle failure
                // that occurred during source validation.
                let _cleanup = self.retire_failed(&work.workspace, connection).await;
                if connection.is_none() {
                    self.forget(&work.workspace);
                }
                return Err(error);
            }
        };
        if let Err(error) = self.check_work(&work) {
            if connection.is_none() {
                self.forget(&work.workspace);
            }
            return Err(error);
        }
        // Only authoritative, valid source may evict an idle process. Admitted
        // cleanup stays owned even after the caller's deadline or cancellation.
        let retired = match self.evict_idle(&work.workspace) {
            Ok(retired) => retired,
            Err(error) => {
                self.forget(&work.workspace);
                return Err(error);
            }
        };
        for (workspace, previous) in retired {
            if let Err(error) = self.join_retirement(&workspace, &previous).await {
                self.forget(&work.workspace);
                return Err(error);
            }
        }
        if connection.as_ref().is_some_and(Connection::failed) {
            let failed = connection.take().expect("failed connection");
            if let Err(error) = self.retire(&work.workspace, failed).await {
                self.forget(&work.workspace);
                return Err(error);
            }
        }
        let result = tokio::select! { biased;
            () = work.stop.cancelled() => Err(Error::Cancelled),
            result = timeout_at(work.deadline, self.run(&work, connection, &source)) => result.unwrap_or(Err(Error::Deadline)),
        };
        if result.is_err() {
            if let Some(failed) = connection.take() {
                // Primary query classification remains authoritative. Failed cleanup
                // withdraws admission and stays observable through provider close.
                let _cleanup = self.retire(&work.workspace, failed).await;
            }
            self.forget(&work.workspace);
        }
        result
    }

    async fn retire_failed(
        &self,
        workspace: &std::path::Path,
        connection: &mut Option<Connection>,
    ) -> Result<()> {
        if connection.as_ref().is_some_and(Connection::failed) {
            let failed = connection.take().expect("failed connection");
            let result = self.retire(workspace, failed).await;
            if result.is_err() {
                self.forget(workspace);
            }
            result?;
        }
        Ok(())
    }
    async fn retire(&self, workspace: &std::path::Path, connection: Connection) -> Result<()> {
        let retained = Arc::new(AsyncMutex::new(connection));
        self.state
            .lock()
            .expect("language retirement")
            .retiring
            .insert(workspace.to_owned(), retained.clone());
        self.join_retirement(workspace, &retained).await
    }
    async fn join_retirement(
        &self,
        workspace: &std::path::Path,
        retained: &Arc<AsyncMutex<Connection>>,
    ) -> Result<()> {
        let result = retained.lock().await.close().await;
        let mut state = self.state.lock().expect("language retirement");
        if result.is_ok() {
            if state
                .retiring
                .get(workspace)
                .is_some_and(|item| Arc::ptr_eq(item, retained))
            {
                state.retiring.remove(workspace);
            }
        } else {
            state.closed = true;
            self.reads.close();
        }
        result
    }

    fn evict_idle(
        &self,
        workspace: &std::path::Path,
    ) -> Result<Vec<(PathBuf, Arc<AsyncMutex<Connection>>)>> {
        let mut state = self.state.lock().expect("language pool replacement");
        let mut retired = Vec::new();
        if state.live.contains(workspace) {
            return Ok(retired);
        }
        if !state.retiring.is_empty() {
            return Err(Error::Capacity);
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
                let retained = Arc::new(AsyncMutex::new(connection));
                state.retiring.insert(path.clone(), retained.clone());
                retired.push((path.clone(), retained));
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
        work: &QueryWork,
        connection: &mut Option<Connection>,
        source: &Source,
    ) -> Result<Output> {
        self.check_work(work)?;
        let QueryWork {
            workspace,
            query,
            language,
            deadline,
            ..
        } = work;
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
            self.check_work(work)?;
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
            *connection = Some(Connection::new(
                process,
                &self.config,
                &self.execution,
                self.stop.child_token(),
            ));
            connection
                .as_ref()
                .expect("inserted server")
                .begin(*deadline)
                .await?;
            connection
                .as_mut()
                .expect("inserted server")
                .initialize(workspace, &self.config)
                .await?;
        } else {
            connection
                .as_ref()
                .expect("existing server")
                .begin(*deadline)
                .await?;
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
        connection.as_ref().expect("ready server").end().await?;
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
        self.reads.is_closed()
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
        let (slots, retiring) = {
            let mut state = self.state.lock().expect("language pool cleanup");
            state.live.clear();
            (
                state
                    .slots
                    .iter()
                    .map(|(path, slot)| (path.clone(), slot.clone()))
                    .collect::<Vec<_>>(),
                state.retiring.clone(),
            )
        };
        let mut result = Ok(());
        for (workspace, retained) in retiring {
            if let Err(error) = self.join_retirement(&workspace, &retained).await {
                result = Err(error);
            }
        }
        for (workspace, slot) in slots {
            if let Some(connection) = slot.lock().await.take()
                && let Err(error) = self.retire(&workspace, connection).await
            {
                result = Err(error);
            }
        }
        self.state
            .lock()
            .expect("language pool cleanup")
            .slots
            .clear();
        result
    }
}
