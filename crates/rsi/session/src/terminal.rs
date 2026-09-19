//! Session authorization and generation lifetime for independent terminal scopes.
use super::{Arc, LocalSessionHandle, PathBuf, Result, SessionError, SessionId, StoreError};
use rsi_pty_protocol::{PtyProvider, PtyScope};
use rsi_session_protocol::terminal::{Operation, PtyError, Reply, Request};
use std::{
    collections::BTreeMap,
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug)]
pub(super) struct Terminals {
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
    pub(super) fn new(
        provider: Arc<dyn PtyProvider>,
        sandbox: Arc<dyn rsi_sandbox::Sandbox>,
    ) -> Self {
        Self {
            stopping: tokio::sync::Mutex::new(()),
            provider,
            sandbox,
            state: StdMutex::new(Registry::default()),
        }
    }
    fn scope(&self, session: &SessionId, create: bool) -> Result<Option<ScopeLease<'_>>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.stopped {
            return Err(SessionError::ShuttingDown);
        }
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
    async fn operate(&self, session: &SessionId, operation: Operation) -> Result<Reply> {
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
        let result = scope.entry.scope.execute(operation).await;
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
    pub(super) async fn terminal_request(&self, request: Request) -> Result<Reply> {
        request.validate()?;
        if self.projection_stopped.is_cancelled() {
            return Err(SessionError::ShuttingDown);
        }
        let terminals = self
            .terminals
            .as_ref()
            .ok_or_else(|| unavailable("terminal provider is absent"))?;
        match request {
            Request::Create { size } => {
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
                if !cfg!(target_os = "linux")
                    || !matches!(
                        header.settings().sandbox(),
                        rsi_sandbox::SandboxMode::ReadOnly
                            | rsi_sandbox::SandboxMode::WorkspaceWrite
                    )
                {
                    return Err(unavailable(
                        "Linux Bash with ReadOnly or WorkspaceWrite Bubblewrap is required",
                    ));
                }

                self.prepare_workspace(&header).await?;
                let workspace = PathBuf::from(header.canonical_cwd());
                let process = terminals
                    .sandbox
                    .confine(rsi_sandbox::ProcessRequest {
                        stdio: rsi_sandbox::ProcessStdio::Pty,
                        mode: header.settings().sandbox(),
                        program: "/bin/bash".into(),
                        arguments: vec!["--noprofile".into(), "--norc".into(), "-i".into()],
                        cwd: workspace.clone(),
                        workspace: workspace.clone(),
                    })
                    .await
                    .map_err(|_| {
                        unavailable("the frozen sandbox cannot provide a controlling terminal")
                    })?;
                let scope = terminals
                    .scope(&self.session_id, true)?
                    .expect("new scope requested");
                let _mutation = scope.entry.mutation.lock().await;
                let attachment = scope.entry.scope.create(rsi_process::PtyProcessSpec {
                    process,
                    size: size.native(),
                    termination_grace_ms: 250,
                    environment: vec![
                        ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
                        ("HOME".into(), workspace.into_os_string()),
                        ("SHELL".into(), "/bin/bash".into()),
                        ("TERM".into(), "xterm-256color".into()),
                        ("LANG".into(), "C.UTF-8".into()),
                        ("HISTFILE".into(), "/dev/null".into()),
                    ],
                })?;
                Ok(Reply::Attached(attachment))
            }
            Request::Operate { operation } => terminals.operate(&self.session_id, operation).await,
        }
    }
}
fn unavailable(message: &str) -> SessionError {
    PtyError::Unavailable(message.into()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
        fn is_empty(&self) -> bool {
            self.empty.load(Ordering::Acquire)
        }
        fn create(
            &self,
            _: rsi_process::PtyProcessSpec,
        ) -> rsi_pty_protocol::Result<rsi_pty_protocol::Attachment> {
            unreachable!()
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
                registry.operate(&id, Operation::CloseAll).await.is_err(),
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
