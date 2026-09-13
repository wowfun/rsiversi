use super::{
    AgentCommitWatermark, AgentControlRecord, AgentControlRecordBody, AppendBatch, AppendCommit,
    Arc, AtomicAgentCommit, AtomicAgentCommitResult, AtomicSessionAppend, BTreeMap, BTreeSet,
    CasObjectRef, Digest, EMPTY_CONTROL_PREFIX_DIGEST, EMPTY_FACT_PREFIX_DIGEST, ForkTurnSelection,
    MAXIMUM_STORE_CAS_BYTES, MAXIMUM_STORE_CONTROL_PAGE_BYTES, MAXIMUM_STORE_FACT_PAGE_BYTES,
    MAXIMUM_STORE_MAILBOX_PAGE_BYTES, MemorySession, MemoryState, MemoryStore, MemoryTurnBoundary,
    MessageId, MessageTarget, Ordering, Result, SessionFact, SessionHeader, SessionId,
    SessionStore, Sha256, StoreActivationPhase, StoreActiveActivation, StoreAgentChild,
    StoreAgentChildPage, StoreAgentDescendantStatus, StoreAgentMailbox, StoreAgentMailboxSummary,
    StoreAgentMessage, StoreAgentMessageState, StoreAgentSessionStatus, StoreAgentSubtreeSnapshot,
    StoreBackwardFactPage, StoreControlPage, StoreError, StoreFactPage, StoreFactTurnRole,
    StoreForkBoundary, StoreOpenTurn, StoreOpenTurnPage, StoreReadyMessage,
    StoreReadyMessageCursor, StoreReadyMessagePage, StoreReadyRootPage, StoreRecentSession,
    StoreRecentSessionCursor, StoreRecentSessionPage, StoreSessionPage, StoreTurnBoundary,
    StoreTurnFactPage, StoreWaitingActivationPage, StoredContextCheckpoint, TurnId,
    WriteContextCheckpoint, advance_control_prefix_digest, advance_fact_prefix_digest, async_trait,
    validate_message_claim_fact, validate_read_limit, validate_session_read_limit,
};

#[async_trait]
impl SessionStore for MemoryStore {
    async fn append(&self, batch: AppendBatch) -> Result<AppendCommit> {
        batch.validate()?;
        if self.should_fail_append() {
            return Err(StoreError::Io("injected append failure".into()));
        }
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(session) = state.sessions.get_mut(&batch.session_id) {
            let actual = session.facts.last().map_or(0, |fact| fact.seq());
            if actual != batch.expected_seq {
                return Err(StoreError::Conflict {
                    expected: batch.expected_seq,
                    actual,
                });
            }
            if batch.header.is_some() {
                return Err(StoreError::Invalid(
                    "existing session cannot replace its immutable header".into(),
                ));
            }
            let (turn_updates, fact_prefix_digest) =
                index_appended_turns(&session.turns, &batch.facts, session.fact_prefix_digest)?;
            session.facts.extend(batch.facts);
            session.turns.extend(turn_updates);
            session.fact_prefix_digest = fact_prefix_digest;
            Ok(AppendCommit {
                durable_seq: session
                    .facts
                    .last()
                    .expect("a validated append is nonempty")
                    .seq(),
            })
        } else {
            if batch.header.is_none() {
                return Err(StoreError::NotFound(batch.session_id.to_string()));
            }
            if batch.expected_seq != 0 {
                return Err(StoreError::Conflict {
                    expected: batch.expected_seq,
                    actual: 0,
                });
            }
            let header = batch
                .header
                .ok_or_else(|| StoreError::NotFound(batch.session_id.as_str().to_owned()))?;
            validate_memory_agent_node(&state, &header)?;
            let durable_seq = batch
                .facts
                .last()
                .expect("a validated append is nonempty")
                .seq();
            let (turns, fact_prefix_digest) =
                index_appended_turns(&BTreeMap::new(), &batch.facts, EMPTY_FACT_PREFIX_DIGEST)?;
            state
                .recent_sessions
                .insert((header.created_at_ms(), batch.session_id.clone()));
            if let Some(origin) = header.fork_origin() {
                state.agent_children.insert(
                    (origin.parent_session_id.clone(), batch.session_id.clone()),
                    StoreAgentChild {
                        session_id: batch.session_id.clone(),
                        path: origin.path.clone(),
                        task_name: origin.task_name.clone(),
                    },
                );
            }
            state.sessions.insert(
                batch.session_id,
                MemorySession {
                    header,
                    facts: batch.facts,
                    turns,
                    fact_prefix_digest,
                    checkpoint: None,
                    controls: Vec::new(),
                    last_settled_control_seq: 0,
                    control_prefix_digest: EMPTY_CONTROL_PREFIX_DIGEST,
                    domain_versions: BTreeMap::new(),
                    domain_requests: BTreeMap::new(),
                    domain_usage: BTreeMap::new(),
                },
            );
            Ok(AppendCommit { durable_seq })
        }
    }

    async fn commit_agent(&self, commit: AtomicAgentCommit) -> Result<AtomicAgentCommitResult> {
        commit.validate()?;
        if self.should_fail_append() {
            return Err(StoreError::Io("injected append failure".into()));
        }
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_memory_activation_guards(&state, &commit)?;
        let mut candidate = state.clone();
        let mut watermarks = Vec::with_capacity(commit.sessions.len());
        for append in commit.sessions {
            watermarks.push(apply_atomic_memory_append(&mut candidate, append)?);
        }
        validate_memory_quiescence_guard(&candidate, commit.quiescent_descendants_of.as_ref())?;
        *state = candidate;
        Ok(AtomicAgentCommitResult {
            sessions: watermarks,
        })
    }

    async fn validate_session(&self, session_id: &SessionId) -> Result<()> {
        // Every mutation validates typed state before installing it under this lock.
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.sessions.contains_key(session_id) {
            Ok(())
        } else {
            Err(StoreError::NotFound(session_id.to_string()))
        }
    }

