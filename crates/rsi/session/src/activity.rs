use super::{LocalSessionService, Result, SessionError, SessionId, StoreError, map_turn_error};
use rsi_agent_session_protocol::TurnId;
use rsi_session_protocol::{ActivityRequest, ActivityStatus, SessionActivity, SessionActivityPage};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
};

pub(super) struct Managed {
    ids: Mutex<VecDeque<SessionId>>,
    slots: tokio::sync::Semaphore,
    cached: Mutex<BTreeMap<SessionId, Cached>>,
}
struct Cached {
    revision: u64,
    sequence: u64,
    open: bool,
}
impl Default for Managed {
    fn default() -> Self {
        Self {
            ids: Mutex::default(),
            slots: tokio::sync::Semaphore::new(2),
            cached: Mutex::default(),
        }
    }
}
impl Managed {
    pub(super) fn remove(&self, id: &SessionId) {
        self.ids
            .lock()
            .expect("managed Session identities")
            .retain(|current| current != id);
        self.cached
            .lock()
            .expect("Session activity cache")
            .remove(id);
    }
    pub(super) fn opened(&self, id: &SessionId) {
        let mut ids = self.ids.lock().expect("managed Session identities");
        ids.retain(|current| current != id);
        ids.push_back(id.clone());
        if ids.len() > 64 {
            ids.pop_front();
        }
    }
}
impl LocalSessionService {
    pub(super) async fn collect_activity(&self) -> Result<SessionActivityPage> {
        self.check_origin()?;
        let _permit = self
            .activity
            .slots
            .try_acquire()
            .map_err(|_| SessionError::Capacity)?;
        tokio::select! {
            biased;
            () = self.projection_stopped.cancelled() => Err(SessionError::ShuttingDown),
            result = tokio::time::timeout(std::time::Duration::from_secs(30), self.activity_snapshot()) =>
                result.map_err(|_| SessionError::Invalid("activity read deadline elapsed".into()))?,
        }
    }
    async fn activity_cuts(
        &self,
        sessions: &[SessionId],
    ) -> BTreeMap<SessionId, std::result::Result<(u64, bool), StoreError>> {
        let revision = self.projection_service.activity_revision();
        let mut cuts = BTreeMap::new();
        let missing = {
            let cache = self.activity.cached.lock().expect("activity cache");
            sessions
                .iter()
                .filter_map(|session| {
                    if let Some(cached) = cache
                        .get(session)
                        .filter(|cached| Some(cached.revision) == revision)
                    {
                        cuts.insert(session.clone(), Ok((cached.sequence, cached.open)));
                        None
                    } else {
                        Some(session.clone())
                    }
                })
                .collect::<Vec<_>>()
        };
        for batch in missing.chunks(64) {
            let rows = match self.store.session_activity_cuts(batch).await {
                Ok(rows) => rows,
                Err(error) => vec![Err(error); batch.len()],
            };
            for (session, row) in batch.iter().zip(rows) {
                cuts.insert(
                    session.clone(),
                    row.map(|row| (row.durable_fact_seq, row.has_open_turn)),
                );
            }
        }
        if let Some(revision) = revision
            .filter(|revision| Some(*revision) == self.projection_service.activity_revision())
        {
            for session in missing {
                if let Some(Ok((sequence, open))) = cuts.get(&session) {
                    self.cache_activity(
                        &session,
                        Cached {
                            revision,
                            sequence: *sequence,
                            open: *open,
                        },
                    );
                }
            }
        }
        cuts
    }
    fn cache_activity(&self, session: &SessionId, cached: Cached) {
        let mut cache = self.activity.cached.lock().expect("activity cache");
        if cache.len() >= 128 && !cache.contains_key(session) {
            cache.pop_first();
        }
        cache.insert(session.clone(), cached);
    }
    async fn retain_visible_activity(
        &self,
        selected: &mut BTreeMap<SessionId, bool>,
        locations: &rsi_execution::ExecutionLocations,
    ) -> Result<Vec<tokio_util::sync::CancellationToken>> {
        let mut leases = Vec::new();
        let ids = selected.keys().cloned().collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(leases);
        }
        let rows = self
            .store
            .session_access(&ids)
            .await
            .map_err(super::map_store_error)?;
        for (id, row) in ids.iter().zip(rows) {
            let allowed = match row {
                Ok(row) => match self.protect_scope(row.protection.as_ref()) {
                    Ok(lease) => {
                        leases.push(lease);
                        locations.contains(&row.location)
                    }
                    Err(rsi_session_protocol::SessionError::Api(
                        rsi_api_protocol::ApiError::Unauthorized,
                    )) => false,
                    Err(error) => return Err(error),
                },
                Err(rsi_agent_store_protocol::StoreError::NotFound(_)) => false,
                Err(error) => return Err(super::map_store_error(error)),
            };
            if !allowed {
                selected.remove(id);
            }
        }
        Ok(leases)
    }
    async fn activity_snapshot(&self) -> Result<SessionActivityPage> {
        let residents = self
            .projection_service
            .resident_activity()
            .map_err(map_turn_error)?;
        let mut selected: BTreeMap<SessionId, bool> = self
            .activity
            .ids
            .lock()
            .expect("managed Session identities")
            .iter()
            .cloned()
            .map(|id| (id, false))
            .collect();
        for row in residents.entries {
            selected.insert(row.session, row.running);
        }
        let visibility = self.visibility()?;
        let leases = self
            .retain_visible_activity(&mut selected, visibility.locations())
            .await?;
        let ids: Vec<_> = selected.keys().cloned().collect();
        let reservation = self.interaction_retention.reserve_collection()?;
        let approvals = self.approvals.pending_for_sessions(&ids).await?;
        let questions = match &self.questions {
            Some(owner) => owner
                .pending_for_sessions(&ids.iter().map(ToString::to_string).collect::<Vec<_>>())
                .await
                .map_err(super::map_question_error)?,
            None => Vec::new(),
        };
        let interactions = reservation.retain(approvals, questions)?;
        let mut cuts = self.activity_cuts(&ids).await;
        let mut entries = Vec::new();
        for (session, running) in selected {
            let (fact_seq, status) = match cuts.remove(&session).expect("selected activity cut") {
                Ok((sequence, open)) => (
                    sequence.to_string(),
                    if !open {
                        ActivityStatus::Idle
                    } else if running {
                        ActivityStatus::Running
                    } else {
                        ActivityStatus::Unknown
                    },
                ),
                Err(StoreError::NotFound(_)) if !running => continue,
                Err(_) => ("0".into(), ActivityStatus::Unknown),
            };
            let requests = interactions
                .approvals()
                .iter()
                .filter(|request| request.subject.session_id() == session.as_str())
                .map(|request| {
                    Ok(ActivityRequest::Approval {
                        turn: TurnId::new(request.subject.turn_id())
                            .map_err(|_| SessionError::Invalid("invalid approval Turn".into()))?,
                        request: request.id.clone(),
                    })
                })
                .chain(
                    interactions
                        .questions()
                        .iter()
                        .filter(|request| request.session_id == session.as_str())
                        .map(|request| {
                            Ok(ActivityRequest::Question {
                                turn: TurnId::new(&request.turn_id).map_err(|_| {
                                    SessionError::Invalid("invalid question Turn".into())
                                })?,
                                request: request.id.clone(),
                            })
                        }),
                )
                .take(33)
                .collect::<Result<Vec<_>>>()?;
            let truncated = requests.len() > 32;
            entries.push(SessionActivity {
                session,
                fact_seq,
                status,
                requests: requests.into_iter().take(32).collect(),
                truncated,
            });
        }
        self.check_origin()?;
        if leases
            .iter()
            .any(tokio_util::sync::CancellationToken::is_cancelled)
        {
            return Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized));
        }
        Ok(SessionActivityPage {
            truncated: residents.has_more || entries.iter().any(|row| row.truncated),
            entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn managed_identities_are_bounded_and_opening_refreshes_recency_without_pinning() {
        let owner = Managed::default();
        for i in 0..64 {
            owner.opened(&SessionId::new(format!("s-{i}")).unwrap());
        }
        owner.opened(&SessionId::new("s-0").unwrap());
        owner.opened(&SessionId::new("new").unwrap());
        let ids = owner.ids.lock().unwrap();
        assert_eq!(ids.len(), 64);
        assert!(ids.contains(&SessionId::new("s-0").unwrap()));
        assert!(!ids.contains(&SessionId::new("s-1").unwrap()));
    }
}
