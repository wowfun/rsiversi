//! Actual caller binding installed before an exported controller can attach.
use async_trait::async_trait;
use rsi_agent_session_protocol::SessionId;
use rsi_api_protocol::CallOrigin;
use rsi_execution::{ExecutionLease, ExecutionResolver, ExecutionResolverContract};
use rsi_meta::{ActivationPlan, ConfigValue, PluginFactory, PreparedActivation};
use rsi_session_protocol::SessionContract;
use rsi_session_protocol::{
    SessionIngressContract, SessionReadContract, SessionReads, SessionService, SessionSource,
    SessionSourceContract, SessionSourceLease, SessionTarget,
};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub(super) struct SourceFactory {
    pub origin: CallOrigin,
    pub session: SessionId,
}
#[async_trait]
impl PluginFactory for SourceFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(super::no_config(config)?
            .requiring_local::<SessionIngressContract>()
            .requiring_local::<SessionReadContract>()
            .requiring_local::<ExecutionResolverContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let sessions = plan
            .local::<SessionIngressContract>()?
            .scoped(self.origin.clone());
        let source = Arc::new(Source {
            origin: self.origin.clone(),
            session: self.session.clone(),
            sessions: sessions.clone(),
            reads: plan.local::<SessionReadContract>()?,
            execution: plan.local::<ExecutionResolverContract>()?,
            lease: Mutex::default(),
        });
        let supplies = [
            plan.context().provide_local::<SessionContract>(sessions)?,
            plan.context()
                .provide_local::<SessionSourceContract>(source)?,
        ];
        plan.defer(
            "withdraw bound Session source",
            Box::new(move || {
                Box::pin(async move {
                    drop(supplies);
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct Source {
    origin: CallOrigin,
    session: SessionId,
    sessions: Arc<dyn SessionService>,
    reads: Arc<dyn SessionReads>,
    execution: Arc<dyn ExecutionResolver>,
    lease: Mutex<Option<ExecutionLease>>,
}
#[async_trait]
impl SessionSource for Source {
    async fn acquire(&self) -> rsi_session_protocol::Result<SessionSourceLease> {
        let handle = self.sessions.attach(&self.session).await?;
        let header = handle.header().await?;
        let target = SessionTarget {
            session_id: self.session.clone(),
            header_key: header
                .fingerprint()
                .map_err(|error| rsi_session_protocol::SessionError::Backend(error.to_string()))?,
        };
        let read = self.reads.acquire(self.origin.clone(), &target).await?;
        let candidate = self
            .execution
            .lease(self.origin.clone(), read.header().coordinates().location())
            .map_err(rsi_session_protocol::SessionError::Api)?;
        let selected = {
            let mut held = self.lease.lock().expect("Session source lease");
            if held.as_ref().is_none_or(|previous| {
                previous.binding().provider_generation()
                    != candidate.binding().provider_generation()
                    || previous.admit().is_err()
            }) {
                *held = Some(candidate);
            }
            held.as_ref().expect("selected source lease").clone()
        };
        SessionSourceLease::new(read, selected)
    }
}