    async fn header(&self, session_id: &SessionId) -> Result<SessionHeader> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .get(session_id)
            .map(|session| session.header.clone())
            .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))
    }

    async fn read_domain_states(
        &self,
        session_id: &SessionId,
        at_control_seq: Option<u64>,
    ) -> Result<rsi_agent_store_protocol::StoreDomainStatePage> {
        use rsi_agent_store_protocol::{StoreDomainState, StoreDomainStatePage};
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        let durable_control_seq = session.controls.last().map_or(0, AgentControlRecord::seq);
        let selected_control_seq = at_control_seq.unwrap_or(durable_control_seq);
        if selected_control_seq > durable_control_seq {
            return Err(StoreError::Invalid(
                "domain horizon exceeds current control tail".into(),
            ));
        }
        let mut states = Vec::new();
        for versions in session.domain_versions.values() {
            let Some(index) = versions
                .partition_point(|head| head.control_seq <= selected_control_seq)
                .checked_sub(1)
            else {
                continue;
            };
            let head = &versions[index];
            let record = domain_control(session, head.control_seq)?;
            let AgentControlRecordBody::DomainStateCommitted { commit } = record.body() else {
                return Err(StoreError::Corrupt(
                    "domain index points to another control kind".into(),
                ));
            };
            let update = commit.updates().get(head.update_index).ok_or_else(|| {
                StoreError::Corrupt("domain update offset exceeds canonical request".into())
            })?;
            if update.revision() != head.revision {
                return Err(StoreError::Corrupt(
                    "domain indexed revision differs from canonical update".into(),
                ));
            }
            states.push(StoreDomainState {
                head: head.clone(),
                snapshot: update.snapshot().clone(),
            });
        }
        let page = StoreDomainStatePage {
            durable_fact_seq: session.facts.last().map_or(0, |fact| fact.seq()),
            durable_control_seq,
            selected_control_seq,
            states,
        };
        page.validate()?;
        Ok(page)
    }

    async fn read_domain_request(
        &self,
        session_id: &SessionId,
        request_id: &rsi_agent_session_protocol::DomainRequestId,
    ) -> Result<Option<AgentControlRecord>> {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        let Some(seq) = session.domain_requests.get(request_id) else {
            return Ok(None);
        };
        let record = domain_control(session, *seq)?;
        if !matches!(record.body(), AgentControlRecordBody::DomainStateCommitted { commit } if commit.request_id() == Some(request_id))
        {
            return Err(StoreError::Corrupt(
                "domain request index differs from canonical request".into(),
            ));
        }
        Ok(Some(record.clone()))
    }

    async fn read_turn_domain_usage(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> Result<rsi_agent_store_protocol::StoreTurnDomainUsage> {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        let (records, bytes) = session
            .domain_usage
            .get(turn_id)
            .copied()
            .unwrap_or_default();
        Ok(rsi_agent_store_protocol::StoreTurnDomainUsage {
            durable_fact_seq: session.facts.last().map_or(0, |fact| fact.seq()),
            durable_control_seq: session.controls.last().map_or(0, AgentControlRecord::seq),
            records,
            bytes,
        })
    }

    async fn read_facts(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreFactPage> {
        validate_read_limit(limit)?;
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.fact_read_cursors.push(after_seq);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))?;
        let durable_seq = session.facts.last().map_or(0, |fact| fact.seq());
        if after_seq > durable_seq {
            return Err(StoreError::Invalid(
                "Fact cursor exceeds the durable tail".into(),
            ));
        }
        let start = usize::try_from(after_seq)
            .map_err(|_| StoreError::Invalid("Fact cursor does not fit memory".into()))?;
        let mut facts = Vec::new();
        let mut encoded_bytes = 0_usize;
        for fact in session.facts.iter().skip(start).take(limit) {
            let projected = encoded_bytes
                .checked_add(fact.encoded_len())
                .ok_or_else(|| StoreError::Corrupt("Fact page size overflow".into()))?;
            if !facts.is_empty() && projected > MAXIMUM_STORE_FACT_PAGE_BYTES {
                break;
            }
            encoded_bytes = projected;
            facts.push(fact.as_ref().clone());
        }
        let page = StoreFactPage {
            after_seq,
            facts,
            durable_seq,
        };
        page.validate()?;
        Ok(page)
    }

    async fn read_controls(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreControlPage> {
        validate_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        let durable_seq = session.controls.last().map_or(0, AgentControlRecord::seq);
        if after_seq > durable_seq {
            return Err(StoreError::Invalid(
                "control cursor exceeds the durable tail".into(),
            ));
        }
        let start = usize::try_from(after_seq)
            .map_err(|_| StoreError::Invalid("control cursor does not fit memory".into()))?;
        let mut records = Vec::new();
        let mut encoded_bytes = 0_usize;
        for record in session.controls.iter().skip(start).take(limit) {
            let projected = encoded_bytes
                .checked_add(record.encoded_len())
                .ok_or_else(|| StoreError::Corrupt("control page size overflow".into()))?;
            if !records.is_empty() && projected > MAXIMUM_STORE_CONTROL_PAGE_BYTES {
                break;
            }
            encoded_bytes = projected;
            records.push(record.clone());
        }
        let page = StoreControlPage {
            after_seq,
            records,
            durable_seq,
        };
        page.validate()?;
        Ok(page)
    }

    async fn read_facts_before(
        &self,
        session_id: &SessionId,
        exclusive_before_seq: u64,
        limit: usize,
    ) -> Result<StoreBackwardFactPage> {
        validate_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))?;
        let durable_seq = session.facts.last().map_or(0, |fact| fact.seq());
        let maximum_before = durable_seq
            .checked_add(1)
            .ok_or_else(|| StoreError::Corrupt("durable sequence is exhausted".into()))?;
        let before_seq = if exclusive_before_seq == 0 {
            maximum_before
        } else {
            exclusive_before_seq
        };
        if before_seq > maximum_before {
            return Err(StoreError::Invalid(
                "backward Fact cursor exceeds one past the durable tail".into(),
            ));
        }
        let take = usize::try_from(before_seq - 1)
            .map_err(|_| StoreError::Invalid("Fact cursor does not fit memory".into()))?;
        let mut facts = Vec::new();
        let mut encoded_bytes = 0_usize;
        for fact in session.facts.iter().take(take).rev() {
            let projected = encoded_bytes
                .checked_add(fact.encoded_len())
                .ok_or_else(|| StoreError::Corrupt("backward Fact page size overflow".into()))?;
            if facts.len() == limit
                || (!facts.is_empty() && projected > MAXIMUM_STORE_FACT_PAGE_BYTES)
            {
                break;
            }
            encoded_bytes = projected;
            facts.push(fact.as_ref().clone());
        }
        facts.reverse();
        let has_more = facts.first().is_some_and(|fact| fact.seq() > 1);
        let page = StoreBackwardFactPage {
            before_seq,
            facts,
            durable_seq,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn read_turn_facts(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreTurnFactPage> {
        validate_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))?;
        let durable_seq = session.facts.last().map_or(0, |fact| fact.seq());
        if after_seq > durable_seq {
            return Err(StoreError::Invalid(
                "turn Fact cursor exceeds the durable tail".into(),
            ));
        }
        if !session.turns.contains_key(turn_id) {
            return Err(StoreError::TurnNotFound {
                session: session_id.to_string(),
                turn: turn_id.to_string(),
            });
        }
        let mut facts = Vec::new();
        let mut encoded_bytes = 0_usize;
        let mut has_more = false;
        for fact in session
            .facts
            .iter()
            .filter(|fact| fact.seq() > after_seq && fact.body().turn_id() == turn_id)
        {
            let projected = encoded_bytes
                .checked_add(fact.encoded_len())
                .ok_or_else(|| StoreError::Corrupt("turn Fact page size overflow".into()))?;
            if facts.len() == limit
                || (!facts.is_empty()
                    && projected > rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES)
            {
                has_more = true;
                break;
            }
            encoded_bytes = projected;
            facts.push(fact.as_ref().clone());
        }
        let page = StoreTurnFactPage {
            turn_id: turn_id.clone(),
            after_seq,
            facts,
            durable_seq,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn read_turn_boundary(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> Result<StoreTurnBoundary> {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))?;
        let boundary = session
            .turns
            .get(turn_id)
            .ok_or_else(|| StoreError::TurnNotFound {
                session: session_id.to_string(),
                turn: turn_id.to_string(),
            })?;
        let accepted = session
            .facts
            .get(usize::try_from(boundary.accepted_seq - 1).expect("bounded sequence"))
            .expect("turn index acceptance points into Facts");
        let terminal = boundary.terminal_seq.map(|seq| {
            session
                .facts
                .get(usize::try_from(seq - 1).expect("bounded sequence"))
                .expect("turn index terminal points into Facts")
                .as_ref()
                .clone()
        });
        StoreTurnBoundary::new(
            turn_id.clone(),
            accepted.as_ref().clone(),
            terminal,
            session.facts.last().map_or(0, |fact| fact.seq()),
        )
    }

    #[allow(clippy::too_many_lines)] // One selection validates balanced membership and both terminal prefixes.
    async fn resolve_fork_boundary(
        &self,
        session_id: &SessionId,
        invoking_turn_id: &TurnId,
        selection: ForkTurnSelection,
    ) -> Result<StoreForkBoundary> {
        selection
            .validate()
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        self.fork_boundary_resolutions
            .fetch_add(1, Ordering::AcqRel);
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        let invoking =
            session
                .turns
                .get(invoking_turn_id)
                .ok_or_else(|| StoreError::TurnNotFound {
                    session: session_id.to_string(),
                    turn: invoking_turn_id.to_string(),
                })?;
        let mut completed = session
            .turns
            .iter()
            .filter_map(|(turn_id, turn)| {
                turn.terminal_seq
                    .filter(|terminal| *terminal < invoking.accepted_seq)
                    .map(|terminal| (terminal, turn.accepted_seq, turn_id))
            })
            .collect::<Vec<_>>();
        completed.sort_unstable();
        let available = u64::try_from(completed.len())
            .map_err(|_| StoreError::Corrupt("completed turn count exceeds u64".into()))?;
        let effective = match selection {
            ForkTurnSelection::None => 0,
            ForkTurnSelection::All => available,
            ForkTurnSelection::Last(count) => available.min(count),
        };
        let selected_count = usize::try_from(effective).map_err(|_| {
            StoreError::Corrupt("effective fork turn count does not fit memory".into())
        })?;
        let selected = &completed[completed.len().saturating_sub(selected_count)..];
        let (resolved_after_seq, resolved_terminal_seq) = selected
            .first()
            .zip(selected.last())
            .map_or((0, 0), |((_, accepted, _), (terminal, _, _))| {
                (accepted.saturating_sub(1), *terminal)
            });
        if effective > 0 {
            let selected_ids = selected
                .iter()
                .map(|(_, _, turn_id)| *turn_id)
                .collect::<BTreeSet<_>>();
            let interval_turns = session
                .turns
                .iter()
                .filter(|(_, turn)| {
                    turn.accepted_seq > resolved_after_seq
                        && turn.accepted_seq <= resolved_terminal_seq
                })
                .map(|(turn_id, _)| turn_id)
                .collect::<BTreeSet<_>>();
            let interval_fact_turns = session
                .facts
                .iter()
                .filter(|fact| {
                    fact.seq() > resolved_after_seq && fact.seq() <= resolved_terminal_seq
                })
                .map(|fact| fact.body().turn_id())
                .collect::<BTreeSet<_>>();
            if interval_turns != selected_ids || interval_fact_turns != selected_ids {
                return Err(StoreError::Invalid(
                    "fork selection does not form a balanced contiguous completed-turn interval"
                        .into(),
                ));
            }
        }
        let terminal_prefix_sha256 = selected.last().map_or_else(
            || Ok(hex::encode(EMPTY_FACT_PREFIX_DIGEST)),
            |(_, _, turn_id)| {
                session
                    .turns
                    .get(*turn_id)
                    .and_then(|turn| turn.terminal_prefix_sha256.clone())
                    .ok_or_else(|| {
                        StoreError::Corrupt(
                            "completed in-memory Turn has no terminal-prefix digest".into(),
                        )
                    })
            },
        )?;
        let (resolved_terminal_control_seq, terminal_control_prefix_sha256) =
            selected.last().map_or_else(
                || Ok((0, hex::encode(EMPTY_CONTROL_PREFIX_DIGEST))),
                |(_, _, turn_id)| {
                    session
                        .turns
                        .get(*turn_id)
                        .and_then(|turn| turn.terminal_control.clone())
                        .ok_or_else(|| {
                            StoreError::Corrupt(
                                "completed Turn has no terminal control boundary".into(),
                            )
                        })
                },
            )?;
        Ok(StoreForkBoundary {
            resolved_terminal_control_seq,
            terminal_control_prefix_sha256,
            resolved_after_seq,
            resolved_terminal_seq,
            terminal_prefix_sha256,
            effective_turns: effective,
        })
    }

    async fn list_open_turns(
        &self,
        session_id: &SessionId,
        after_accepted_seq: u64,
        limit: usize,
    ) -> Result<StoreOpenTurnPage> {
        validate_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))?;
        let durable_seq = session.facts.last().map_or(0, |fact| fact.seq());
        if after_accepted_seq > durable_seq {
            return Err(StoreError::Invalid(
                "open-turn cursor exceeds the durable tail".into(),
            ));
        }
        let mut turns = session
            .turns
            .iter()
            .filter_map(|(turn_id, boundary)| {
                (boundary.terminal_seq.is_none() && boundary.accepted_seq > after_accepted_seq)
                    .then_some(StoreOpenTurn {
                        turn_id: turn_id.clone(),
                        accepted_seq: boundary.accepted_seq,
                    })
            })
            .collect::<Vec<_>>();
        turns.sort_by_key(|turn| turn.accepted_seq);
        let has_more = turns.len() > limit;
        turns.truncate(limit);
        let page = StoreOpenTurnPage {
            after_accepted_seq,
            turns,
            durable_seq,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn list_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreSessionPage> {
        validate_session_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut sessions = state
            .sessions
            .keys()
            .filter(|session| after.is_none_or(|after| *session > after))
            .take(limit + 1)
            .cloned()
            .collect::<Vec<_>>();
        let has_more = sessions.len() > limit;
        sessions.truncate(limit);
        let page = StoreSessionPage {
            after: after.cloned(),
            sessions,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn list_recent_sessions(
        &self,
        after: Option<&StoreRecentSessionCursor>,
        limit: usize,
    ) -> Result<StoreRecentSessionPage> {
        validate_session_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut sessions = state
            .recent_sessions
            .iter()
            .rev()
            .filter(|(created_at_ms, session_id)| {
                after.is_none_or(|after| {
                    (*created_at_ms, session_id) < (after.created_at_ms, &after.session_id)
                })
            })
            .take(limit + 1)
            .map(|(_, session_id)| StoreRecentSession {
                header: state
                    .sessions
                    .get(session_id)
                    .expect("recent index references its Session")
                    .header
                    .clone(),
            })
            .collect::<Vec<_>>();
        let has_more = sessions.len() > limit;
        sessions.truncate(limit);
        let page = StoreRecentSessionPage {
            after: after.cloned(),
            sessions,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn list_open_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreSessionPage> {
        validate_session_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut sessions = state
            .sessions
            .iter()
            .filter(|(session_id, session)| {
                after.is_none_or(|after| *session_id > after)
                    && session
                        .turns
                        .values()
                        .any(|boundary| boundary.terminal_seq.is_none())
            })
            .map(|(session_id, _)| session_id.clone())
            .take(limit + 1)
            .collect::<Vec<_>>();
        let has_more = sessions.len() > limit;
        sessions.truncate(limit);
        let page = StoreSessionPage {
            after: after.cloned(),
            sessions,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn list_ready_messages(
        &self,
        root_session_id: &SessionId,
        after: Option<&StoreReadyMessageCursor>,
        limit: usize,
    ) -> Result<StoreReadyMessagePage> {
        validate_session_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut all = state
            .ready_messages
            .iter()
            .filter(|((root, _, _, _), _)| root == root_session_id)
            .map(|(_, message)| message.clone())
            .filter(|message| {
                after.is_none_or(|after| {
                    (
                        message.timestamp_ms,
                        &message.session_id,
                        message.control_seq,
                    ) > (after.timestamp_ms, &after.session_id, after.control_seq)
                })
            })
            .collect::<Vec<_>>();
        all.sort_by(|left, right| {
            (left.timestamp_ms, &left.session_id, left.control_seq).cmp(&(
                right.timestamp_ms,
                &right.session_id,
                right.control_seq,
            ))
        });
        let has_more = all.len() > limit;
        all.truncate(limit);
        let page = StoreReadyMessagePage {
            after: after.cloned(),
            messages: all,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn inspect_session(
        &self,
        session_id: &SessionId,
    ) -> Result<rsi_agent_store_protocol::StoreSessionInspection> {
        use rsi_agent_store_protocol::{StorePendingMessage, StoreSessionInspection};
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        let mut pending = state
            .agent_messages
            .iter()
            .filter(|((candidate, _), entry)| {
                candidate == session_id && matches!(entry.state, StoreAgentMessageState::Pending)
            })
            .map(|(_, entry)| StorePendingMessage {
                permits_promotion: entry.permits_promotion(),
                message_id: entry.message.message_id.clone(),
                delivery: entry.delivery,
                target: entry.target,
                bound_turn_id: entry.bound_turn_id.clone(),
                accepted_control_seq: entry.accepted_control_seq,
            })
            .collect::<Vec<_>>();
        pending.sort_by_key(|entry| entry.accepted_control_seq);
        let inspection = StoreSessionInspection {
            header: session.header.clone(),
            durable_fact_seq: session.facts.last().map_or(0, |fact| fact.seq()),
            durable_control_seq: session.controls.last().map_or(0, AgentControlRecord::seq),
            pending,
            active_turn_id: session
                .turns
                .iter()
                .filter(|(_, turn)| turn.terminal_seq.is_none())
                .min_by_key(|(_, turn)| turn.accepted_seq)
                .map(|(id, _)| id.clone()),
            activation_phase: state
                .active_activations
                .get(session_id)
                .map(|activation| activation.phase),
            tree: memory_agent_subtree(&state, session_id)?,
        };
        inspection.validate()?;
        Ok(inspection)
    }

    async fn read_agent_mailbox(
        &self,
        session_id: &SessionId,
        selected_message_id: Option<&MessageId>,
    ) -> Result<StoreAgentMailbox> {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        let durable_control_seq = session.controls.last().map_or(0, AgentControlRecord::seq);
        let durable_fact_seq = session.facts.last().map_or(0, |fact| fact.seq());
        let selected = selected_message_id
            .and_then(|message_id| {
                state
                    .agent_messages
                    .get(&(session_id.clone(), message_id.clone()))
            })
            .cloned();
        let mut pending = state
            .agent_messages
            .iter()
            .filter_map(|((candidate, _), entry)| {
                (candidate == session_id && matches!(entry.state, StoreAgentMessageState::Pending))
                    .then_some(entry)
            })
            .collect::<Vec<_>>();
        pending.sort_by_key(|entry| entry.accepted_control_seq);
        let pending_count = pending.len();
        let mut encoded_bytes = 0_usize;
        let mut bounded = Vec::new();
        for entry in pending {
            let projected = encoded_bytes
                .checked_add(entry.encoded_message_bytes)
                .ok_or_else(|| StoreError::Corrupt("mailbox byte count overflowed".into()))?;
            if projected > MAXIMUM_STORE_MAILBOX_PAGE_BYTES {
                break;
            }
            encoded_bytes = projected;
            bounded.push(entry.clone());
        }
        let mailbox = StoreAgentMailbox {
            selected,
            pending: bounded,
            pending_count,
            durable_control_seq,
            durable_fact_seq,
        };
        mailbox.validate()?;
        Ok(mailbox)
    }

    async fn read_agent_mailbox_summary(
        &self,
        session_id: &SessionId,
    ) -> Result<StoreAgentMailboxSummary> {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        let summary = StoreAgentMailboxSummary {
            pending_count: state
                .agent_messages
                .iter()
                .filter(|((candidate, _), entry)| {
                    candidate == session_id
                        && matches!(entry.state, StoreAgentMessageState::Pending)
                })
                .count(),
            pending_promotable_message_ids: {
                let mut messages = state
                    .agent_messages
                    .iter()
                    .filter(|((candidate, _), entry)| {
                        candidate == session_id
                            && matches!(entry.state, StoreAgentMessageState::Pending)
                            && entry.target == MessageTarget::NextStep
                            && !entry.wake_required
                            && entry.permits_promotion()
                    })
                    .map(|(_, entry)| {
                        (entry.accepted_control_seq, entry.message.message_id.clone())
                    })
                    .collect::<Vec<_>>();
                messages.sort_by_key(|(control_seq, _)| *control_seq);
                messages
                    .into_iter()
                    .map(|(_, message_id)| message_id)
                    .collect()
            },
            durable_control_seq: session.controls.last().map_or(0, AgentControlRecord::seq),
            durable_fact_seq: session.facts.last().map_or(0, |fact| fact.seq()),
        };
        summary.validate()?;
        Ok(summary)
    }

    async fn list_agent_children(
        &self,
        parent_session_id: &SessionId,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreAgentChildPage> {
        validate_session_read_limit(limit)?;
        if self.should_fail_agent_tree_read(parent_session_id) {
            return Err(StoreError::Io("injected Agent tree-read failure".into()));
        }
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut children = state
            .agent_children
            .iter()
            .filter(|((parent, child), _)| {
                parent == parent_session_id && after.is_none_or(|after| child > after)
            })
            .map(|(_, child)| child.clone())
            .take(limit + 1)
            .collect::<Vec<_>>();
        let has_more = children.len() > limit;
        children.truncate(limit);
        let page = StoreAgentChildPage {
            after: after.cloned(),
            children,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn read_agent_subtree_snapshot(
        &self,
        parent_session_id: &SessionId,
    ) -> Result<StoreAgentSubtreeSnapshot> {
        if self.should_fail_agent_tree_read(parent_session_id) {
            return Err(StoreError::Io("injected Agent tree-read failure".into()));
        }
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        memory_agent_subtree(&state, parent_session_id)
    }

    async fn list_ready_roots(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreReadyRootPage> {
        validate_session_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut roots = state
            .ready_messages
            .keys()
            .map(|(root, _, _, _)| root)
            .filter(|root| after.is_none_or(|after| *root > after))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(limit + 1)
            .collect::<Vec<_>>();
        let has_more = roots.len() > limit;
        roots.truncate(limit);
        let page = StoreReadyRootPage {
            after: after.cloned(),
            roots,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn active_activation(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<StoreActiveActivation>> {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.sessions.contains_key(session_id) {
            return Err(StoreError::NotFound(session_id.to_string()));
        }
        Ok(state.active_activations.get(session_id).cloned())
    }

    async fn completion_reservation_count(&self, parent_session_id: &SessionId) -> Result<usize> {
        Ok(self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active_activations
            .values()
            .filter(|activation| {
                activation.parent_session_id.as_ref() == Some(parent_session_id)
                    && activation.completion_reserved_bytes.is_some()
            })
            .count())
    }

    async fn list_waiting_activations(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreWaitingActivationPage> {
        validate_session_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut sessions = state
            .active_activations
            .iter()
            .filter(|(session_id, activation)| {
                activation.phase == StoreActivationPhase::WaitingForDescendants
                    && after.is_none_or(|after| *session_id > after)
            })
            .map(|(session_id, _)| session_id.clone())
            .take(limit + 1)
            .collect::<Vec<_>>();
        let has_more = sessions.len() > limit;
        sessions.truncate(limit);
        let page = StoreWaitingActivationPage {
            after: after.cloned(),
            sessions,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn read_context_checkpoint(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<StoredContextCheckpoint>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .get(session_id)
            .map(|session| session.checkpoint.clone())
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))
    }

    async fn write_context_checkpoint(&self, write: WriteContextCheckpoint) -> Result<()> {
        write.validate()?;
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get_mut(&write.session_id)
            .ok_or_else(|| StoreError::NotFound(write.session_id.to_string()))?;
        let actual = session.facts.last().map_or(0, |fact| fact.seq());
        if actual != write.expected_durable_seq {
            return Err(StoreError::Conflict {
                expected: write.expected_durable_seq,
                actual,
            });
        }
        if write.checkpoint.header_fingerprint
            != session.header.fingerprint().map_err(|error| {
                StoreError::Corrupt(format!("stored session header is invalid: {error}"))
            })?
        {
            return Err(StoreError::Invalid(
                "checkpoint header fingerprint differs from the durable session".into(),
            ));
        }
        if write.checkpoint.fact_prefix_sha256 != hex::encode(session.fact_prefix_digest) {
            return Err(StoreError::Invalid(
                "checkpoint Fact-prefix digest differs from the durable session".into(),
            ));
        }
        session.checkpoint = Some(write.checkpoint);
        Ok(())
    }

    async fn put_cas(&self, bytes: Arc<[u8]>) -> Result<CasObjectRef> {
        if bytes.is_empty() || bytes.len() > MAXIMUM_STORE_CAS_BYTES {
            return Err(StoreError::Invalid(
                "CAS bytes must be nonempty and bounded".into(),
            ));
        }
        let reference = CasObjectRef {
            sha256: hex::encode(Sha256::digest(&bytes)),
            byte_len: bytes.len() as u64,
        };
        reference.validate()?;
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cas
            .entry(reference.sha256.clone())
            .or_insert(bytes);
        Ok(reference)
    }

    async fn read_cas(&self, object: &CasObjectRef) -> Result<Arc<[u8]>> {
        object.validate()?;
        let bytes = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cas
            .get(&object.sha256)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(object.sha256.clone()))?;
        if bytes.len() as u64 != object.byte_len
            || hex::encode(Sha256::digest(&bytes)) != object.sha256
        {
            return Err(StoreError::Corrupt(
                "CAS bytes do not match their reference".into(),
            ));
        }
        Ok(bytes)
    }
}

fn domain_control(session: &MemorySession, seq: u64) -> Result<&AgentControlRecord> {
    let index = seq
        .checked_sub(1)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| StoreError::Corrupt("invalid domain control position".into()))?;
    session
        .controls
        .get(index)
        .filter(|record| record.seq() == seq)
        .ok_or_else(|| StoreError::Corrupt("domain control is absent".into()))
}

fn apply_domain_updates(
    session: &mut MemorySession,
    minimum_fact_seq: u64,
    controls: &[AgentControlRecord],
) -> Result<()> {
    use rsi_agent_session_protocol::DomainMutationSource;
    for record in controls {
        let AgentControlRecordBody::DomainStateCommitted { commit } = record.body() else {
            continue;
        };
        if let Some(request) = commit.request_id()
            && session.domain_requests.contains_key(request)
        {
            return Err(StoreError::DomainRequestConflict {
                request_id: request.to_string(),
            });
        }
        if let DomainMutationSource::Turn { turn_id, .. } = commit.source() {
            let live = session.turns.get(turn_id).is_some_and(|turn| {
                turn.terminal_seq
                    .is_none_or(|terminal| terminal >= minimum_fact_seq)
            });
            if !live {
                return Err(StoreError::Invalid(
                    "domain mutation has no open originating Turn".into(),
                ));
            }
            let usage = session.domain_usage.entry(turn_id.clone()).or_default();
            usage.0 = usage
                .0
                .checked_add(1)
                .ok_or_else(|| StoreError::Invalid("domain record count overflow".into()))?;
            usage.1 = usage
                .1
                .checked_add(record.encoded_len() as u64)
                .ok_or_else(|| StoreError::Invalid("domain byte count overflow".into()))?;
        }
        let heads: Vec<_> = session
            .domain_versions
            .values()
            .filter_map(|versions| versions.last().cloned())
            .collect();
        let next = rsi_agent_store_protocol::domain_heads_after(&heads, record.seq(), commit)?;
        for head in next
            .into_iter()
            .filter(|head| head.control_seq == record.seq())
        {
            session
                .domain_versions
                .entry(head.identity.id().into())
                .or_default()
                .push(head);
        }
        if let Some(request) = commit.request_id() {
            session
                .domain_requests
                .insert(request.clone(), record.seq());
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // Keep the mechanical atomic-append mirror auditable as one transaction state transition.
fn apply_atomic_memory_append(
    state: &mut MemoryState,
    append: AtomicSessionAppend,
) -> Result<AgentCommitWatermark> {
    let session_id = append.session_id.clone();
    let minimum_entered_fact_seq = append
        .expected_fact_seq
        .checked_add(1)
        .ok_or_else(|| StoreError::Invalid("Fact sequence is exhausted".into()))?;
    if let Some(session) = state.sessions.get_mut(&session_id) {
        if append.header.is_some() {
            return Err(StoreError::Invalid(
                "existing session cannot replace its immutable Header".into(),
            ));
        }
        let actual_fact = session.facts.last().map_or(0, |fact| fact.seq());
        let actual_control = session.controls.last().map_or(0, AgentControlRecord::seq);
        if actual_fact != append.expected_fact_seq {
            return Err(StoreError::Conflict {
                expected: append.expected_fact_seq,
                actual: actual_fact,
            });
        }
        if actual_control != append.expected_control_seq {
            return Err(StoreError::ControlConflict {
                session: session_id.to_string(),
                expected: append.expected_control_seq,
                actual: actual_control,
            });
        }
        let (turn_updates, fact_digest) =
            index_appended_turns(&session.turns, &append.facts, session.fact_prefix_digest)?;
        let control_digest =
            append
                .controls
                .iter()
                .try_fold(session.control_prefix_digest, |digest, record| {
                    advance_control_prefix_digest(digest, record)
                        .map_err(|error| StoreError::Invalid(error.to_string()))
                })?;
        session.facts.extend(append.facts);
        session.turns.extend(turn_updates);
        session.fact_prefix_digest = fact_digest;
        session.controls.extend(append.controls.clone());
        session.control_prefix_digest = control_digest;
        apply_message_updates(
            state,
            &session_id,
            minimum_entered_fact_seq,
            &append.controls,
        )?;
        apply_ready_updates(state, &session_id, &append.controls)?;
        apply_activation_updates(state, &session_id, &append.controls)?;
    } else {
        let header = append
            .header
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        validate_memory_agent_node(state, &header)?;
        let (turns, fact_digest) =
            index_appended_turns(&BTreeMap::new(), &append.facts, EMPTY_FACT_PREFIX_DIGEST)?;
        let control_digest =
            append
                .controls
                .iter()
                .try_fold(EMPTY_CONTROL_PREFIX_DIGEST, |digest, record| {
                    advance_control_prefix_digest(digest, record)
                        .map_err(|error| StoreError::Invalid(error.to_string()))
                })?;
        state
            .recent_sessions
            .insert((header.created_at_ms(), session_id.clone()));
        if let Some(origin) = header.fork_origin() {
            state.agent_children.insert(
                (origin.parent_session_id.clone(), session_id.clone()),
                StoreAgentChild {
                    session_id: session_id.clone(),
                    path: origin.path.clone(),
                    task_name: origin.task_name.clone(),
                },
            );
        }
        state.sessions.insert(
            session_id.clone(),
            MemorySession {
                header,
                facts: append.facts,
                turns,
                fact_prefix_digest: fact_digest,
                checkpoint: None,
                controls: append.controls.clone(),
                last_settled_control_seq: 0,
                control_prefix_digest: control_digest,
                domain_versions: BTreeMap::new(),
                domain_requests: BTreeMap::new(),
                domain_usage: BTreeMap::new(),
            },
        );
        apply_message_updates(
            state,
            &session_id,
            minimum_entered_fact_seq,
            &append.controls,
        )?;
        apply_ready_updates(state, &session_id, &append.controls)?;
        apply_activation_updates(state, &session_id, &append.controls)?;
    }
    let session = state
        .sessions
        .get_mut(&session_id)
        .expect("atomic append installed or updated its session");
    apply_domain_updates(session, minimum_entered_fact_seq, &append.controls)?;
    if let Some(record) = append.controls.iter().rev().find(|record| {
        matches!(
            record.body(),
            AgentControlRecordBody::ActivationSettled { .. }
        )
    }) {
        session.last_settled_control_seq = record.seq();
    }
    if let Some(record) = append.controls.last()
        && let AgentControlRecordBody::TurnBoundaryRecorded {
            turn_id,
            terminal_fact_seq,
        } = record.body()
    {
        let boundary = session
            .turns
            .get_mut(turn_id)
            .ok_or_else(|| StoreError::Corrupt("terminal marker lost its Turn".into()))?;
        if boundary.terminal_seq != Some(*terminal_fact_seq) || boundary.terminal_control.is_some()
        {
            return Err(StoreError::Corrupt(
                "terminal marker differs from its Fact".into(),
            ));
        }
        boundary.terminal_control =
            Some((record.seq(), hex::encode(session.control_prefix_digest)));
    }
    Ok(AgentCommitWatermark {
        session_id,
        durable_fact_seq: session.facts.last().map_or(0, |fact| fact.seq()),
        durable_control_seq: session.controls.last().map_or(0, AgentControlRecord::seq),
    })
}

fn validate_memory_agent_node(state: &MemoryState, header: &SessionHeader) -> Result<()> {
    let Some(origin) = header.fork_origin() else {
        return Ok(());
    };
    let parent = state
        .sessions
        .get(&origin.parent_session_id)
        .ok_or_else(|| StoreError::Invalid("Agent parent session is not durable".into()))?;
    let expected_root = parent
        .header
        .fork_origin()
        .map_or(parent.header.session_id(), |parent_origin| {
            &parent_origin.root_session_id
        });
    if &origin.root_session_id != expected_root {
        return Err(StoreError::Invalid(
            "Agent child root differs from its parent's durable root".into(),
        ));
    }
    let child_count = state
        .sessions
        .values()
        .filter(|session| {
            session
                .header
                .fork_origin()
                .is_some_and(|candidate| candidate.root_session_id == origin.root_session_id)
        })
        .count();
    if child_count.saturating_add(1) >= rsi_agent_session_protocol::MAXIMUM_DURABLE_AGENT_TREE_NODES
    {
        return Err(StoreError::Invalid(
            "Agent tree exceeds its durable node bound".into(),
        ));
    }
    for ((parent_session_id, _), child) in &state.agent_children {
        let same_tree = state
            .sessions
            .get(&child.session_id)
            .and_then(|session| session.header.fork_origin())
            .is_some_and(|candidate| candidate.root_session_id == origin.root_session_id);
        if same_tree && child.path == origin.path {
            return Err(StoreError::Invalid(
                "Agent tree path is already present".into(),
            ));
        }
        if parent_session_id == &origin.parent_session_id && child.task_name == origin.task_name {
            return Err(StoreError::Invalid(
                "Agent task name is already present below its parent".into(),
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // Keep one closed atomic mailbox-index transition table auditable in source order.
fn apply_message_updates(
    state: &mut MemoryState,
    session_id: &SessionId,
    minimum_entered_fact_seq: u64,
    controls: &[AgentControlRecord],
) -> Result<()> {
    for record in controls {
        match record.body() {
            AgentControlRecordBody::MessageAccepted {
                message,
                delivery,
                bound_turn_id,
                root_session_id,
                target,
                wake_required,
            } => {
                let header = &state
                    .sessions
                    .get(session_id)
                    .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?
                    .header;
                let expected_root = header
                    .fork_origin()
                    .map_or(header.session_id(), |origin| &origin.root_session_id);
                if root_session_id != expected_root {
                    return Err(StoreError::Invalid(
                        "Agent message root differs from its target Session root".into(),
                    ));
                }
                let pending = state
                    .agent_messages
                    .iter()
                    .filter(|((candidate, _), entry)| {
                        candidate == session_id
                            && matches!(entry.state, StoreAgentMessageState::Pending)
                    })
                    .count();
                let reservations = state
                    .active_activations
                    .values()
                    .filter(|activation| {
                        activation.parent_session_id.as_ref() == Some(session_id)
                            && activation.completion_reserved_bytes.is_some()
                    })
                    .count();
                if pending.saturating_add(reservations)
                    >= rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES
                {
                    return Err(StoreError::Invalid(
                        "mailbox exceeds its pending-message bound".into(),
                    ));
                }
                let key = (session_id.clone(), message.message_id.clone());
                if state
                    .agent_messages
                    .insert(
                        key,
                        StoreAgentMessage {
                            delivery: *delivery,
                            bound_turn_id: bound_turn_id.clone(),
                            accepted_timestamp_ms: record.timestamp_ms(),
                            message: message.clone(),
                            encoded_message_bytes: serde_json::to_vec(message)
                                .map_err(|error| StoreError::Invalid(error.to_string()))?
                                .len(),
                            root_session_id: root_session_id.clone(),
                            target: *target,
                            wake_required: *wake_required,
                            accepted_control_seq: record.seq(),
                            state: StoreAgentMessageState::Pending,
                        },
                    )
                    .is_some()
                {
                    return Err(StoreError::Corrupt(
                        "mailbox index repeats a message identity".into(),
                    ));
                }
            }
            AgentControlRecordBody::MessageClaimed {
                message_id,
                activation_id,
                turn_id,
                step_id,
                entered_fact_seq,
            } => {
                let key = (session_id.clone(), message_id.clone());
                let message = state
                    .agent_messages
                    .get(&key)
                    .ok_or_else(|| {
                        StoreError::Corrupt("mailbox claim has no indexed message".into())
                    })?
                    .clone();
                if !matches!(message.state, StoreAgentMessageState::Pending) {
                    return Err(StoreError::Corrupt(
                        "mailbox claim references a non-pending message".into(),
                    ));
                }
                message.validate_claim_turn(turn_id)?;
                let fact = usize::try_from(*entered_fact_seq)
                    .ok()
                    .and_then(|sequence| sequence.checked_sub(1))
                    .and_then(|index| {
                        state
                            .sessions
                            .get(session_id)
                            .and_then(|session| session.facts.get(index))
                    });
                validate_message_claim_fact(
                    &message.message,
                    turn_id,
                    step_id,
                    minimum_entered_fact_seq,
                    fact.map(AsRef::as_ref),
                )?;
                let entry = state
                    .agent_messages
                    .get_mut(&key)
                    .expect("validated mailbox message remains indexed");
                entry.state = StoreAgentMessageState::Claimed {
                    activation_id: activation_id.clone(),
                    turn_id: turn_id.clone(),
                    step_id: step_id.clone(),
                    entered_fact_seq: *entered_fact_seq,
                };
            }
            AgentControlRecordBody::MessagePromoted { message_id } => {
                let entry = state
                    .agent_messages
                    .get_mut(&(session_id.clone(), message_id.clone()))
                    .ok_or_else(|| {
                        StoreError::Corrupt("mailbox promotion has no indexed message".into())
                    })?;
                if !matches!(entry.state, StoreAgentMessageState::Pending)
                    || entry.target != MessageTarget::NextStep
                    || entry.wake_required
                    || !entry.permits_promotion()
                {
                    return Err(StoreError::Corrupt(
                        "mailbox promotion requires eligible pending next-Step input".into(),
                    ));
                }
                entry.target = MessageTarget::NextTurn;
                entry.wake_required = true;
            }
            AgentControlRecordBody::MessageDiscarded { message_id, reason } => {
                let entry = state
                    .agent_messages
                    .get_mut(&(session_id.clone(), message_id.clone()))
                    .ok_or_else(|| {
                        StoreError::Corrupt("mailbox discard has no indexed message".into())
                    })?;
                if !matches!(entry.state, StoreAgentMessageState::Pending) {
                    return Err(StoreError::Corrupt(
                        "mailbox discard references a non-pending message".into(),
                    ));
                }
                entry.state = StoreAgentMessageState::Discarded {
                    reason: *reason,
                    control_seq: record.seq(),
                };
            }
            AgentControlRecordBody::ActivationStarted { .. }
            | AgentControlRecordBody::ActivationWaitingForDescendants { .. }
            | AgentControlRecordBody::ActivationSettled { .. }
            | AgentControlRecordBody::WaitParked { .. }
            | AgentControlRecordBody::WaitResumed { .. }
            | AgentControlRecordBody::CompletionReserved { .. }
            | AgentControlRecordBody::TurnBoundaryRecorded { .. }
            | AgentControlRecordBody::DomainStateCommitted { .. } => {}
        }
    }
    Ok(())
}

fn validate_memory_activation_guards(
    state: &MemoryState,
    commit: &AtomicAgentCommit,
) -> Result<()> {
    for guard in &commit.required_active_activations {
        if state
            .active_activations
            .get(&guard.session_id)
            .is_none_or(|activation| activation.activation_id != guard.activation_id)
        {
            return Err(StoreError::ActivationGuardConflict {
                session: guard.session_id.to_string(),
            });
        }
    }
    Ok(())
}

fn validate_memory_quiescence_guard(state: &MemoryState, root: Option<&SessionId>) -> Result<()> {
    if let Some(root) = root {
        for descendant in memory_agent_subtree(state, root)?.descendants {
            let status = descendant.status;
            if status.has_active_activation || status.has_open_turn || status.has_waking_message {
                return Err(StoreError::SessionNotQuiescent {
                    session: status.session_id.to_string(),
                });
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // One closed control vocabulary updates the activation projection in a single scan.
fn apply_activation_updates(
    state: &mut MemoryState,
    session_id: &SessionId,
    controls: &[AgentControlRecord],
) -> Result<()> {
    for record in controls {
        match record.body() {
            AgentControlRecordBody::ActivationStarted {
                activation_id,
                parent_session_id,
                root_session_id,
                path,
            } => {
                let header = &state
                    .sessions
                    .get(session_id)
                    .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?
                    .header;
                rsi_agent_store_protocol::validate_activation_lineage(
                    header,
                    root_session_id,
                    parent_session_id.as_ref(),
                    path,
                )?;
                if state.active_activations.contains_key(session_id) {
                    return Err(StoreError::Corrupt(
                        "activation start follows an unsettled activation".into(),
                    ));
                }
                state.active_activations.insert(
                    session_id.clone(),
                    StoreActiveActivation {
                        activation_id: activation_id.clone(),
                        parent_session_id: parent_session_id.clone(),
                        turn_id: None,
                        phase: StoreActivationPhase::Running,
                        completion_reserved_bytes: None,
                    },
                );
            }
            AgentControlRecordBody::ActivationWaitingForDescendants { activation_id } => {
                let activation = state
                    .active_activations
                    .get_mut(session_id)
                    .ok_or_else(|| {
                        StoreError::Corrupt("activation wait has no active activation".into())
                    })?;
                if &activation.activation_id != activation_id
                    || activation.phase != StoreActivationPhase::Running
                {
                    return Err(StoreError::Corrupt(
                        "activation wait does not match a running activation".into(),
                    ));
                }
                activation.phase = StoreActivationPhase::WaitingForDescendants;
            }
            AgentControlRecordBody::CompletionReserved {
                activation_id,
                parent_session_id,
                maximum_bytes,
            } => {
                let pending = state
                    .agent_messages
                    .iter()
                    .filter(|((candidate, _), entry)| {
                        candidate == parent_session_id
                            && matches!(entry.state, StoreAgentMessageState::Pending)
                    })
                    .count();
                let reservations = state
                    .active_activations
                    .values()
                    .filter(|candidate| {
                        candidate.parent_session_id.as_ref() == Some(parent_session_id)
                            && candidate.completion_reserved_bytes.is_some()
                    })
                    .count();
                if pending.saturating_add(reservations)
                    >= rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES
                {
                    return Err(StoreError::Invalid(
                        "parent mailbox has no completion-reservation capacity".into(),
                    ));
                }
                let activation = state
                    .active_activations
                    .get_mut(session_id)
                    .ok_or_else(|| {
                        StoreError::Corrupt(
                            "completion reservation has no active activation".into(),
                        )
                    })?;
                if &activation.activation_id != activation_id
                    || activation.parent_session_id.as_ref() != Some(parent_session_id)
                    || activation.completion_reserved_bytes.is_some()
                {
                    return Err(StoreError::Corrupt(
                        "completion reservation disagrees with its active child".into(),
                    ));
                }
                activation.completion_reserved_bytes = Some(*maximum_bytes);
            }
            AgentControlRecordBody::MessageClaimed {
                activation_id,
                turn_id,
                ..
            } => {
                let activation = state
                    .active_activations
                    .get_mut(session_id)
                    .ok_or_else(|| {
                        StoreError::Corrupt("message claim has no active activation".into())
                    })?;
                if &activation.activation_id != activation_id
                    || activation
                        .turn_id
                        .as_ref()
                        .is_some_and(|active_turn| active_turn != turn_id)
                {
                    return Err(StoreError::Corrupt(
                        "message claim disagrees with its active activation".into(),
                    ));
                }
                activation.turn_id.get_or_insert_with(|| turn_id.clone());
            }
            AgentControlRecordBody::ActivationSettled { activation_id, .. } => {
                let activation = state.active_activations.get(session_id).ok_or_else(|| {
                    StoreError::Corrupt("activation settlement has no active activation".into())
                })?;
                if &activation.activation_id != activation_id
                    || activation.parent_session_id.is_some()
                        && activation.completion_reserved_bytes.is_none()
                {
                    return Err(StoreError::Corrupt(
                        "activation settlement disagrees with its active reservation".into(),
                    ));
                }
                state.active_activations.remove(session_id);
            }
            AgentControlRecordBody::WaitParked {
                activation_id,
                turn_id,
                ..
            } => {
                let activation = state
                    .active_activations
                    .get_mut(session_id)
                    .ok_or_else(|| StoreError::Corrupt("parked wait has no activation".into()))?;
                if &activation.activation_id != activation_id
                    || activation.turn_id.as_ref() != Some(turn_id)
                    || activation.phase != StoreActivationPhase::Running
                {
                    return Err(StoreError::Corrupt(
                        "parked wait disagrees with its running activation".into(),
                    ));
                }
                activation.phase = StoreActivationPhase::Parked;
            }
            AgentControlRecordBody::WaitResumed {
                activation_id,
                turn_id,
                ..
            } => {
                let activation = state
                    .active_activations
                    .get_mut(session_id)
                    .ok_or_else(|| StoreError::Corrupt("resumed wait has no activation".into()))?;
                if &activation.activation_id != activation_id
                    || activation.turn_id.as_ref() != Some(turn_id)
                    || activation.phase != StoreActivationPhase::Parked
                {
                    return Err(StoreError::Corrupt(
                        "resumed wait disagrees with its parked activation".into(),
                    ));
                }
                activation.phase = StoreActivationPhase::Running;
            }
            AgentControlRecordBody::MessageAccepted {
                bound_turn_id: Some(bound),
                ..
            } => {
                if !state
                    .active_activations
                    .get(session_id)
                    .is_some_and(|active| {
                        active.turn_id.as_ref() == Some(bound)
                            && matches!(
                                active.phase,
                                StoreActivationPhase::Running | StoreActivationPhase::Parked
                            )
                    })
                {
                    return Err(StoreError::Invalid(
                        "steering binding requires the current activation Turn".into(),
                    ));
                }
            }
            AgentControlRecordBody::MessageAccepted {
                bound_turn_id: None,
                ..
            }
            | AgentControlRecordBody::MessagePromoted { .. }
            | AgentControlRecordBody::MessageDiscarded { .. }
            | AgentControlRecordBody::TurnBoundaryRecorded { .. }
            | AgentControlRecordBody::DomainStateCommitted { .. } => {}
        }
    }
    Ok(())
}

fn apply_ready_updates(
    state: &mut MemoryState,
    session_id: &SessionId,
    controls: &[AgentControlRecord],
) -> Result<()> {
    for record in controls {
        match record.body() {
            AgentControlRecordBody::MessageAccepted {
                message,
                root_session_id,
                target,
                wake_required: true,
                ..
            } => {
                let message_key = (session_id.clone(), message.message_id.clone());
                if state.ready_keys.contains_key(&message_key) {
                    return Err(StoreError::Corrupt(
                        "ready index repeats a message identity".into(),
                    ));
                }
                let key = (
                    root_session_id.clone(),
                    record.timestamp_ms(),
                    session_id.clone(),
                    record.seq(),
                );
                state.ready_messages.insert(
                    key.clone(),
                    StoreReadyMessage {
                        session_id: session_id.clone(),
                        message_id: message.message_id.clone(),
                        source_kind: message.source.kind(),
                        control_seq: record.seq(),
                        timestamp_ms: record.timestamp_ms(),
                        target: *target,
                    },
                );
                state.ready_keys.insert(message_key, key);
            }
            AgentControlRecordBody::MessageClaimed { message_id, .. }
            | AgentControlRecordBody::MessageDiscarded { message_id, .. } => {
                let message_key = (session_id.clone(), message_id.clone());
                if let Some(key) = state.ready_keys.remove(&message_key) {
                    state.ready_messages.remove(&key);
                }
            }
            AgentControlRecordBody::MessagePromoted { message_id } => {
                let message_key = (session_id.clone(), message_id.clone());
                if state.ready_keys.contains_key(&message_key) {
                    return Err(StoreError::Corrupt(
                        "ready index repeats a promoted message identity".into(),
                    ));
                }
                let entry = state.agent_messages.get(&message_key).ok_or_else(|| {
                    StoreError::Corrupt("ready promotion has no indexed message".into())
                })?;
                let (timestamp_ms, control_seq) =
                    entry.promotion_order(record.timestamp_ms(), record.seq());
                let key = (
                    entry.root_session_id.clone(),
                    timestamp_ms,
                    session_id.clone(),
                    control_seq,
                );
                state.ready_messages.insert(
                    key.clone(),
                    StoreReadyMessage {
                        session_id: session_id.clone(),
                        message_id: message_id.clone(),
                        source_kind: entry.message.source.kind(),
                        control_seq,
                        timestamp_ms,
                        target: MessageTarget::NextTurn,
                    },
                );
                state.ready_keys.insert(message_key, key);
            }
            AgentControlRecordBody::MessageAccepted {
                wake_required: false,
                ..
            }
            | AgentControlRecordBody::ActivationStarted { .. }
            | AgentControlRecordBody::ActivationWaitingForDescendants { .. }
            | AgentControlRecordBody::ActivationSettled { .. }
            | AgentControlRecordBody::WaitParked { .. }
            | AgentControlRecordBody::WaitResumed { .. }
            | AgentControlRecordBody::CompletionReserved { .. }
            | AgentControlRecordBody::TurnBoundaryRecorded { .. }
            | AgentControlRecordBody::DomainStateCommitted { .. } => {}
        }
    }
    Ok(())
}

fn index_appended_turns(
    turns: &BTreeMap<TurnId, MemoryTurnBoundary>,
    facts: &[Arc<SessionFact>],
    mut prefix_digest: [u8; 32],
) -> Result<(BTreeMap<TurnId, MemoryTurnBoundary>, [u8; 32])> {
    let mut updates = BTreeMap::new();
    for fact in facts {
        prefix_digest = advance_fact_prefix_digest(prefix_digest, fact)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let role = rsi_agent_store_protocol::store_fact_turn_role(fact.body());
        let turn_id = fact.body().turn_id();
        match role {
            StoreFactTurnRole::Acceptance => {
                if turns.contains_key(turn_id) || updates.contains_key(turn_id) {
                    return Err(StoreError::Corrupt(role.rejected_message().into()));
                }
                updates.insert(
                    turn_id.clone(),
                    MemoryTurnBoundary {
                        accepted_seq: fact.seq(),
                        terminal_seq: None,
                        terminal_prefix_sha256: None,
                        terminal_control: None,
                    },
                );
            }
            StoreFactTurnRole::Terminal => {
                if !updates.contains_key(turn_id) {
                    let boundary = turns
                        .get(turn_id)
                        .cloned()
                        .ok_or_else(|| StoreError::Corrupt(role.rejected_message().into()))?;
                    updates.insert(turn_id.clone(), boundary);
                }
                let boundary = updates.get_mut(turn_id).expect("boundary was inserted");
                if boundary.terminal_seq.is_some() {
                    return Err(StoreError::Corrupt(role.rejected_message().into()));
                }
                boundary.terminal_seq = Some(fact.seq());
                boundary.terminal_prefix_sha256 = Some(hex::encode(prefix_digest));
            }
            StoreFactTurnRole::Event => {
                if updates
                    .get(turn_id)
                    .or_else(|| turns.get(turn_id))
                    .is_none_or(|boundary| boundary.terminal_seq.is_some())
                {
                    return Err(StoreError::Corrupt(role.rejected_message().into()));
                }
            }
        }
    }
    Ok((updates, prefix_digest))
}

fn memory_agent_subtree(
    state: &MemoryState,
    root: &SessionId,
) -> Result<StoreAgentSubtreeSnapshot> {
    let status = |id: &SessionId| -> Result<StoreAgentSessionStatus> {
        let session = state
            .sessions
            .get(id)
            .ok_or_else(|| StoreError::NotFound(id.to_string()))?;
        Ok(StoreAgentSessionStatus {
            last_settled_control_seq: session.last_settled_control_seq,
            session_id: id.clone(),
            durable_control_seq: session.controls.last().map_or(0, AgentControlRecord::seq),
            has_open_turn: session
                .turns
                .values()
                .any(|turn| turn.terminal_seq.is_none()),
            has_active_activation: state.active_activations.contains_key(id),
            has_waking_message: state
                .ready_keys
                .keys()
                .any(|(candidate, _)| candidate == id),
        })
    };
    let session = status(root)?;
    let mut visited = BTreeSet::from([root.clone()]);
    let mut pending = vec![root.clone()];
    let mut descendants = Vec::new();
    while let Some(parent) = pending.pop() {
        for ((candidate, id), child) in &state.agent_children {
            if candidate != &parent {
                continue;
            }
            if visited.len() >= rsi_agent_session_protocol::MAXIMUM_DURABLE_AGENT_TREE_NODES
                || !visited.insert(id.clone())
            {
                return Err(StoreError::Corrupt(
                    "Agent subtree exceeds its node bound or contains a cycle".into(),
                ));
            }
            pending.push(id.clone());
            descendants.push(StoreAgentDescendantStatus {
                status: status(id)?,
                parent_session_id: parent.clone(),
                path: child.path.clone(),
                task_name: child.task_name.clone(),
            });
        }
    }
    descendants.sort_by(|left, right| left.status.session_id.cmp(&right.status.session_id));
    let snapshot = StoreAgentSubtreeSnapshot {
        session,
        descendants,
    };
    snapshot.validate()?;
    Ok(snapshot)
}
