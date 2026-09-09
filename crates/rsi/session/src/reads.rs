use super::{LocalSessionService, Result, SessionError};
use async_trait::async_trait;

#[async_trait]
impl rsi_session_protocol::SessionReads for LocalSessionService {
    async fn acquire(
        &self,
        target: &rsi_session_protocol::SessionTarget,
    ) -> Result<rsi_session_protocol::SessionReadLease> {
        target.validate()?;
        self.drafts.accepting()?;
        let handle = self.attach_local(&target.session_id).await?;
        let activity = handle.begin_activity()?;
        handle.reconcile_fresh_read().await?;
        let header = handle.header_snapshot().await?;
        if header.session_id() != &target.session_id
            || header
                .fingerprint()
                .map_err(|error| SessionError::Backend(error.to_string()))?
                != target.header_key
        {
            return Err(SessionError::NotFound(
                "Session Header no longer matches".into(),
            ));
        }
        if self.projection_stopped.is_cancelled() {
            return Err(SessionError::ShuttingDown);
        }
        Ok(rsi_session_protocol::SessionReadLease::new(
            header,
            self.projection_stopped.clone(),
            activity,
        ))
    }
}
