use super::{Arc, CancellationToken, Connection, SessionId, json};
use tokio::sync::OwnedMutexGuard;

/// Owns the mirror transaction even when its caller is dropped during a CAS read.
pub(super) struct Load {
    connection: Arc<Connection>,
    session: SessionId,
    lane: Option<OwnedMutexGuard<()>>,
    runtime: tokio::runtime::Handle,
}
impl Load {
    pub(super) fn new(
        connection: Arc<Connection>,
        session: SessionId,
        lane: OwnedMutexGuard<()>,
    ) -> Self {
        Self {
            connection,
            session,
            lane: Some(lane),
            runtime: tokio::runtime::Handle::current(),
        }
    }
    pub(super) fn finish(mut self) {
        self.lane.take();
    }
    pub(super) fn abort(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let lane = self.lane.take()?;
        let connection = self.connection.clone();
        let session = self.session.clone();
        Some(self.runtime.spawn(async move {
            let _lane = lane;
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                connection.request(
                    "agent.load.abort",
                    json!({"session_id": session}),
                    CancellationToken::new(),
                ),
            )
            .await;
        }))
    }
}
impl Drop for Load {
    fn drop(&mut self) {
        self.abort();
    }
}
