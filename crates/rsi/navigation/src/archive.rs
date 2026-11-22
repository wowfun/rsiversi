use super::{ApiError, Arc, CallOrigin, Navigation, Result, SessionId};
use rsi_agent_session_protocol::SessionHeader;
use rsi_agent_store_protocol::{
    StoreAgentDescendantStatus, StoreAgentSessionStatus, StoreAgentSubtreeSnapshot,
};
use rsi_navigation_api::{ArchiveActivity, ArchiveItem};

fn running(status: &StoreAgentSessionStatus) -> bool {
    status.has_open_turn || status.has_active_activation || status.has_waking_message
}

fn active_subagents(
    tree: &StoreAgentSubtreeSnapshot,
) -> impl Iterator<Item = &StoreAgentDescendantStatus> {
    tree.descendants
        .iter()
        .filter(|child| running(&child.status) && tree.activation_owns(child))
}

impl Navigation {
    pub(super) fn can_archive(&self, origin: &CallOrigin) -> Result<bool> {
        if matches!(origin, CallOrigin::Local) {
            return Ok(true);
        }
        let Some((context, slot)) = &self.archive_host else {
            return Ok(false);
        };
        let Some(devices) =
            context.lookup_local::<rsi_api_protocol::DeviceAdministrationContract>()
        else {
            return Ok(false);
        };
        devices.owns_managed_slot(slot, origin)
    }

    pub(super) fn require_archive_owner(&self, origin: &CallOrigin) -> Result<()> {
        if self.can_archive(origin)? {
            Ok(())
        } else {
            Err(ApiError::Unauthorized)
        }
    }

    fn service<C: rsi_meta::LocalContract>(&self) -> Option<Arc<C::Service>> {
        self.archive_host.as_ref()?.0.lookup_local::<C>()
    }

    fn background_scope(
        &self,
        header: &SessionHeader,
    ) -> Result<Option<(Arc<dyn rsi_jobs::Jobs>, rsi_jobs::JobScopeAuthority)>> {
        let Some(jobs) = self.service::<rsi_jobs::JobsContract>() else {
            return Ok(None);
        };
        let fingerprint = header.fingerprint().map_err(|_| ApiError::Unavailable)?;
        let id = rsi_agent_turn_protocol::session_job_scope(&fingerprint)
            .map_err(|_| ApiError::Unavailable)?;
        Ok(jobs.find_scope(&id).map(|scope| (jobs, scope)))
    }

    pub(super) async fn archive_activity(
        &self,
        header: &SessionHeader,
    ) -> Result<Vec<ArchiveActivity>> {
        let session = header.session_id();
        let tree = self
            .store
            .read_agent_subtree_snapshot(session)
            .await
            .map_err(|_| ApiError::Unavailable)?;
        let mut activity = Vec::new();
        let children: Vec<_> = active_subagents(&tree)
            .map(|child| ArchiveItem {
                id: child.status.session_id.to_string(),
                label: child.task_name.clone(),
            })
            .collect();
        if !children.is_empty() {
            activity.push(ArchiveActivity::Subagent { items: children });
        }
        let mut active = if tree.session.has_open_turn
            && let Some(turns) = self.service::<rsi_agent_turn_protocol::TurnJobsContract>()
        {
            turns
                .active_jobs(session)
                .map_err(|_| ApiError::Unavailable)?
        } else {
            Vec::new()
        };
        if let Some((jobs, scope)) = self.background_scope(header)? {
            active.extend(jobs.active(&scope).map_err(|_| ApiError::Unavailable)?);
        }
        if !active.is_empty() {
            activity.push(ArchiveActivity::Job {
                items: active
                    .into_iter()
                    .map(|job| ArchiveItem {
                        id: job.id,
                        label: job.label,
                    })
                    .collect(),
            });
        }
        if running(&tree.session) {
            activity.push(ArchiveActivity::Turn);
        }
        Ok(activity)
    }

    /// Runs under the navigation writer so restoration cannot race the stop.
    pub(super) async fn commit_edit(
        &self,
        origin: &CallOrigin,
        session: &SessionId,
        header: Option<&SessionHeader>,
        was_archived: bool,
        metadata: &super::SessionMetadata,
        stop_activity: bool,
    ) -> Result<std::result::Result<Arc<super::Document>, Vec<ArchiveActivity>>> {
        let archive_changed = was_archived != metadata.archived;
        let archiving = header.filter(|_| archive_changed && metadata.archived);
        if let Some(header) = archiving
            && !stop_activity
        {
            let activity = self.archive_activity(header).await?;
            if !activity.is_empty() {
                return Ok(Err(activity));
            }
        }
        let document = self.write_metadata(session, metadata).await?;
        if let Some(header) = archiving
            && stop_activity
        {
            if let Err(error) = self.stop_background_jobs(header) {
                eprintln!("Session {session} archived; stopping background jobs failed: {error}");
            }
            if let Err(error) = self.stop_session_work(origin.clone(), session).await {
                eprintln!("Session {session} archived; stopping Session activity failed: {error}");
            }
        }
        Ok(Ok(document))
    }

    async fn stop_session_work(&self, origin: CallOrigin, session: &SessionId) -> Result<()> {
        const STOP: rsi_agent_turn_protocol::CancelTarget =
            rsi_agent_turn_protocol::CancelTarget::Session { keep_inbox: false };
        let failure =
            || ApiError::Backend("Session archived, but some work could not be stopped".into());
        let tree = self
            .store
            .read_agent_subtree_snapshot(session)
            .await
            .map_err(|_| failure())?;
        let service = self
            .service::<rsi_session_protocol::SessionIngressContract>()
            .map(|ingress| ingress.scoped(origin));
        let mut result = Ok(());
        for id in running(&tree.session)
            .then_some(session)
            .into_iter()
            .chain(active_subagents(&tree).map(|child| &child.status.session_id))
        {
            let cancel = async {
                let handle = service
                    .as_ref()
                    .ok_or_else(failure)?
                    .attach(id)
                    .await
                    .map_err(|_| failure())?;
                handle.cancel(STOP, None).await.map_err(|_| failure())?;
                Ok(())
            }
            .await;
            result = result.and(cancel);
        }
        result
    }

    fn stop_background_jobs(&self, header: &SessionHeader) -> Result<()> {
        let failure = || ApiError::Unavailable;
        let mut result = Ok(());
        if let Some((jobs, scope)) = self.background_scope(header).map_err(|_| failure())? {
            for row in jobs.roster(&scope).map_err(|_| failure())? {
                if !row.status.is_terminal()
                    && let Some(origin) = row.origin
                {
                    result = result.and(
                        jobs.request_stop(&scope, &row.id, &origin)
                            .map_err(|_| failure()),
                    );
                }
            }
        }
        result
    }
}
