//! Bounded acquisition and reusable forward scans; no transcript or Fact retention.
use super::{Result, SessionError, map_store_error};
use rsi_agent_session_protocol::SessionId;
use rsi_agent_store_protocol::SessionStore;
use rsi_conversation::MetricsReducer;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[derive(Debug, Default)]
struct Scan {
    reducer: MetricsReducer,
    watermark: u64,
    tree: Option<rsi_session_protocol::TreeMetricsRead>,
}

#[derive(Debug, Eq, PartialEq)]
struct Key {
    session: SessionId,
    tree: bool,
}

pub(super) struct Progress {
    pub(super) watermark: u64,
    pub(super) complete: bool,
    pub(super) summary: rsi_conversation::SessionMetrics,
}

#[derive(Debug)]
pub(super) struct MetricsCache {
    entries: Mutex<VecDeque<(Key, Arc<tokio::sync::Mutex<Scan>>)>>,
    workers: tokio::sync::Semaphore,
}
impl Default for MetricsCache {
    fn default() -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            workers: tokio::sync::Semaphore::new(4),
        }
    }
}
impl MetricsCache {
    fn entry(&self, session: &SessionId) -> Result<Arc<tokio::sync::Mutex<Scan>>> {
        self.key_entry(Key {
            session: session.clone(),
            tree: false,
        })
    }
    fn key_entry(&self, key: Key) -> Result<Arc<tokio::sync::Mutex<Scan>>> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| SessionError::Backend("metrics cache lock poisoned".into()))?;
        if let Some(index) = entries.iter().position(|(id, _)| id == &key) {
            let entry = entries.remove(index).expect("located cache slot");
            let scan = entry.1.clone();
            entries.push_back(entry);
            return Ok(scan);
        }
        if entries.len() == 64 {
            let index = entries
                .iter()
                .position(|(_, scan)| Arc::strong_count(scan) == 1)
                .ok_or(SessionError::Capacity)?;
            entries.remove(index);
        }
        let scan = Arc::new(tokio::sync::Mutex::new(Scan::default()));
        entries.push_back((key, scan.clone()));
        Ok(scan)
    }

    pub(super) async fn read(
        &self,
        store: &dyn SessionStore,
        session: &SessionId,
    ) -> Result<Progress> {
        let entry = self.entry(session)?;
        let mut scan = entry.lock().await;
        let _permit = self
            .workers
            .try_acquire()
            .map_err(|_| SessionError::Capacity)?;
        if scan.reducer.summary().through_seq == scan.watermark {
            scan.watermark = store
                .inspect_session(session)
                .await
                .map_err(map_store_error)?
                .durable_fact_seq;
        }
        let mut bytes = 0usize;
        for _ in 0..8 {
            if scan.reducer.summary().through_seq == scan.watermark {
                break;
            }
            let page = store
                .read_facts(session, scan.reducer.summary().through_seq, 128)
                .await
                .map_err(map_store_error)?;
            if page.facts.is_empty() {
                return Err(SessionError::Backend(
                    "metrics fixed cut contains a missing Fact".into(),
                ));
            }
            for fact in &page.facts {
                if fact.seq() > scan.watermark {
                    break;
                }
                if bytes > 0 && bytes.saturating_add(fact.encoded_len()) > 16 * 1024 * 1024 {
                    break;
                }
                scan.reducer
                    .observe(fact)
                    .map_err(|error| SessionError::Backend(error.into()))?;
                bytes = bytes.saturating_add(fact.encoded_len());
            }
            if bytes >= 16 * 1024 * 1024
                || page.facts.iter().any(|fact| {
                    fact.seq() <= scan.watermark && fact.seq() > scan.reducer.summary().through_seq
                })
            {
                break;
            }
        }
        check_charge(&mut scan)?;
        Ok(Progress {
            watermark: scan.watermark,
            complete: scan.reducer.summary().through_seq == scan.watermark,
            summary: scan.reducer.summary().clone(),
        })
    }

    #[allow(clippy::too_many_lines)] // One bounded scan owns admission, member cuts and partial publication.
    pub(super) async fn tree(
        &self,
        store: &dyn SessionStore,
        session: &SessionId,
        refresh: bool,
    ) -> Result<rsi_session_protocol::TreeMetricsRead> {
        use rsi_session_protocol::{TreeMetricsMember, TreeMetricsRead};
        let entry = self.key_entry(Key {
            session: session.clone(),
            tree: true,
        })?;
        let mut scan = entry.lock().await;
        let _permit = self
            .workers
            .try_acquire()
            .map_err(|_| SessionError::Capacity)?;
        if scan
            .tree
            .as_ref()
            .is_none_or(|tree| refresh && tree.complete)
        {
            let inspection = store
                .inspect_session(session)
                .await
                .map_err(map_store_error)?;
            let member = |session_id, watermark| TreeMetricsMember {
                session_id,
                watermark,
                through_seq: 0,
                complete: watermark == Some(0),
            };
            let mut members = Vec::with_capacity((inspection.tree.descendants.len() + 1).min(256));
            members.push(member(session.clone(), Some(inspection.durable_fact_seq)));
            members.extend(
                inspection
                    .tree
                    .descendants
                    .iter()
                    .take(255)
                    .map(|child| member(child.status.session_id.clone(), None)),
            );
            scan.tree = Some(TreeMetricsRead {
                session_id: session.clone(),
                membership_control_seq: inspection.durable_control_seq,
                membership_complete: inspection.tree.descendants.len() < 256,
                complete: members.iter().all(|member| member.complete),
                members,
                totals: rsi_conversation::UsageTotals::default(),
            });
            scan.reducer = MetricsReducer::default();
        }
        let mut bytes = 0usize;
        for _ in 0..8 {
            let tree = scan.tree.as_ref().expect("initialized tree scan");
            let Some(index) = tree.members.iter().position(|member| !member.complete) else {
                break;
            };
            let member = &tree.members[index];
            let id = member.session_id.clone();
            let Some(watermark) = member.watermark else {
                let watermark = store
                    .inspect_session(&id)
                    .await
                    .map_err(map_store_error)?
                    .durable_fact_seq;
                let member = &mut scan.tree.as_mut().expect("tree scan").members[index];
                member.watermark = Some(watermark);
                member.complete = watermark == 0;
                continue;
            };
            let page = store
                .read_facts(&id, member.through_seq, 128)
                .await
                .map_err(map_store_error)?;
            if page.facts.is_empty() {
                return Err(SessionError::Backend(
                    "tree metrics cut contains a missing Fact".into(),
                ));
            }
            let mut exhausted = false;
            for fact in &page.facts {
                if fact.seq() > watermark {
                    break;
                }
                if bytes > 0 && bytes.saturating_add(fact.encoded_len()) > 16 * 1024 * 1024 {
                    exhausted = true;
                    break;
                }
                scan.reducer
                    .observe(fact)
                    .map_err(|error| SessionError::Backend(error.into()))?;
                bytes = bytes.saturating_add(fact.encoded_len());
            }
            let through = scan.reducer.summary().through_seq;
            let summary = scan.reducer.summary().clone();
            let tree = scan.tree.as_mut().expect("tree scan");
            tree.members[index].through_seq = through;
            if through == watermark {
                tree.totals
                    .add_session(&summary)
                    .map_err(|error| SessionError::Backend(error.into()))?;
                tree.members[index].complete = true;
                scan.reducer = MetricsReducer::default();
            }
            if exhausted || bytes >= 16 * 1024 * 1024 {
                break;
            }
        }
        let tree = scan.tree.as_mut().expect("tree scan");
        tree.complete = tree.members.iter().all(|member| member.complete);
        let mut response = tree.clone();
        response
            .totals
            .add_session(scan.reducer.summary())
            .map_err(|error| SessionError::Backend(error.into()))?;
        check_charge(&mut scan)?;
        response.validate()?;
        Ok(response)
    }
}

