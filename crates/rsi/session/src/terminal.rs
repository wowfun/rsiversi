//! Session authorization and generation lifetime for independent terminal scopes.
use super::{Arc, LocalSessionHandle, PathBuf, Result, SessionError, SessionId, StoreError};
use rsi_pty_protocol::{PtyProvider, PtyScope};
use rsi_session_protocol::terminal::{Operation, PtyError, Reply, Request};
mod creation;
use std::{
    collections::BTreeMap,
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug)]
pub(super) struct Terminals {
    changed: tokio::sync::Notify,
    stopping: tokio::sync::Mutex<()>,
    provider: Arc<dyn PtyProvider>,
    sandbox: Arc<dyn rsi_sandbox::Sandbox>,
    state: StdMutex<Registry>,
}
#[derive(Debug, Default)]
struct Registry {
    stopped: bool,
    retired: bool,
    failure: Option<PtyError>,
    scopes: BTreeMap<SessionId, Arc<ScopeEntry>>,
    creations: BTreeMap<creation::Identity, creation::Receipt>,
}
#[derive(Debug)]
struct ScopeEntry {
    scope: Arc<dyn PtyScope>,
    mutation: tokio::sync::Mutex<()>,
    operations: AtomicUsize,
}
impl ScopeEntry {
    fn new(scope: Arc<dyn PtyScope>) -> Arc<Self> {
        Arc::new(Self {
            scope,
            mutation: tokio::sync::Mutex::new(()),
            operations: AtomicUsize::new(0),
        })
    }
}
struct ScopeLease<'a> {
    owner: &'a Terminals,
    session: SessionId,
    entry: Arc<ScopeEntry>,
}
impl Drop for ScopeLease<'_> {
    fn drop(&mut self) {
        let mut state = self
            .owner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.entry.operations.fetch_sub(1, Ordering::AcqRel) == 1
            && self.entry.scope.is_empty()
            && state
                .scopes
                .get(&self.session)
                .is_some_and(|entry| Arc::ptr_eq(entry, &self.entry))
        {
            state.scopes.remove(&self.session);
        }
    }
}
impl Terminals {
    fn running(&self) -> Result<std::sync::MutexGuard<'_, Registry>> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.stopped {
            return Err(SessionError::ShuttingDown);
        }
        Ok(state)
    }
    pub(super) fn busy(&self, session: &SessionId) -> Result<bool> {
        Ok(self.running()?.scopes.get(session).is_some_and(|entry| {
            entry.operations.load(Ordering::Acquire) != 0 || !entry.scope.is_empty()
        }))
    }

    pub(super) fn new(
        provider: Arc<dyn PtyProvider>,
        sandbox: Arc<dyn rsi_sandbox::Sandbox>,
    ) -> Self {
        Self {
            changed: tokio::sync::Notify::new(),
            stopping: tokio::sync::Mutex::new(()),
            provider,
            sandbox,
            state: StdMutex::new(Registry::default()),
        }
    }
    fn scope(&self, session: &SessionId, create: bool) -> Result<Option<ScopeLease<'_>>> {
        let mut state = self.running()?;
        self.scope_locked(&mut state, session, create)
    }
    fn scope_locked(
        &self,
        state: &mut Registry,
        session: &SessionId,
        create: bool,
    ) -> Result<Option<ScopeLease<'_>>> {
        let entry = if let Some(entry) = state.scopes.get(session) {
            entry.clone()
        } else {
            if !create {
                return Ok(None);
            }
            if state.scopes.len() >= 256 {
                return Err(SessionError::Capacity);
            }
            let entry = ScopeEntry::new(self.provider.scope()?);
            state.scopes.insert(session.clone(), entry.clone());
            entry
        };
        entry.operations.fetch_add(1, Ordering::Relaxed);
        Ok(Some(ScopeLease {
            owner: self,
            session: session.clone(),
            entry,
        }))
    }
    async fn operate(
        &self,
        session: &SessionId,
        operation: Operation,
        execution: Option<rsi_execution::ExecutionLease>,
    ) -> Result<Reply> {
        let Some(scope) = self.scope(session, false)? else {
            return match operation {
                Operation::List => Ok(Reply::List(vec![])),
                Operation::CloseAll | Operation::Close { .. } => Ok(Reply::Done),
                _ => Err(unavailable(
                    "live terminal ID is unavailable in this service generation",
                )),
            };
        };
        let closing = matches!(operation, Operation::Close { .. } | Operation::CloseAll);
        let _mutation = if closing {
            Some(scope.entry.mutation.lock().await)
        } else {
            None
        };
        let result = match execution {
            Some(execution) => scope.entry.scope.execute_with(operation, execution).await,
            None => scope.entry.scope.execute(operation).await,
        };
        if closing {
            self.changed.notify_waiters();
        }
        Ok(result?)
    }
    pub(super) async fn stop(&self) -> Result<()> {
        let _stopping = self.stopping.lock().await;
        let scopes = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.retired {
                return state
                    .failure
                    .clone()
                    .map_or(Ok(()), |error| Err(error.into()));
            }
            state.stopped = true;
            self.changed.notify_waiters();
            state
                .scopes
                .values()
                .map(|entry| entry.scope.clone())
                .collect::<Vec<_>>()
        };
        // Every scope is attempted even if an earlier native reap reports an error.
        let mut failure = None;
        for scope in scopes {
            if let Err(error) = scope.retire().await {
                failure.get_or_insert(error);
            }
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.failure.clone_from(&failure);
        state.scopes.clear();
        state.retired = true;
        failure.map_or(Ok(()), |error| Err(error.into()))
    }
}
impl LocalSessionHandle {
    pub(super) async fn borrow_attachment(
        &self,
        request: Request,
    ) -> Result<Box<dyn rsi_session_protocol::terminal::AttachmentLease>> {
        if !matches!(
            request,
            Request::Create { .. }
                | Request::Operate {
                    operation: Operation::Attach { .. }
                }
        ) {
            return Err(PtyError::Invalid("expected an attachment operation".into()).into());
        }
        let create = matches!(request, Request::Create { .. });
        let Reply::Attached(attachment) = self.terminal_request(request).await? else {
            return Err(PtyError::OutcomeUnknown.into());
        };
        let scope = self
            .terminals
            .as_ref()
            .ok_or_else(|| unavailable("terminal provider is absent"))?
            .scope(&self.session_id, false)?
            .ok_or_else(|| {
                if create {
                    SessionError::TerminalKeyRetired
                } else {
                    unavailable("terminal scope retired")
                }
            })?;
        Ok(Box::new(creation::Held {
            scope: scope.entry.scope.clone(),
            attachment,
        }))
    }
    fn terminal_owner(&self) -> Option<&rsi_api_protocol::DeviceId> {
        match &self.origin {
            rsi_api_protocol::CallOrigin::Local => None,
            rsi_api_protocol::CallOrigin::Device(device) => Some(&device.id),
        }
    }
    pub(super) async fn read_terminal_catalog(
        &self,
    ) -> Result<rsi_session_protocol::terminal::Catalog> {
        let _admission = self.admit()?;
        let _activity = self.begin_read()?;
        let terminals = self
            .terminals
            .as_ref()
            .ok_or_else(|| unavailable("terminal provider is absent"))?;
        let header = self.header_snapshot().await?;
        let cwd = PathBuf::from(header.canonical_cwd());
        let local = header.coordinates().location() == &rsi_execution::ExecutionLocation::Local;
        terminal_mode(local, header.settings().sandbox())?;
        let shell = manual_shell(cfg!(windows) && local, &cwd)?;
        let bindings = terminals
            .bindings(&self.session_id, self.terminal_owner())
            .await?;
        Ok(rsi_session_protocol::terminal::Catalog {
            program: shell.program,
            arguments: shell.arguments,
            cwd,
            bindings,
        })
    }
    pub(super) async fn wait_for_terminal(&self, key: &str) -> Result<()> {
        let _admission = self.admit()?;
        let _activity = self.begin_read()?;
        let terminals = self
            .terminals
            .as_ref()
            .ok_or_else(|| unavailable("terminal provider is absent"))?;
        loop {
            let changed = terminals.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !terminals
                .bindings(&self.session_id, self.terminal_owner())
                .await?
                .iter()
                .any(|row| row.key == key)
            {
                return Ok(());
            }
            changed.await;
        }
    }
    pub(super) async fn terminal_request(&self, request: Request) -> Result<Reply> {
        let admission = self.admit_mutation()?;
        let activity = self.begin_activity()?;
        request.validate()?;
        if self.projection_stopped.is_cancelled() {
            return Err(SessionError::ShuttingDown);
        }
        let terminals = self
            .terminals
            .as_ref()
            .ok_or_else(|| unavailable("terminal provider is absent"))?;
        let (key, create) = match request {
            Request::Create { key, size } => {
                let header = self.header_snapshot().await?;
                let persisted =
                    self.store
                        .header(&self.session_id)
                        .await
                        .map_err(|error| match error {
                            StoreError::NotFound(_) => {
                                unavailable("persist this Session before opening a terminal")
                            }
                            error => SessionError::Backend(error.to_string()),
                        })?;
                if persisted != *header {
                    return Err(unavailable("Session Header is no longer current"));
                }
                let launch = self.prepare_terminal(terminals, &header, size).await?;
                (key, Some((size, launch)))
            }
            Request::CloseKey { key } => (key, None),
            Request::Operate { operation } => {
                let execution = if matches!(
                    operation,
                    Operation::Input { .. } | Operation::Resize { .. }
                ) {
                    self.execution_lease()?
                } else {
                    None
                };
                return terminals
                    .operate(&self.session_id, operation, execution)
                    .await;
            }
        };
        let identity = (self.session_id.clone(), self.terminal_owner().cloned(), key);
        let terminals = terminals.clone();
        // The admitted key mutation settles even if its caller loses the reply.
        tokio::runtime::Handle::try_current()
            .map_err(|_| unavailable("terminal creation keys require a runtime"))?
            .spawn(async move {
                let _guards = (admission, activity);
                match create {
                    Some((size, launch)) => terminals.create_keyed(identity, size, launch).await,
                    None => terminals.close_keyed(identity).await,
                }
            })
            .await
            .map_err(|_| PtyError::OutcomeUnknown)?
    }
}
enum TerminalLaunch {
    Native(rsi_process::PtyProcessSpec),
    Execution(rsi_process::PtyProcessSpec<rsi_execution::PreparedProcess>),
}
impl LocalSessionHandle {
    async fn prepare_terminal(
        &self,
        terminals: &Terminals,
        header: &super::SessionHeader,
        size: rsi_pty_protocol::Size,
    ) -> Result<TerminalLaunch> {
        let workspace = PathBuf::from(header.canonical_cwd());
        let lease = self.execution_lease()?;
        let local = lease.as_ref().is_none_or(|lease| {
            *lease.binding().location() == rsi_execution::ExecutionLocation::Local
        });
        let windows = cfg!(windows) && local;
        let mode = terminal_mode(local, header.settings().sandbox())?;
        let shell = manual_shell(windows, &workspace)?;
        let environment = shell.environment;
        let request = rsi_sandbox::ProcessRequest {
            stdio: rsi_sandbox::ProcessStdio::Pty,
            mode,
            program: shell.program,
            arguments: shell.arguments,
            cwd: workspace.clone(),
            workspace,
        };
        if let Some(lease) = lease {
            let coordinates = lease
                .canonicalize(header.canonical_cwd())
                .await
                .map_err(|_| unavailable("terminal workspace is unavailable on its target"))?;
            if &coordinates != header.coordinates() {
                return Err(unavailable("terminal workspace changed on its target"));
            }
            let program = if *lease.binding().location() == rsi_execution::ExecutionLocation::Local
            {
                lease
                    .resolve_local_program(rsi_execution::ResolvedProgram {
                        program: request.program.clone(),
                        environment,
                    })
                    .await
            } else {
                lease.resolve_program("terminal").await
            }
            .map_err(|_| unavailable("terminal shell is unavailable on its target"))?;
            let process = lease
                .prepare(request.map_program(|_| program))
                .await
                .map_err(|_| {
                    unavailable("the target sandbox cannot provide a controlling terminal")
                })?;
            let environment = process.environment().to_vec();
            Ok(TerminalLaunch::Execution(rsi_process::PtyProcessSpec {
                process,
                environment,
                size: size.native(),
                termination_grace_ms: 250,
            }))
        } else {
            self.prepare_workspace(header).await?;
            let process = terminals.sandbox.confine(request).await.map_err(|_| {
                unavailable("the frozen sandbox cannot provide a controlling terminal")
            })?;
            Ok(TerminalLaunch::Native(rsi_process::PtyProcessSpec {
                process,
                environment,
                size: size.native(),
                termination_grace_ms: 250,
            }))
        }
    }
}
struct ManualShell {
    program: PathBuf,
    arguments: Vec<String>,
    environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}
