use super::*;
use crate::tests::{UnknownThenAcceptedHandle, UnusedWorkspace};
use rsi_meta::{FiberState, ResolvedFactory, Runtime, UpdateMode};
use rsi_session_protocol::{
    CreateSession, RecentSessionCursor, RecentSessionPage, SessionError, SessionHandle,
    SessionService,
};
use std::sync::atomic::AtomicUsize;

#[derive(Clone, Debug, Default)]
struct BlockedRead {
    started: Arc<tokio::sync::Notify>,
    active: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for BlockedRead {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supplies = vec![
            plan.context()
                .provide_local::<SessionContract>(Arc::new(self.clone()))?,
            plan.context()
                .provide_local::<WorkspaceRegistryContract>(Arc::new(UnusedWorkspace))?,
            plan.context()
                .provide_local::<rsi_process::ProcessOutputCacheContract>(Arc::new(
                    UnknownThenAcceptedHandle::default(),
                ))?,
        ];
        plan.defer(
            "withdraw fixture capabilities",
            Box::new(move || {
                Box::pin(async move {
                    drop(supplies);
                    Ok(())
                })
            }),
        )
    }
}
struct Reading(Arc<AtomicUsize>);
impl Drop for Reading {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl SessionService for BlockedRead {
    async fn create(
        &self,
        _: CreateSession,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        Err(SessionError::NotFound("not used".into()))
    }
    async fn attach(
        &self,
        _: &rsi_agent_session_protocol::SessionId,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        Err(SessionError::NotFound("not used".into()))
    }
    async fn list_recent(
        &self,
        _: Option<&RecentSessionCursor>,
        _: usize,
    ) -> rsi_session_protocol::Result<RecentSessionPage> {
        self.active.fetch_add(1, Ordering::SeqCst);
        let _reading = Reading(self.active.clone());
        self.started.notify_one();
        std::future::pending().await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn application_owns_an_unpolled_entry_waiter_and_drains_reads_before_releasing_terminal() {
    let runtime = Runtime::default();
    let context = runtime.root();
    let source = Arc::new(BlockedRead::default());
    context
        .apply(
            ResolvedFactory::linked(
                "fixture",
                "test",
                UpdateMode::RestartRequired,
                source.clone(),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    for id in ["first-terminal", "second-terminal"] {
        let fiber = context
            .apply(
                ResolvedFactory::linked(
                    id,
                    "test",
                    UpdateMode::RestartRequired,
                    Arc::new(CliFactory::new(vec!["--list".into()])),
                ),
                ConfigValue::Null,
            )
            .await
            .unwrap();
        assert_eq!(fiber.snapshot().state, FiberState::Active);
        let application = context.lookup_local::<ApplicationRunContract>().unwrap();
        drop(application.clone().run());
        tokio::time::timeout(std::time::Duration::from_secs(2), source.started.notified())
            .await
            .unwrap();
        assert_eq!(source.active.load(Ordering::SeqCst), 1);
        assert!(matches!(
            application.clone().run().await,
            Err(ApplicationError::AlreadyStarted)
        ));
        assert!(fiber.dispose().await.is_clean());
        assert_eq!(source.active.load(Ordering::SeqCst), 0);
        assert!(context.lookup_local::<ApplicationRunContract>().is_none());
        assert!(matches!(
            application.run().await,
            Err(ApplicationError::ShuttingDown)
        ));
    }
    assert!(runtime.shutdown().await.is_clean());
}