fn check_charge(scan: &mut Scan) -> Result<()> {
    let tree = scan.tree.as_ref().map_or(0, |tree| {
        std::mem::size_of_val(tree)
            + tree.members.capacity()
                * std::mem::size_of::<rsi_session_protocol::TreeMetricsMember>()
            + serde_json::to_vec(tree)
                .map_or(usize::MAX / 4, |bytes| bytes.len())
                .saturating_mul(2)
    });
    if std::mem::size_of::<Scan>()
        .saturating_add(scan.reducer.retained_bytes())
        .saturating_add(tree)
        > 256 * 1024
    {
        *scan = Scan::default();
        return Err(SessionError::Capacity);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{
        AgentPresetId, EffectId, FrozenAgentSettings, ModelEventPurpose, ModelPurpose, SessionFact,
        SessionFactBody, SessionHeader, TurnId,
    };
    use rsi_agent_store_protocol::AppendBatch;

    fn header() -> SessionHeader {
        SessionHeader::new(
            SessionId::new("metrics").unwrap(),
            1,
            "/workspace",
            AgentPresetId::new("standard").unwrap(),
            FrozenAgentSettings::new(
                "default",
                "system",
                rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
                rsi_sandbox::SandboxMode::WorkspaceWrite,
                false,
            )
            .unwrap(),
        )
        .unwrap()
    }
    async fn append(
        store: &rsi_agent_testkit::MemoryStore,
        header: &SessionHeader,
        from: u64,
        through: u64,
    ) {
        let mut start = from;
        while start < through {
            let end = (start + 512).min(through);
            let facts = (start + 1..=end)
                .map(|seq| {
                    let turn_id = TurnId::new("turn").unwrap();
                    let effect_id = EffectId::new("model").unwrap();
                    let body = match seq {
                        1 => SessionFactBody::TurnAccepted {
                            turn_id,
                            text: "input".into(),
                            model: None,
                            reasoning_effort: None,
                            sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                            require_approval: false,
                        },
                        2 => SessionFactBody::ModelIntent {
                            evidence: rsi_agent_session_protocol::RequestEvidence::Unavailable {
                                reason:
                                    rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
                            },
                            price_quote: None,
                            turn_id,
                            effect_id,
                            purpose: ModelPurpose::Conversation,
                            snapshot: rsi_ai_protocol::PreparedCallSnapshot {
                                call_id: "call".into(),
                                deployment_id: "fixture".into(),
                                provider_family: "fixture".into(),
                                capability: rsi_ai_protocol::AiCapability::Language,
                                model: "model".into(),
                                protocol: "fixture".into(),
                                transport: "local".into(),
                                endpoint_fingerprint: "fixture".into(),
                                config_generation: 1,
                                credential_source: None,
                                retry_policy: rsi_ai_protocol::RetryPolicy::default(),
                                request_sha256: "0".repeat(64),
                                language_settings: None,
                            },
                        },
                        3 => SessionFactBody::ModelStarted { turn_id, effect_id },
                        _ => SessionFactBody::ModelEvent {
                            turn_id,
                            effect_id,
                            purpose: ModelEventPurpose::Conversation,
                            event: if seq == 4 {
                                rsi_ai_protocol::LanguageEvent::ContentStarted {
                                    index: 0,
                                    content: rsi_ai_protocol::ContentStart::Text,
                                }
                            } else {
                                rsi_ai_protocol::LanguageEvent::ContentDelta {
                                    index: 0,
                                    delta: rsi_ai_protocol::ContentDelta::Text("x".into()),
                                }
                            },
                        },
                    };
                    Arc::new(SessionFact::new(seq, seq, body).unwrap())
                })
                .collect();
            store
                .append(AppendBatch {
                    session_id: header.session_id().clone(),
                    expected_seq: start,
                    header: (start == 0).then(|| header.clone()),
                    facts,
                })
                .await
                .unwrap();
            start = end;
        }
    }

    #[tokio::test]
    async fn bounded_forward_slices_keep_their_cut_while_the_store_advances() {
        let store = rsi_agent_testkit::MemoryStore::new();
        let header = header();
        append(&store, &header, 0, 1026).await;
        let cache = MetricsCache::default();
        let first = cache.read(&store, header.session_id()).await.unwrap();
        assert!(!first.complete);
        assert_eq!(first.watermark, 1026);
        assert_eq!(first.summary.through_seq, 1024);
        assert_eq!(
            store.take_fact_read_cursors(),
            vec![0, 128, 256, 384, 512, 640, 768, 896]
        );
        append(&store, &header, 1026, 1028).await;
        let second = cache.read(&store, header.session_id()).await.unwrap();
        assert!(second.complete);
        assert_eq!(second.watermark, 1026);
        assert_eq!(second.summary.through_seq, 1026);
        assert_eq!(store.take_fact_read_cursors(), vec![1024]);
        let third = cache.read(&store, header.session_id()).await.unwrap();
        assert!(third.complete);
        assert_eq!(third.watermark, 1028);
        assert_eq!(store.take_fact_read_cursors(), vec![1026]);
        let same = cache.read(&store, header.session_id()).await.unwrap();
        assert_eq!(same.summary, third.summary);
        assert!(store.take_fact_read_cursors().is_empty());
    }

    #[tokio::test]
    async fn cache_eviction_preserves_in_use_slots_and_worker_admission_is_bounded() {
        let cache = MetricsCache::default();
        let active = cache.entry(&SessionId::new("active").unwrap()).unwrap();
        for index in 0..100 {
            cache
                .entry(&SessionId::new(format!("idle-{index}")).unwrap())
                .unwrap();
        }
        {
            let entries = cache.entries.lock().unwrap();
            assert_eq!(entries.len(), 64);
            assert!(
                entries
                    .iter()
                    .any(|(id, _)| id.session.as_str() == "active")
            );
            assert!(
                !entries
                    .iter()
                    .any(|(id, _)| id.session.as_str() == "idle-0")
            );
        }
        drop(active);
        let permits = cache.workers.acquire_many(4).await.unwrap();
        assert!(matches!(
            cache
                .read(
                    &rsi_agent_testkit::MemoryStore::new(),
                    &SessionId::new("blocked").unwrap()
                )
                .await,
            Err(SessionError::Capacity)
        ));
        drop(permits);
        assert_eq!(cache.workers.available_permits(), 4);
    }

    #[test]
    fn oversized_cached_vector_capacity_is_released_on_admission_failure() {
        let mut scan = Scan {
            tree: Some(rsi_session_protocol::TreeMetricsRead {
                session_id: SessionId::new("root").unwrap(),
                membership_control_seq: 0,
                membership_complete: true,
                complete: false,
                members: Vec::with_capacity(8192),
                totals: rsi_conversation::UsageTotals::default(),
            }),
            ..Scan::default()
        };
        assert!(matches!(
            check_charge(&mut scan),
            Err(SessionError::Capacity)
        ));
        assert!(scan.tree.is_none());
        check_charge(&mut scan).unwrap();
    }

    #[tokio::test]
    async fn large_tree_uses_one_cache_slot_and_explicit_fixed_member_cuts() {
        use rsi_agent_session_protocol::{
            AgentPath, ForkOrigin, ForkTurnSelection, ModelSelection,
        };
        let store = rsi_agent_testkit::MemoryStore::new();
        let root = header();
        append(&store, &root, 0, 1026).await;
        let mut child_headers = Vec::new();
        for index in 1..=255 {
            let child = root
                .forked_child(
                    SessionId::new(format!("child-{index:03}")).unwrap(),
                    2,
                    ForkOrigin {
                        parent_session_id: root.session_id().clone(),
                        root_session_id: root.session_id().clone(),
                        path: AgentPath::new(vec![index]).unwrap(),
                        task_name: format!("task-{index}"),
                        parent_header_fingerprint: root.fingerprint().unwrap(),
                        invoking_turn_id: TurnId::new("turn").unwrap(),
                        resolved_after_seq: 0,
                        resolved_terminal_seq: 0,
                        terminal_prefix_sha256: "0".repeat(64),
                        resolved_terminal_control_seq: 0,
                        terminal_control_prefix_sha256: "0".repeat(64),
                        requested_turns: ForkTurnSelection::None,
                        effective_turns: 0,
                    },
                    ModelSelection::baseline(root.settings()),
                )
                .unwrap();
            append(&store, &child, 0, 4).await;
            child_headers.push(child);
        }
        let cache = MetricsCache::default();
        let first = cache.tree(&store, root.session_id(), false).await.unwrap();
        assert_eq!(first.members.len(), 256);
        assert!(first.membership_complete);
        assert!(!first.complete);
        assert_eq!(first.members[0].through_seq, 1024);
        assert_eq!(first.members[0].watermark, Some(1026));
        assert!(first.members[1].watermark.is_none());
        append(&store, &root, 1026, 1028).await;
        append(&store, &child_headers[0], 4, 6).await;
        // Refresh cannot discard an incomplete cycle or move its root horizon.
        let mut done = cache.tree(&store, root.session_id(), true).await.unwrap();
        assert!(!done.complete);
        assert_eq!(done.members[0].watermark, Some(1026));
        for _ in 0..100 {
            if done.complete {
                break;
            }
            done = cache.tree(&store, root.session_id(), false).await.unwrap();
            done.validate().unwrap();
        }
        assert!(done.complete);
        assert!(done.membership_complete);
        assert_eq!(done.members[0].watermark, Some(1026));
        assert_eq!(done.members[1].watermark, Some(6));
        assert_eq!(done.totals.attempts, 256);
        assert_eq!(done.totals.configured_cost.missing_price, 256);
        assert_eq!(cache.entries.lock().unwrap().len(), 1);
        let own = cache.read(&store, root.session_id()).await.unwrap();
        assert_eq!(own.summary.attempts, 1);
        assert_eq!(cache.entries.lock().unwrap().len(), 2);
        store.take_fact_read_cursors();
        assert!(
            cache
                .tree(&store, root.session_id(), false)
                .await
                .unwrap()
                .complete
        );
        assert!(
            store.take_fact_read_cursors().is_empty(),
            "completed detail must be a cache read until explicit refresh"
        );
        let fresh = cache.tree(&store, root.session_id(), true).await.unwrap();
        assert_eq!(fresh.members[0].watermark, Some(1028));
        assert!(!fresh.complete);
    }

    #[tokio::test]
    async fn callers_waiting_for_one_scan_do_not_consume_worker_capacity() {
        let cache = Arc::new(MetricsCache::default());
        let entry = cache.entry(&SessionId::new("coalesced").unwrap()).unwrap();
        let held = entry.lock().await;
        let store = rsi_agent_testkit::MemoryStore::new();
        let id = SessionId::new("coalesced").unwrap();
        let mut futures = (0..8)
            .map(|_| Box::pin(cache.read(&store, &id)))
            .collect::<Vec<_>>();
        for future in &mut futures {
            assert!(futures_util::poll!(future.as_mut()).is_pending());
        }
        assert_eq!(cache.workers.available_permits(), 4);
        drop(futures);
        drop(held);
        assert_eq!(cache.workers.available_permits(), 4);
    }
}