fn manual_shell(windows: bool, workspace: &std::path::Path) -> Result<ManualShell> {
    if windows {
        let (program, environment) = windows_shell()?;
        return Ok(ManualShell {
            program,
            arguments: vec!["-NoLogo".into(), "-NoProfile".into()],
            environment,
        });
    }
    Ok(ManualShell {
        program: "/bin/bash".into(),
        arguments: vec!["--noprofile".into(), "--norc".into(), "-i".into()],
        environment: vec![
            ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
            ("HOME".into(), workspace.as_os_str().into()),
            ("SHELL".into(), "/bin/bash".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("HISTFILE".into(), "/dev/null".into()),
        ],
    })
}

fn unavailable(message: &str) -> SessionError {
    PtyError::Unavailable(message.into()).into()
}

fn terminal_mode(local: bool, agent: rsi_sandbox::SandboxMode) -> Result<rsi_sandbox::SandboxMode> {
    use rsi_sandbox::SandboxMode;
    if local && cfg!(windows) {
        return Ok(SandboxMode::DangerFullAccess);
    }
    if local && !cfg!(target_os = "linux")
        || !matches!(agent, SandboxMode::ReadOnly | SandboxMode::WorkspaceWrite)
    {
        return Err(unavailable(
            "this target requires Linux Bash with ReadOnly or WorkspaceWrite Bubblewrap",
        ));
    }
    Ok(agent)
}

// Product-owned shell selection: never copy the Host's credential environment.
fn windows_shell() -> Result<(PathBuf, Vec<(std::ffi::OsString, std::ffi::OsString)>)> {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|root| root.is_absolute())
        .ok_or_else(|| unavailable("Windows SystemRoot is unavailable"))?;
    let program = root.join("System32/WindowsPowerShell/v1.0/powershell.exe");
    if !program.is_file() {
        return Err(unavailable("Windows PowerShell is unavailable"));
    }
    let environment = WINDOWS_TERMINAL_ENV
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| ((*name).into(), value)))
        .chain(std::iter::once(("TERM".into(), "xterm-256color".into())))
        .collect();
    Ok((program, environment))
}
const WINDOWS_TERMINAL_ENV: &[&str] = &[
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "PATH",
    "PATHEXT",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
    "HOMEDRIVE",
    "HOMEPATH",
    "USERNAME",
    "USERDOMAIN",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramData",
    "PSModulePath",
];

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    mod keyed;
    #[test]
    fn manual_terminal_policy_is_local_to_the_target() {
        use rsi_sandbox::SandboxMode;
        for agent in [
            SandboxMode::ReadOnly,
            SandboxMode::WorkspaceWrite,
            SandboxMode::DangerFullAccess,
        ] {
            if cfg!(windows) {
                assert_eq!(
                    terminal_mode(true, agent).unwrap(),
                    SandboxMode::DangerFullAccess
                );
            }
            if agent == SandboxMode::DangerFullAccess {
                assert!(terminal_mode(false, agent).is_err());
            } else {
                assert_eq!(terminal_mode(false, agent).unwrap(), agent);
            }
        }
    }
    #[cfg(windows)]
    #[test]
    fn windows_terminal_selects_native_shell_and_only_noncredential_environment() {
        let (program, environment) = windows_shell().unwrap();
        assert!(program.is_absolute());
        assert_eq!(program.file_name().unwrap(), "powershell.exe");
        assert!(environment.iter().all(|(name, _)| name == "TERM"
            || WINDOWS_TERMINAL_ENV.iter().any(|allowed| name == allowed)));
        assert!(
            !environment
                .iter()
                .any(|(name, _)| name.to_string_lossy().contains("KEY")
                    || name.to_string_lossy().contains("TOKEN"))
        );
    }

    #[derive(Debug)]
    struct Scope {
        empty: AtomicBool,
        calls: AtomicUsize,
        hold: AtomicBool,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        fail: bool,
    }
    impl Scope {
        fn new(hold: bool, fail: bool) -> Arc<Self> {
            Arc::new(Self {
                empty: AtomicBool::new(true),
                calls: AtomicUsize::new(0),
                hold: AtomicBool::new(hold),
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
                fail,
            })
        }
    }
    #[async_trait]
    impl PtyScope for Scope {
        fn release_attachment(&self, _terminal: &str, _attachment: &str) {}
        async fn create_execution(
            &self,
            _: rsi_process::PtyProcessSpec<rsi_execution::PreparedProcess>,
        ) -> rsi_pty_protocol::Result<rsi_pty_protocol::Attachment> {
            unreachable!()
        }
        async fn execute_with(
            &self,
            _: Operation,
            _: rsi_execution::ExecutionLease,
        ) -> rsi_pty_protocol::Result<Reply> {
            unreachable!()
        }
        fn is_empty(&self) -> bool {
            self.empty.load(Ordering::Acquire)
        }
        async fn create(
            &self,
            spec: rsi_process::PtyProcessSpec,
        ) -> rsi_pty_protocol::Result<rsi_pty_protocol::Attachment> {
            self.release.notified().await;
            assert!(self.empty.swap(false, Ordering::AcqRel));
            Ok(rsi_pty_protocol::Attachment {
                terminal: rsi_pty_protocol::Terminal {
                    id: "shell".into(),
                    size: rsi_pty_protocol::Size {
                        rows: spec.size.rows,
                        columns: spec.size.columns,
                    },
                    sandbox: spec.process.stamp.requested,
                    phase: rsi_pty_protocol::Phase::Running,
                    controller: Some("writer".into()),
                    controller_epoch: 1,
                },
                id: "writer".into(),
                stream_epoch: 1,
            })
        }
        async fn execute(&self, operation: Operation) -> rsi_pty_protocol::Result<Reply> {
            assert!(
                matches!(operation, Operation::Close { .. } | Operation::CloseAll),
                "cleanup must not issue List"
            );
            self.empty.store(true, Ordering::Release);
            if self.fail {
                Err(PtyError::Io("fixture reap failure".into()))
            } else {
                Ok(Reply::Done)
            }
        }
        async fn retire(&self) -> rsi_pty_protocol::Result<()> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            self.entered.notify_one();
            loop {
                let release = self.release.notified();
                if !self.hold.load(Ordering::Acquire) {
                    break;
                }
                release.await;
            }
            if self.fail {
                Err(PtyError::Io("fixture reap failure".into()))
            } else {
                Ok(())
            }
        }
    }
    #[derive(Debug)]
    struct Unused;
    impl PtyProvider for Unused {
        fn scope(&self) -> rsi_pty_protocol::Result<Arc<dyn PtyScope>> {
            Ok(Scope::new(false, false))
        }
    }
    #[async_trait]
    impl rsi_sandbox::Sandbox for Unused {
        async fn workspace_read(
            &self,
            _: rsi_sandbox::WorkspaceReadRequest,
        ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
            unreachable!()
        }
        async fn confine(
            &self,
            _: rsi_sandbox::ProcessRequest,
        ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
            unreachable!()
        }
    }
    fn registry(first: Arc<Scope>, second: Arc<Scope>) -> Arc<Terminals> {
        let registry = Arc::new(Terminals::new(Arc::new(Unused), Arc::new(Unused)));
        for (id, scope) in [("first", first), ("second", second)] {
            registry
                .state
                .lock()
                .unwrap()
                .scopes
                .insert(SessionId::new(id).unwrap(), ScopeEntry::new(scope));
        }
        registry
    }

    #[tokio::test]
    async fn retirement_attempts_all_scopes_and_retains_the_first_failure() {
        let first = Scope::new(false, true);
        let second = Scope::new(false, false);
        let registry = registry(first.clone(), second.clone());
        for _ in 0..2 {
            assert!(
                matches!(registry.stop().await, Err(SessionError::Terminal(PtyError::Io(message))) if message == "fixture reap failure")
            );
        }
        assert_eq!(first.calls.load(Ordering::Acquire), 1);
        assert_eq!(second.calls.load(Ordering::Acquire), 1);
        assert!(registry.state.lock().unwrap().scopes.is_empty());
        assert!(matches!(
            registry.busy(&SessionId::new("unrelated").unwrap()),
            Err(SessionError::ShuttingDown)
        ));
    }

    #[tokio::test]
    async fn cancelled_retirement_retains_scopes_until_a_retry_finishes() {
        let first = Scope::new(true, false);
        let second = Scope::new(false, false);
        let registry = registry(first.clone(), second.clone());
        let task = {
            let registry = registry.clone();
            tokio::spawn(async move { registry.stop().await })
        };
        first.entered.notified().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(registry.state.lock().unwrap().stopped);
        assert_eq!(registry.state.lock().unwrap().scopes.len(), 2);
        tokio::select! { biased;
            result = registry.stop() => panic!("retirement returned before cleanup: {result:?}"),
            () = std::future::ready(()) => {},
        }
        first.hold.store(false, Ordering::Release);
        first.release.notify_waiters();
        registry.stop().await.unwrap();
        assert_eq!(second.calls.load(Ordering::Acquire), 1);
        assert!(registry.state.lock().unwrap().scopes.is_empty());
    }
    #[test]
    fn incidental_entry_clone_does_not_retain_an_empty_scope() {
        let registry = Terminals::new(Arc::new(Unused), Arc::new(Unused));
        let id = SessionId::new("empty").unwrap();
        let lease = registry.scope(&id, true).unwrap().unwrap();
        let retained = lease.entry.clone();
        drop(lease);
        assert!(registry.state.lock().unwrap().scopes.is_empty());
        drop(retained);
    }

    #[tokio::test]
    async fn closing_releases_empty_scopes_even_after_errors_without_listing() {
        for fail in [false, true] {
            let registry = Terminals::new(Arc::new(Unused), Arc::new(Unused));
            let id = SessionId::new("closed").unwrap();
            let backing = Scope::new(false, fail);
            backing.empty.store(false, Ordering::Release);
            registry
                .state
                .lock()
                .unwrap()
                .scopes
                .insert(id.clone(), ScopeEntry::new(backing));
            assert_eq!(
                registry
                    .operate(&id, Operation::CloseAll, None)
                    .await
                    .is_err(),
                fail
            );
            assert!(registry.state.lock().unwrap().scopes.is_empty());
        }
    }

    #[test]
    fn empty_scopes_reclaim_capacity_only_after_the_last_operation() {
        let registry = Terminals::new(Arc::new(Unused), Arc::new(Unused));
        for index in 0..300 {
            let id = SessionId::new(format!("session-{index}")).unwrap();
            let first = registry.scope(&id, true).unwrap().unwrap();
            let second = registry.scope(&id, false).unwrap().unwrap();
            drop(first);
            assert_eq!(registry.state.lock().unwrap().scopes.len(), 1);
            drop(second);
            assert!(registry.state.lock().unwrap().scopes.is_empty());
        }
        let id = SessionId::new("retained").unwrap();
        let backing = Scope::new(false, false);
        backing.empty.store(false, Ordering::Release);
        registry
            .state
            .lock()
            .unwrap()
            .scopes
            .insert(id.clone(), ScopeEntry::new(backing.clone()));
        let lease = registry.scope(&id, true).unwrap().unwrap();
        drop(lease);
        assert!(registry.scope(&id, false).unwrap().is_some());
        let lease = registry.scope(&id, false).unwrap().unwrap();
        backing.empty.store(true, Ordering::Release);
        drop(lease);
        assert!(registry.state.lock().unwrap().scopes.is_empty());
    }
}
