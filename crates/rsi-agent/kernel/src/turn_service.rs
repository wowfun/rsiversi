use super::observation::durable_message_entry;
use super::*;
use rsi_agent_session_protocol::{
    AgentMessageSourceKind, CancelReason, MessageDelivery, SessionPermission,
};

impl AgentKernel {
    async fn append_session_control(
        &self,
        admission: SubmissionAdmissionLease,
        cut: rsi_agent_store_protocol::StoreSessionWatermarks,
        body: AgentControlRecordBody,
    ) -> TurnResult<std::num::NonZeroU64> {
        let seq = cut
            .durable_control_seq
            .checked_add(1)
            .and_then(std::num::NonZeroU64::new)
            .ok_or(TurnError::Capacity)?;
        let record = AgentControlRecord::new(seq.get(), self.inner.clock.now_ms().max(1), body)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let kernel = self.clone();
        self.owned_commit(async move {
            let _admission = admission;
            kernel
                .commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
                    sessions: vec![AtomicSessionAppend {
                        session_id: cut.session_id,
                        expected_fact_seq: cut.durable_fact_seq,
                        expected_control_seq: cut.durable_control_seq,
                        header: None,
                        facts: vec![],
                        controls: vec![record],
                    }],
                    required_active_activations: vec![],
                    quiescent_descendants_of: None,
                })
                .await?
                .map_err(turn_store_error)?;
            Ok(seq)
        })
        .await
    }

    pub(super) async fn current_permission(
        &self,
        header: &SessionHeader,
        composition: &AgentCompositionPin,
    ) -> TurnResult<Option<SessionPermission>> {
        if !composition.domains().contains(SessionPermission::DOMAIN) {
            return Ok(None);
        }
        let Some((_, state)) = self
            .domain_state(header.session_id(), SessionPermission::DOMAIN)
            .await?
        else {
            return Ok(None);
        };
        SessionPermission::from_snapshot(&state.snapshot)
            .map_err(|error| TurnError::Invalid(error.to_string()))
    }

    async fn session_sandbox(
        &self,
        session: &SubmitSession,
        explicit: Option<rsi_sandbox::SandboxMode>,
    ) -> TurnResult<rsi_sandbox::SandboxMode> {
        if let Some(mode) = explicit {
            return delegated_sandbox(session.header(), mode);
        }
        let selected = match session {
            SubmitSession::Fresh(prepared) => prepared
                .baseline()
                .initial_state(SessionPermission::DOMAIN)
                .map(SessionPermission::from_snapshot)
                .transpose()
                .map_err(|error| TurnError::Invalid(error.to_string()))?
                .flatten(),
            SubmitSession::Resume(prepared) => {
                let (header, composition) = self.inner.resume_issuer.inspect(prepared)?;
                self.current_permission(header, composition).await?
            }
        };
        Ok(SessionPermission::bounded_sandbox(
            session.header(),
            selected.map_or(session.header().settings().sandbox(), |value| value.sandbox),
        ))
    }
}

fn delegated_sandbox(
    header: &SessionHeader,
    mode: rsi_sandbox::SandboxMode,
) -> TurnResult<rsi_sandbox::SandboxMode> {
    if SessionPermission::bounded_sandbox(header, mode) == mode {
        Ok(mode)
    } else {
        Err(TurnError::Invalid(
            "Requested sandbox exceeds the delegated Session ceiling".into(),
        ))
    }
}

pub(super) enum MessageAdmission<'a> {
    Session,
    Agent(&'a AgentCallerAuthority, &'a CancellationToken),
    BackgroundJob,
}

#[async_trait]
impl TurnService for AgentKernel {
    async fn working_directory(
        &self,
        header: &SessionHeader,
        path: Option<&str>,
    ) -> TurnResult<rsi_agent_session_protocol::ExecutionCoordinates> {
        if path.is_some() {
            return self.change_working_directory(header, path, None).await;
        }
        let cached = self.current_working_directory(header).await?;
        let selected = self.ensure_coordinates(header, cached).await?;
        Ok(selected
            .as_deref()
            .unwrap_or_else(|| header.coordinates())
            .clone())
    }
    async fn create_plugin_child(
        &self,
        request: rsi_agent_turn_protocol::CreatePluginChild,
    ) -> TurnResult<SessionHeader> {
        use rsi_agent_session_protocol::{ExecutionOwner, ModelSelection, PluginToolOrigin};
        use rsi_agent_turn_protocol::PluginChildSource;
        let (parent, caller) = match &request.source {
            PluginChildSource::Session(parent) => (parent.as_ref(), None),
            PluginChildSource::Tool(caller) => (caller.header(), Some(caller.as_ref())),
        };
        if parent.session_id() == &request.session_id {
            return Err(TurnError::Invalid(
                "a plugin child requires a fresh identity".into(),
            ));
        }
        let admission = self
            .inner
            .submission_admission
            .acquire_pair(&request.session_id, Some(parent.session_id()))
            .await?;
        let tool = if let Some(caller) = caller {
            self.validate_agent_caller(caller)?;
            Some(PluginToolOrigin {
                turn_id: caller.turn_id().clone(),
                effect_id: caller
                    .tool_effect_id()
                    .ok_or(TurnError::StaleClaim)?
                    .clone(),
            })
        } else {
            None
        };
        let (_, parent_path) = agent_root_and_path(parent);
        let path = self
            .next_child_path(parent.session_id(), &parent_path, &request.task_name)
            .await?;
        let directory = match request.directory {
            Some(directory) => Some(directory),
            None => self
                .current_working_directory(parent)
                .await?
                .as_deref()
                .cloned(),
        };
        let preset = match (request.preset, caller) {
            (Some(id), _) => Some(id),
            (None, Some(caller)) => Some(self.composition(caller.claim())?.preset_id().clone()),
            (None, None) => match self
                .inner
                .store
                .read_executable_generation(parent.session_id())
                .await
            {
                Ok((_, generation)) => generation.and_then(|row| row.selection.preset().cloned()),
                Err(StoreError::NotFound(_)) => None,
                Err(error) => return Err(turn_store_error(error)),
            },
        };
        let selected = match self
            .domain_state(parent.session_id(), SessionPermission::DOMAIN)
            .await
        {
            Ok(Some((_, value))) => SessionPermission::from_snapshot(&value.snapshot)
                .map_err(|error| TurnError::Invalid(error.to_string()))?,
            Ok(None) => None,
            Err(TurnError::SessionNotFound(_)) if caller.is_none() => None,
            Err(error) => return Err(error),
        };
        let child = parent
            .plugin_child(
                request.session_id,
                self.inner.clock.now_ms().max(1),
                path,
                request.task_name,
                ExecutionOwner::Plugin {
                    session_id: parent.session_id().clone(),
                    plugin: request.plugin,
                    tool,
                },
                rsi_agent_session_protocol::PluginChildTarget {
                    coordinates: directory,
                    preset,
                    offered_tools: caller.and_then(AgentCallerAuthority::source_tools),
                    selection: caller
                        .and_then(AgentCallerAuthority::source_selection)
                        .cloned()
                        .unwrap_or_else(|| ModelSelection::baseline(parent.settings())),
                },
            )
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let sandbox = SessionPermission::bounded_sandbox(
            parent,
            selected.map_or(parent.settings().sandbox(), |value| value.sandbox),
        );
        let (sandbox, require_approval) = if let Some(caller) = caller {
            let input = rsi_agent_turn_protocol::initial_turn_input(self, caller.claim()).await?;
            (
                SessionPermission::narrower_sandbox(sandbox, input.sandbox),
                parent.settings().require_approval() || input.require_approval,
            )
        } else {
            (sandbox, parent.settings().require_approval())
        };
        let child = child
            .with_execution_policy(sandbox, require_approval)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        self.inner
            .composition
            .pin_session(&child, None)
            .await
            .map_err(turn_composition_error)?;
        let kernel = self.clone();
        self.owned_commit(async move {
            let _admission = admission;
            if let PluginChildSource::Tool(caller) = &request.source {
                kernel.validate_agent_caller(caller)?;
            }
            kernel
                .inner
                .store
                .save_blank(child.clone(), None)
                .await
                .map_err(turn_store_error)?;
            kernel.inner.session_changes.committed(child.session_id());
            Ok(child)
        })
        .await
    }
    async fn record_workspace_changes(&self, id: &SessionId, turn: &TurnId) -> TurnResult<()> {
        let admission = self.inner.submission_admission.acquire(id).await?;
        let cut = self
            .inner
            .store
            .read_watermarks(id)
            .await
            .map_err(turn_store_error)?;
        self.append_session_control(
            admission,
            cut,
            AgentControlRecordBody::WorkspaceChangesRecorded {
                turn_id: turn.clone(),
            },
        )
        .await?;
        Ok(())
    }

    async fn change_message_feedback(
        &self,
        id: &SessionId,
        change: rsi_agent_session_protocol::MessageFeedbackChange,
    ) -> TurnResult<rsi_agent_session_protocol::MessageFeedbackOutcome> {
        use rsi_agent_session_protocol::{
            FeedbackDecision, MessageFeedbackOutcome, SavedMessageFeedback,
        };
        let admission = self.inner.submission_admission.acquire(id).await?;
        if change.value.is_some() {
            // The transcript can publish a settled Fact before its writer finishes.
            let pending = lock_state(&self.inner)
                .sessions
                .get(id)
                .is_some_and(|s| change.message_seq.get() > s.durable_seq && !s.pending.is_empty());
            if pending {
                self.flush_resident_admitted(id).await?;
            }
        }
        let snapshot = self
            .inner
            .store
            .read_message_feedback(id, change.message_seq)
            .await
            .map_err(turn_store_error)?;
        let current = snapshot.current;
        match change.decide(current.as_ref()) {
            FeedbackDecision::Conflict => return Ok(MessageFeedbackOutcome::Conflict { current }),
            FeedbackDecision::Unchanged => return Ok(MessageFeedbackOutcome::Saved { current }),
            FeedbackDecision::Append => {}
        }
        let value = change.value.clone();
        let version = self
            .append_session_control(
                admission,
                snapshot.watermarks,
                AgentControlRecordBody::MessageFeedbackChanged { change },
            )
            .await?;
        Ok(MessageFeedbackOutcome::Saved {
            current: value.map(|value| SavedMessageFeedback { version, value }),
        })
    }

    async fn begin_maintenance(
        &self,
        id: &SessionId,
    ) -> TurnResult<Box<dyn rsi_agent_turn_protocol::SessionMaintenance>> {
        maintenance::begin_callback(self, id).await
    }
    fn maintenance_cancellation(&self, id: &SessionId) -> Option<CancellationToken> {
        self.inner
            .submission_admission
            .with_callback(id, |callback| callback.stop.clone())
    }
    async fn reserve_manual_compaction(
        &self,
        id: &SessionId,
        command: &rsi_agent_session_protocol::DomainRequestId,
    ) -> TurnResult<rsi_agent_turn_protocol::ManualCompactionAdmission> {
        manual_compaction::reserve(self, id, command).await
    }
    async fn generation_status(
        &self,
        session: &SessionId,
    ) -> TurnResult<Option<rsi_agent_turn_protocol::SessionGenerationStatus>> {
        Ok(lock_state(&self.inner)
            .sessions
            .get(session)
            .map(|session| rsi_agent_turn_protocol::SessionGenerationStatus {
                selection: session.composition.selection().clone(),
                rejection: session.generation_rejection.clone(),
                rebuilding: session.generation_rebuilding,
            }))
    }
    async fn wait_idle(&self, session_id: &SessionId) -> TurnResult<()> {
        let mut changes = self.inner.session_changes.session(session_id);
        loop {
            changes.mark_seen();
            let finished = {
                let _admission = self.inner.submission_admission.acquire(session_id).await?;
                let finished = self
                    .inner
                    .submission_admission
                    .with_callback(session_id, |callback| callback.finished.clone());
                if finished.is_none() && maintenance::driver_idle(self, session_id).await? {
                    return Ok(());
                }
                finished
            };
            tokio::select! {
                () = self.inner.submission_admission.closed.cancelled() => return Err(TurnError::ShuttingDown),
                () = async {
                    if let Some(done) = finished { done.cancelled().await } else { changes.changed().await }
                } => {},
            }
        }
    }
    async fn prepare_fork(
        &self,
        request: rsi_agent_turn_protocol::PrepareFork,
    ) -> TurnResult<PreparedFreshSession> {
        let cancelled = request.cancellation.clone();
        tokio::select! {
            biased;
            () = cancelled.cancelled() => Err(TurnError::ExecutionUnavailable),
            () = self.inner.stop_worker.cancelled() => Err(TurnError::ShuttingDown),
            result = self.prepare_independent_fork(request) => result,
        }
    }
    fn prepare_retirement(
        &self,
        sessions: &[SessionId],
    ) -> TurnResult<Box<dyn rsi_agent_turn_protocol::SessionRetirement>> {
        maintenance::reserve(self, sessions)
    }

    async fn mutate_queue(
        &self,
        session: &SessionId,
        request: rsi_agent_session_protocol::QueueMutationRequest,
        execution: Option<rsi_execution::ExecutionLease>,
    ) -> TurnResult<rsi_agent_session_protocol::QueueMutationReceipt> {
        self.mutate_queue_owned(session, request, execution).await
    }
    async fn queue_mutation_status(
        &self,
        session: &SessionId,
        operation: &rsi_agent_session_protocol::QueueOperationId,
    ) -> TurnResult<Option<rsi_agent_session_protocol::QueueMutationReceipt>> {
        self.read_queue_receipt(session, operation).await
    }
    fn controlled_work(
        &self,
        session: &SessionId,
        turn: &TurnId,
    ) -> TurnResult<Option<rsi_agent_turn_protocol::ControlledWork>> {
        let state = lock_state(&self.inner);
        Ok(state.controlled_work.get(session, turn))
    }
    fn settlement_health(&self) -> SettlementHealth {
        self.inner
            .settlement_health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn ready_health(&self) -> rsi_agent_turn_protocol::ReadySchedulerHealth {
        self.inner
            .ready_activation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .health
            .clone()
    }

    async fn prepare_resume(&self, session_id: &SessionId) -> TurnResult<PreparedResumeSession> {
        self.prepare_resume_session(session_id).await
    }

    async fn tree_sessions(&self, session_id: &SessionId) -> TurnResult<Vec<SessionId>> {
        let header = read_validated_header_bounded(&self.inner, session_id)
            .await
            .map_err(turn_store_error)?;
        let root_session_id = header.fork_origin().map_or_else(
            || header.session_id().clone(),
            |origin| origin.root_session_id.clone(),
        );
        let mut sessions = vec![root_session_id.clone()];
        sessions.extend(
            list_agent_descendants(&self.inner.store, &root_session_id)
                .await?
                .into_iter()
                .map(|(_, child)| child.session_id),
        );
        Ok(sessions)
    }

    #[allow(clippy::too_many_lines)] // Spawn validates and commits one indivisible child Header, lineage, and first mailbox message.
    async fn prepare_program(
        &self,
        request: rsi_agent_turn_protocol::PrepareProgram,
    ) -> TurnResult<Arc<dyn rsi_agent_turn_protocol::ProgramRun>> {
        self.prepare_program_run(request).await
    }
    async fn read_program(
        &self,
        caller: &AgentCallerAuthority,
        run: &rsi_agent_session_protocol::ProgramRunId,
    ) -> TurnResult<rsi_agent_turn_protocol::ProgramSnapshot> {
        self.read_program_snapshot(caller, run).await
    }
    async fn cancel_program(
        &self,
        caller: &AgentCallerAuthority,
        run: &rsi_agent_session_protocol::ProgramRunId,
    ) -> TurnResult<()> {
        self.cancel_program_owned(caller, run).await
    }
    async fn list_session_programs(
        &self,
        session: &SessionId,
        seed: u64,
        before: Option<u64>,
        limit: usize,
    ) -> TurnResult<rsi_agent_turn_protocol::ProgramHistoryPage> {
        self.session_programs(session, seed, before, limit).await
    }
    async fn read_session_program(
        &self,
        session: &SessionId,
        run: &rsi_agent_session_protocol::ProgramRunId,
        request: rsi_agent_turn_protocol::ProgramRead,
    ) -> TurnResult<rsi_agent_turn_protocol::ProgramDetails> {
        self.session_program(session, run, request).await
    }
    async fn cancel_session_program(
        &self,
        session: &SessionId,
        run: &rsi_agent_session_protocol::ProgramRunId,
    ) -> TurnResult<rsi_agent_turn_protocol::ProgramCancelReceipt> {
        self.cancel_session_run(session, run).await
    }
    async fn spawn_agent(&self, request: SpawnAgentRequest) -> TurnResult<SpawnedAgent> {
        let cancellation = request.cancellation.clone();
        if cancellation.is_cancelled() {
            return Err(TurnError::Cancelled);
        }
        tokio::select! {
            biased;
            result = self.spawn_agent_prepared(request) => result,
            () = cancellation.cancelled() => Err(TurnError::Cancelled),
        }
    }

    async fn read_agent_result(
        &self,
        caller: &AgentCallerAuthority,
        locator: &rsi_agent_session_protocol::AgentResultLocator,
    ) -> TurnResult<rsi_agent_turn_protocol::AgentResult> {
        self.read_structured_result(caller, locator).await
    }

    async fn send_agent_message(&self, request: SendAgentMessage) -> TurnResult<MessageReceipt> {
        let cancellation = request.cancellation.clone();
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(TurnError::Cancelled),
            result = self.send_agent_message_prepared(request) => result,
        }
    }

    async fn list_agents(
        &self,
        caller: &AgentCallerAuthority,
        scope: AgentListScope,
    ) -> TurnResult<Vec<AgentNode>> {
        let _execution = self.admit_agent_read(caller)?;
        let snapshot = self
            .inner
            .store
            .read_agent_subtree_snapshot(caller.session_id())
            .await
            .map_err(turn_store_error)?;
        snapshot.validate().map_err(turn_store_error)?;
        let mut nodes = snapshot
            .descendants
            .into_iter()
            .filter(|child| {
                scope == AgentListScope::Descendants
                    || &child.parent_session_id == caller.session_id()
            })
            .map(|child| AgentNode {
                state: if child.status.has_open_turn {
                    AgentNodeState::Running
                } else if child.status.has_waking_message {
                    AgentNodeState::Ready
                } else {
                    AgentNodeState::Idle
                },
                session_id: child.status.session_id,
                parent_session_id: child.parent_session_id,
                path: child.path,
                task_name: child.task_name,
            })
            .collect::<Vec<_>>();
        nodes.sort_by(|left, right| left.path.cmp(&right.path));
        let _execution = self.admit_agent_read(caller)?;
        Ok(nodes)
    }

    #[allow(clippy::too_many_lines)] // Wait persists one park/resume race with explicit message, completion, timeout, and cancel winners.
    async fn wait_agent(
        &self,
        caller: &AgentCallerAuthority,
        timeout: Duration,
        cancellation: CancellationToken,
    ) -> TurnResult<AgentWaitResult> {
        if timeout < Duration::from_millis(1) || timeout > Duration::from_hours(1) {
            return Err(TurnError::Invalid(
                "Agent wait timeout must be within 1ms..=1h".into(),
            ));
        }
        let deadline = Instant::now() + timeout;
        let execution = self.admit_agent_read(caller)?;
        let root = caller
            .header()
            .fork_origin()
            .map_or(caller.session_id(), |origin| &origin.root_session_id);
        let mut watches = vec![
            self.inner.session_changes.session(caller.session_id()),
            self.inner.session_changes.tree(root),
        ];
        let baseline = self
            .inner
            .store
            .read_agent_subtree_snapshot(caller.session_id())
            .await
            .map_err(turn_store_error)?;
        baseline.validate().map_err(turn_store_error)?;
        watches.extend(
            baseline
                .descendants
                .iter()
                .map(|child| self.inner.session_changes.session(&child.status.session_id)),
        );
        drop(execution);
        if baseline.descendants.is_empty()
            || baseline
                .descendants
                .iter()
                .all(|child| !child.status.has_open_turn && !child.status.has_waking_message)
        {
            let result = match tokio::time::timeout_at(
                deadline,
                observe_agent_wait_change(self, caller, &baseline),
            )
            .await
            {
                Ok(Ok(Some(_))) => Ok(AgentWaitResult::Changed),
                Ok(Ok(None)) => Ok(AgentWaitResult::NoProgress),
                Ok(Err(error)) => Err(error),
                Err(_) => Ok(AgentWaitResult::TimedOut),
            };
            let _execution = self.admit_agent_read(caller)?;
            return result;
        }
        let timeout_ms = u64::try_from(timeout.as_millis())
            .map_err(|_| TurnError::Invalid("Agent wait timeout exceeds its bound".into()))?;
        let deadline_ms = self
            .inner
            .clock
            .now_ms()
            .max(1)
            .checked_add(timeout_ms)
            .ok_or_else(|| TurnError::Invalid("Agent wait deadline overflowed".into()))?;
        let parked = self.park_agent_wait(caller, deadline_ms).await?;
        let (result, cause) = 'waiting: loop {
            // Mark before reading: commits during the observation remain pending.
            for watch in &mut watches {
                watch.mark_seen();
            }
            let observation = tokio::select! {
                biased;
                () = cancellation.cancelled() => break 'waiting (Err(TurnError::Cancelled), WaitResumeCause::Cancel),
                () = parked.interrupted() => break 'waiting (Err(TurnError::Cancelled), WaitResumeCause::Cancel),
                result = tokio::time::timeout_at(deadline, observe_agent_wait_change(self, caller, &baseline)) => result,
            };
            match observation {
                Ok(Ok(Some(cause))) => {
                    break 'waiting (Ok(AgentWaitResult::Changed), cause);
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => break 'waiting (Err(error), WaitResumeCause::Cancel),
                Err(_) => {
                    break 'waiting (Ok(AgentWaitResult::TimedOut), WaitResumeCause::Timeout);
                }
            }
            tokio::select! {
                () = cancellation.cancelled() => {
                    break 'waiting (Err(TurnError::Cancelled), WaitResumeCause::Cancel);
                }
                () = parked.interrupted() => {
                    break 'waiting (Err(TurnError::Cancelled), WaitResumeCause::Cancel);
                }
                _ = futures_util::future::select_all(watches.iter_mut().map(|watch| Box::pin(watch.changed()))) => {}
                () = tokio::time::sleep(DURABLE_OBSERVER_FALLBACK_INTERVAL) => {}
                () = tokio::time::sleep_until(deadline) => {
                    break 'waiting (Ok(AgentWaitResult::TimedOut), WaitResumeCause::Timeout);
                }
            }
        };
        parked.finish(cause, cancellation).await?;
        result
    }

    async fn interrupt_agent(
        &self,
        caller: &AgentCallerAuthority,
        target_session_id: &SessionId,
        cancellation: CancellationToken,
    ) -> TurnResult<CancelResult> {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(TurnError::Cancelled),
            result = self.interrupt_agent_prepared(caller, target_session_id, cancellation.clone()) => result,
        }
    }

    async fn submit_message(&self, request: SubmitMessage) -> TurnResult<MessageReceipt> {
        let first_task = match &request.session {
            SubmitSession::Fresh(prepared) => prepared.first_command_task(),
            SubmitSession::Resume(_) => None,
        };
        if let Some(task) = first_task {
            if task != &request.message || request.delivery != MessageDelivery::NextTurn {
                return Err(TurnError::Invalid(
                    "first turn must admit the command's exact task".into(),
                ));
            }
        } else if matches!(
            request.message.source,
            AgentMessageSource::Continuation { .. }
                | AgentMessageSource::Program { .. }
                | AgentMessageSource::Command { .. }
        ) {
            return Err(TurnError::Invalid(
                "owned input requires its dedicated admission service".into(),
            ));
        }
        self.submit_message_authorized(request, None).await
    }

    async fn message_status(
        &self,
        session_id: &SessionId,
        message_id: &MessageId,
    ) -> TurnResult<MessageReceipt> {
        let scan = read_mailbox_selection(
            &self.inner,
            session_id,
            MailboxSelection::Exact(message_id.clone()),
        )
        .await?;
        scan.selected
            .map(durable_message_entry)
            .map(|entry| message_receipt(session_id, scan.durable_fact_seq, &entry))
            .ok_or_else(|| TurnError::MessageNotFound {
                session: session_id.to_string(),
                message: message_id.to_string(),
            })
    }

    async fn claim_message(&self, request: ClaimMessage) -> TurnResult<SubmittedTurn> {
        self.claim_message_with_lane(request, None, None).await
    }

    fn watch_tree_membership(
        &self,
        root: &SessionId,
    ) -> TurnResult<rsi_agent_turn_protocol::TreeMembershipChanges> {
        let observer = ObserverLease::acquire(&self.inner, ObserverKind::Tree)?;
        let watch = self.inner.session_changes.tree(root);
        let inner = Arc::downgrade(&self.inner);
        Ok(stream::unfold(
            (watch, inner, observer),
            |(mut watch, owner, observer)| async move {
                let inner = owner.upgrade()?;
                tokio::select! {
                    () = inner.stop_worker.cancelled() => None,
                    () = watch.changed() => Some(((), (watch, owner, observer))),
                }
            },
        )
        .boxed())
    }

    async fn observe_models(
        &self,
        session_id: &SessionId,
    ) -> TurnResult<rsi_agent_turn_protocol::ModelLiveStream> {
        model_live::observe(&self.inner, session_id)
    }

    async fn observe_session(
        &self,
        session_id: &SessionId,
        cursor: ObservationCursor,
    ) -> TurnResult<SessionObservationStream> {
        let observer_lease = ObserverLease::acquire(&self.inner, ObserverKind::Session)?;
        let mut watch = self.inner.session_changes.session(session_id);
        watch.mark_seen();
        let mut state = DurableObservationState {
            inner: Arc::downgrade(&self.inner),
            session_id: session_id.clone(),
            control_seq: cursor.control_seq,
            fact_seq: cursor.fact_seq,
            pending: VecDeque::new(),
            watch,
            next_page: ObservationPageKind::Control,
            read_controls: true,
            read_facts: true,
            stopped: false,
            _observer_lease: observer_lease,
        };
        fill_observation_page(&self.inner, &mut state).await?;
        Ok(stream::unfold(state, durable_observation_next).boxed())
    }

    async fn cancel_target(
        &self,
        session_id: &SessionId,
        target: CancelTarget,
        reason: Option<CancelReason>,
    ) -> TurnResult<CancelResult> {
        let message_id = match target {
            CancelTarget::Message(message_id) => message_id,
            CancelTarget::Turn(turn_id) => return self.cancel(session_id, &turn_id, reason).await,
            CancelTarget::Inbox => {
                return self
                    .cancel_inbox(session_id, inbox::InboxCancel::Clear, reason)
                    .await;
            }
            CancelTarget::Session { keep_inbox } => {
                return self
                    .cancel_inbox(session_id, inbox::InboxCancel::Stop { keep_inbox }, reason)
                    .await;
            }
        };
        if reason.is_some() {
            return Err(TurnError::Invalid(
                "unclaimed-message cancellation does not accept a free-form reason".into(),
            ));
        }
        let header = read_validated_header_bounded(&self.inner, session_id)
            .await
            .map_err(turn_store_error)?;
        if let Some(result) = self
            .cancel_program_message(
                &header,
                &message_id,
                rsi_agent_session_protocol::ProgramOutcome::Cancelled,
            )
            .await?
        {
            return Ok(result);
        }
        let admission = self.inner.submission_admission.acquire(session_id).await?;
        self.fence_pending_terminal(session_id).await?;
        let scan = read_mailbox_selection(
            &self.inner,
            session_id,
            MailboxSelection::Exact(message_id.clone()),
        )
        .await?;
        let entry = scan.selected.map(durable_message_entry).ok_or_else(|| {
            TurnError::Invalid(format!(
                "message `{}` does not exist in session `{}`",
                message_id.as_str(),
                session_id.as_str()
            ))
        })?;
        match entry.state {
            MessageState::Discarded { .. } => {
                return Ok(CancelResult {
                    accepted: false,
                    already_terminal: true,
                });
            }
            MessageState::Claimed { turn_id, .. } => {
                drop(admission);
                return self.cancel(session_id, &turn_id, None).await;
            }
            MessageState::Pending => {}
        }
        let expected_fact_seq = scan.durable_fact_seq;
        let control = AgentControlRecord::new(
            scan.durable_control_seq
                .checked_add(1)
                .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?,
            self.inner.clock.now_ms().max(1),
            AgentControlRecordBody::MessageDiscarded {
                message_id,
                reason: MessageDiscardReason::Cancelled,
            },
        )
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        self.commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session_id.clone(),
                expected_fact_seq,
                expected_control_seq: scan.durable_control_seq,
                header: None,
                facts: Vec::new(),
                controls: vec![control],
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: None,
        })
        .await?
        .map_err(turn_store_error)?;
        Ok(CancelResult {
            accepted: true,
            already_terminal: false,
        })
    }

    async fn submit(&self, request: SubmitTurn) -> TurnResult<SubmittedTurn> {
        let SubmitTurn {
            session,
            turn_id,
            text,
            model,
            reasoning_effort,
            sandbox,
        } = request;
        if text.is_empty() || text.len() > MAXIMUM_TURN_TEXT_BYTES {
            return Err(TurnError::Invalid(format!(
                "turn text must contain 1..={MAXIMUM_TURN_TEXT_BYTES} UTF-8 bytes"
            )));
        }
        if let Some(model) = &model {
            model
                .validate()
                .map_err(|error| TurnError::Invalid(error.to_string()))?;
        }
        let header = session.header();
        let profile = header.settings();
        let sandbox = self.session_sandbox(&session, sandbox).await?;
        let require_approval = profile.require_approval();
        let body = SessionFactBody::TurnAccepted {
            turn_id: turn_id.clone(),
            text,
            model,
            reasoning_effort,
            sandbox,
            require_approval,
        };
        self.submit_body(session, turn_id, body).await
    }

    async fn submit_image(&self, request: SubmitImage) -> TurnResult<SubmittedTurn> {
        let SubmitImage {
            session,
            turn_id,
            model,
            request,
        } = request;
        model
            .validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        request
            .validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let body = SessionFactBody::ImageRequested {
            turn_id: turn_id.clone(),
            model,
            request,
        };
        self.submit_body(session, turn_id, body).await
    }

    async fn cancel(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        reason: Option<CancelReason>,
    ) -> TurnResult<CancelResult> {
        let body = SessionFactBody::CancelRequested {
            turn_id: turn_id.clone(),
            reason,
        };
        body.validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let mut submission_admission = self.inner.submission_admission.acquire(session_id).await?;
        if !lock_state(&self.inner).sessions.contains_key(session_id) {
            if read_stored_outcome(&self.inner, session_id, turn_id)
                .await?
                .is_some()
            {
                return Ok(CancelResult {
                    accepted: false,
                    already_terminal: true,
                });
            }
            submission_admission = self
                .ensure_session_loaded(session_id, submission_admission)
                .await?;
        }
        let live = lock_state(&self.inner)
            .sessions
            .get(session_id)
            .is_some_and(|session| session.turns.contains_key(turn_id));
        if !live {
            if lock_state(&self.inner)
                .sessions
                .get(session_id)
                .is_some_and(|session| session.durable_seq == 0)
            {
                return Err(turn_not_found(session_id, turn_id));
            }
            return match read_stored_outcome(&self.inner, session_id, turn_id).await? {
                Some(_) => Ok(CancelResult {
                    accepted: false,
                    already_terminal: true,
                }),
                None => Err(TurnError::Invariant(
                    "nonterminal durable turn is absent from live control state".into(),
                )),
            };
        }
        let (wait, cancellation) = {
            let mut state = lock_state(&self.inner);
            let session = state
                .sessions
                .get_mut(session_id)
                .ok_or_else(|| TurnError::SessionNotFound(session_id.to_string()))?;
            let turn = session
                .turns
                .get(turn_id)
                .ok_or_else(|| TurnError::TurnNotFound {
                    session: session_id.to_string(),
                    turn: turn_id.to_string(),
                })?;
            if turn.terminal.is_some() {
                return Ok(CancelResult {
                    accepted: false,
                    already_terminal: true,
                });
            }
            if turn.cancel_requested {
                return Ok(CancelResult {
                    accepted: false,
                    already_terminal: false,
                });
            }
            let cancellation = turn.cancellation.clone();
            let fact = next_fact(&self.inner, session, body).map_err(turn_kernel_error)?;
            let cancel_seq = fact.seq();
            push_pending(&self.inner, session, fact.clone()).map_err(turn_kernel_error)?;
            session
                .turns
                .get_mut(turn_id)
                .expect("validated turn exists")
                .cancel_requested = true;
            publish_live_watermarks(session);
            (DurabilityWait::new(session, cancel_seq), cancellation)
        };
        drop(submission_admission);
        self.wait_for_durable(wait)
            .await
            .map_err(turn_kernel_error)?;
        cancellation.cancel();
        Ok(CancelResult {
            accepted: true,
            already_terminal: false,
        })
    }

    async fn observe(&self, session_id: &SessionId, after_seq: u64) -> TurnResult<TurnObservation> {
        let observer_lease = ObserverLease::acquire(&self.inner, ObserverKind::Turn)?;
        let live_snapshot = {
            let state = lock_state(&self.inner);
            state.sessions.get(session_id).map(|session| {
                (
                    session.updates.subscribe(),
                    Some(session.flush_status.subscribe()),
                    session.durable_seq,
                    session.live_seq(),
                )
            })
        };
        let (receiver, flush_status, durable_target, live_seq, durable_facts) =
            if let Some((receiver, flush_status, durable_target, live_seq)) = live_snapshot {
                (
                    receiver,
                    flush_status,
                    durable_target,
                    live_seq.map_err(turn_kernel_error)?,
                    VecDeque::new(),
                )
            } else {
                let (facts, page_durable_seq) =
                    read_observed_facts(&self.inner, session_id, after_seq).await?;
                let durable_facts = facts.into();
                let state = lock_state(&self.inner);
                if let Some(session) = state.sessions.get(session_id) {
                    (
                        session.updates.subscribe(),
                        Some(session.flush_status.subscribe()),
                        session.durable_seq.max(page_durable_seq),
                        session
                            .live_seq()
                            .map_err(turn_kernel_error)?
                            .max(page_durable_seq),
                        durable_facts,
                    )
                } else {
                    let (sender, receiver) = watch::channel(LiveWatermarks {
                        live_seq: page_durable_seq,
                        durable_seq: page_durable_seq,
                    });
                    drop(sender);
                    (
                        receiver,
                        None,
                        page_durable_seq,
                        page_durable_seq,
                        durable_facts,
                    )
                }
            };
        if after_seq > live_seq {
            return Err(TurnError::Invalid(
                "observation cursor exceeds the live tail".into(),
            ));
        }
        let state = ObservationState {
            session_id: session_id.clone(),
            cursor: after_seq,
            durable_target,
            live_target: live_seq,
            inner: Arc::downgrade(&self.inner),
            receiver,
            flush_status,
            durable_facts,
            ended: false,
            _observer_lease: observer_lease,
        };
        Ok(stream::unfold(state, observation_next).boxed())
    }

    async fn outcome(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> TurnResult<Option<TurnOutcome>> {
        {
            let state = lock_state(&self.inner);
            if let Some(session) = state
                .sessions
                .get(session_id)
                .filter(|session| session.permanent_flush_error.is_none())
            {
                if let Some(turn) = session.turns.get(turn_id) {
                    return Ok(turn
                        .terminal_seq
                        .filter(|terminal_seq| *terminal_seq <= session.durable_seq)
                        .and_then(|_| turn.terminal.clone()));
                }
                if session.durable_seq == 0 {
                    return Err(turn_not_found(session_id, turn_id));
                }
            }
        }
        read_stored_outcome(&self.inner, session_id, turn_id).await
    }

    async fn session_header(&self, session_id: &SessionId) -> TurnResult<SessionHeader> {
        if let Some(header) = lock_state(&self.inner)
            .sessions
            .get(session_id)
            .map(|session| session.header.as_ref().clone())
        {
            return Ok(header);
        }
        read_header_bounded(&self.inner, session_id)
            .await
            .map_err(turn_store_error)
    }
    async fn domain_states(
        &self,
        session_id: &SessionId,
    ) -> TurnResult<Vec<rsi_agent_session_protocol::DomainStateView>> {
        let page = observation::read_domain_states_bounded(&self.inner, session_id, None)
            .await
            .map_err(turn_store_error)?;
        Ok(page
            .states
            .into_iter()
            .map(|state| rsi_agent_session_protocol::DomainStateView {
                revision: state.head.revision,
                snapshot: state.snapshot,
            })
            .collect())
    }
    async fn domain_state(
        &self,
        session_id: &SessionId,
        domain: &str,
    ) -> TurnResult<Option<(u64, rsi_agent_session_protocol::DomainStateView)>> {
        let page = observation::read_domain_state_page_bounded(
            &self.inner,
            session_id,
            None,
            Some(domain),
        )
        .await
        .map_err(turn_store_error)?;
        Ok(page.states.into_iter().next().map(|state| {
            (
                page.durable_control_seq,
                rsi_agent_session_protocol::DomainStateView {
                    revision: state.head.revision,
                    snapshot: state.snapshot,
                },
            )
        }))
    }
    async fn domain_request(
        &self,
        session_id: &SessionId,
        request_id: &rsi_agent_session_protocol::DomainRequestId,
    ) -> TurnResult<Option<rsi_agent_turn_protocol::DomainMutationReceipt>> {
        observation::read_domain_request_bounded(&self.inner, session_id, request_id).await
    }
}

pub(super) async fn append_retained_wait_control(
    kernel: &AgentKernel,
    caller: &AgentCallerAuthority,
    lease: &mutation::AgentMutationLease,
    activation_id: &rsi_agent_session_protocol::ActivationId,
    body: AgentControlRecordBody,
) -> std::result::Result<(), super::human_wait::WaitControlError> {
    lease.validate(kernel, caller)?;
    let _admission = kernel
        .inner
        .submission_admission
        .acquire_retained(caller.session_id(), lease)
        .await?;
    lease.validate(kernel, caller)?;
    if matches!(&body, AgentControlRecordBody::WaitResumed { .. }) {
        let active = kernel
            .inner
            .store
            .active_activation(caller.session_id())
            .await?
            .ok_or_else(|| TurnError::Invariant("retained wait lost its activation".into()))?;
        if active.activation_id != *activation_id
            || active.turn_id.as_ref() != Some(caller.turn_id())
        {
            return Err(TurnError::Invariant("retained wait activation changed".into()).into());
        }
        match active.phase {
            // A prior commit can succeed even if its acknowledgement was lost.
            StoreActivationPhase::Running => return Ok(()),
            StoreActivationPhase::Parked => (),
            StoreActivationPhase::WaitingForDescendants => {
                return Err(
                    TurnError::Invariant("retained wait activation already ended".into()).into(),
                );
            }
        }
    }
    commit_wait_control(kernel, caller, activation_id, body).await
}

async fn commit_wait_control(
    kernel: &AgentKernel,
    caller: &AgentCallerAuthority,
    activation_id: &rsi_agent_session_protocol::ActivationId,
    body: AgentControlRecordBody,
) -> std::result::Result<(), super::human_wait::WaitControlError> {
    let session_id = caller.session_id().clone();
    kernel.fence_pending_terminal(&session_id).await?;
    let watermarks = kernel.inner.store.read_watermarks(&session_id).await?;
    let expected_fact_seq = watermarks.durable_fact_seq;
    let expected_control_seq = watermarks.durable_control_seq;
    let control = AgentControlRecord::new(
        expected_control_seq
            .checked_add(1)
            .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?,
        kernel.inner.clock.now_ms().max(1),
        body,
    )
    .map_err(|error| TurnError::Invalid(error.to_string()))?;
    kernel
        .commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id,
                expected_fact_seq,
                expected_control_seq,
                header: None,
                facts: Vec::new(),
                controls: vec![control],
            }],
            required_active_activations: vec![AgentActivationGuard {
                session_id: caller.session_id().clone(),
                activation_id: activation_id.clone(),
            }],
            quiescent_descendants_of: None,
        })
        .await??;
    Ok(())
}

impl AgentKernel {
    async fn submit_message_authorized(
        &self,
        request: SubmitMessage,
        source: Option<(&AgentCallerAuthority, &CancellationToken)>,
    ) -> TurnResult<MessageReceipt> {
        request
            .message
            .validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let admission = self
            .inner
            .submission_admission
            .acquire(request.session.session_id())
            .await?;
        let owner = match source {
            Some((caller, cancellation)) => MessageAdmission::Agent(caller, cancellation),
            None => MessageAdmission::Session,
        };
        self.submit_message_admitted(request, owner, admission)
            .await
    }

    #[allow(clippy::too_many_lines)] // Admission commits the validated Header, mailbox payload, and ready index as one operation.
    pub(super) async fn submit_message_admitted(
        &self,
        request: SubmitMessage,
        owner: MessageAdmission<'_>,
        admission: SubmissionAdmissionLease,
    ) -> TurnResult<MessageReceipt> {
        let source = match owner {
            MessageAdmission::Agent(caller, cancellation) => Some((caller, cancellation)),
            _ => None,
        };
        let execution = request
            .session
            .execution()
            .cloned()
            .or_else(|| source.and_then(|(caller, _)| caller.execution().cloned()));
        let execution_admission =
            execution_admission::admit(request.session.header(), execution.as_ref())?;
        if let Some(mode) = request.message.options.sandbox {
            delegated_sandbox(request.session.header(), mode)?;
        }
        let session_id = request.session.session_id().clone();
        let requested_header = request.session.header().clone();
        let root_session_id = agent_root_and_path(&requested_header).0;
        if let SubmitSession::Resume(prepared) = &request.session {
            self.inner.resume_issuer.inspect(prepared)?;
        }
        if let Some(error) = lock_state(&self.inner)
            .sessions
            .get(&session_id)
            .and_then(|session| session.permanent_flush_error.clone())
        {
            return Err(TurnError::Flush(error));
        }

        self.fence_pending_terminal(&session_id).await?;
        let observed_execution = self.inner.execution_messages.snapshot(&session_id);
        let durable_header = match read_validated_header_bounded(&self.inner, &session_id).await {
            Ok(header) => Some(header),
            Err(StoreError::NotFound(_)) if matches!(&request.session, SubmitSession::Fresh(_)) => {
                None
            }
            Err(error) => return Err(turn_store_error(error)),
        };
        let fresh_reservation =
            if matches!(&request.session, SubmitSession::Fresh(_)) && durable_header.is_none() {
                Some(self.reserve_fresh_session(&requested_header, true).await?)
            } else {
                None
            };
        if durable_header
            .as_ref()
            .is_some_and(|header| header != &requested_header)
        {
            return Err(TurnError::Invalid(
                "message submission Header disagrees with the durable session".into(),
            ));
        }

        let scan = if durable_header.is_some() {
            read_mailbox_selection(
                &self.inner,
                &session_id,
                MailboxSelection::Submit(request.message.message_id.clone()),
            )
            .await?
        } else {
            rsi_agent_store_protocol::StoreMailboxSelection::default()
        };
        self.inner.execution_messages.retain_pending(
            &session_id,
            &scan.pending,
            observed_execution,
        );
        if let Some(entry) = scan.selected.map(durable_message_entry) {
            if let SubmitSession::Fresh(prepared) = &request.session {
                self.validate_fresh_baseline(prepared).await?;
            }
            if entry.message != request.message
                || entry.root_session_id != root_session_id
                || entry.delivery != request.delivery
            {
                return Err(TurnError::MessageConflict {
                    session: session_id.to_string(),
                    message: request.message.message_id.to_string(),
                });
            }
            if entry.state == MessageState::Pending {
                let reservation = self.inner.execution_messages.reserve(
                    &session_id,
                    &entry.message.message_id,
                    execution.as_ref(),
                )?;
                self.inner.execution_messages.publish(reservation);
                self.request_ready_scan();
            }
            return Ok(message_receipt(&session_id, scan.durable_fact_seq, &entry));
        }
        if matches!(&request.session, SubmitSession::Fresh(_)) && durable_header.is_some() {
            return Err(TurnError::Invalid(
                "fresh message submission selected an existing session".into(),
            ));
        }
        let completion_reservations = self
            .inner
            .store
            .completion_reservation_count(&session_id)
            .await
            .map_err(turn_store_error)?;
        let expected_fact_seq = scan.durable_fact_seq;
        let bound_turn_id = if request.delivery == MessageDelivery::Steer {
            if !request.message.source.is_direct()
                || request.message.options != MessageOptions::default()
            {
                return Err(TurnError::Invalid(
                    "steering requires direct input without new-Turn options".into(),
                ));
            }
            let state = lock_state(&self.inner);
            state.sessions.get(&session_id).and_then(|session| {
                session.turns.iter().find_map(|(id, turn)| {
                    (turn.activation_id.is_some()
                        && turn.terminal.is_none()
                        && !turn.cancel_requested)
                        .then(|| id.clone())
                })
            })
        } else {
            None
        };
        let target = match request.delivery {
            MessageDelivery::NextStep => MessageTarget::NextStep,
            MessageDelivery::Steer if bound_turn_id.is_some() => MessageTarget::NextStep,
            MessageDelivery::NextTurn | MessageDelivery::Steer => MessageTarget::NextTurn,
        };
        let automatic = matches!(
            request.message.source,
            AgentMessageSource::Continuation { .. }
        );
        if automatic
            && scan.pending.iter().any(|entry| {
                entry.target == MessageTarget::NextTurn
                    && entry.source_kind != AgentMessageSourceKind::Continuation
            })
        {
            return Err(TurnError::ContinuationDisarmed);
        }
        let pending_count = scan.pending.len();
        let superseded = if !automatic
            && !matches!(owner, MessageAdmission::BackgroundJob)
            && target == MessageTarget::NextTurn
        {
            scan.pending
                .into_iter()
                .filter(|entry| entry.source_kind == AgentMessageSourceKind::Continuation)
                .map(|entry| entry.message_id)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if pending_count
            .saturating_sub(superseded.len())
            .saturating_add(completion_reservations)
            >= MAXIMUM_PENDING_AGENT_MESSAGES
        {
            return Err(TurnError::Capacity);
        }
        let baseline = match &request.session {
            SubmitSession::Fresh(prepared) => lifecycle::initial_controls(prepared)?,
            SubmitSession::Resume(_) => vec![],
        };
        let control_seq = scan
            .durable_control_seq
            .checked_add(
                1 + baseline.len() as u64
                    + u64::try_from(superseded.len()).map_err(|_| TurnError::Capacity)?,
            )
            .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?;
        let mut controls = baseline;
        for message_id in superseded {
            let seq = scan
                .durable_control_seq
                .checked_add(u64::try_from(controls.len()).map_err(|_| TurnError::Capacity)? + 1)
                .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?;
            controls.push(
                AgentControlRecord::new(
                    seq,
                    self.inner.clock.now_ms().max(1),
                    AgentControlRecordBody::MessageDiscarded {
                        message_id,
                        reason: MessageDiscardReason::Superseded,
                    },
                )
                .map_err(|error| TurnError::Invalid(error.to_string()))?,
            );
        }
        let control = AgentControlRecord::new(
            control_seq,
            self.inner.clock.now_ms().max(1),
            AgentControlRecordBody::MessageAccepted {
                message: request.message.clone(),
                delivery: request.delivery,
                bound_turn_id,
                root_session_id,
                target,
                wake_required: target == MessageTarget::NextTurn,
            },
        )
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        controls.push(control);
        if scan.inbox_paused && request.delivery != MessageDelivery::NextStep {
            inbox::append_resume(&mut controls, self.inner.clock.now_ms().max(1))?;
        }
        let (header, baseline_resources) = match request.session {
            SubmitSession::Fresh(prepared) => {
                let (header, _composition, baseline) = prepared.into_parts();
                (Some(header), Some(baseline))
            }
            SubmitSession::Resume(prepared) => {
                let _parts = self.inner.resume_issuer.consume(prepared)?;
                (None, None)
            }
        };
        let source_lease = source
            .map(|(caller, cancellation)| self.admit_agent_mutation(caller, cancellation))
            .transpose()?;
        let execution_reservation = self.inner.execution_messages.reserve(
            &session_id,
            &request.message.message_id,
            execution.as_ref(),
        )?;
        let kernel = self.clone();
        self.owned_commit(async move {
            let _admission = admission;
            let _baseline_resources = baseline_resources;
            let _execution_admission = execution_admission;
            let _source_lease = source_lease;
            let commit = kernel
                .commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
                    sessions: vec![AtomicSessionAppend {
                        session_id: session_id.clone(),
                        expected_fact_seq,
                        expected_control_seq: scan.durable_control_seq,
                        header,
                        facts: Vec::new(),
                        controls,
                    }],
                    required_active_activations: Vec::new(),
                    quiescent_descendants_of: None,
                })
                .await?
                .map_err(turn_store_error)?;
            let observed_fact_seq = commit
                .sessions
                .first()
                .filter(|watermark| watermark.session_id == session_id)
                .map(|watermark| watermark.durable_fact_seq)
                .ok_or_else(|| {
                    TurnError::Invariant("message commit returned no target watermark".into())
                })?;
            drop(fresh_reservation);
            kernel
                .inner
                .execution_messages
                .publish(execution_reservation);
            kernel.request_ready_scan();
            Ok(MessageReceipt {
                session_id,
                message_id: request.message.message_id,
                accepted_control_seq: control_seq,
                observed_fact_seq,
                state: MessageState::Pending,
            })
        })
        .await
    }
}

impl AgentKernel {
    #[allow(clippy::too_many_lines)] // Claim materializes Activation, Turn, Step, context, reservation, and indexes in one transaction.
    pub(super) async fn claim_message_with_lane(
        &self,
        request: ClaimMessage,
        lane: Option<Arc<TreeClaimLane>>,
        cancellation: Option<&CancellationToken>,
    ) -> TurnResult<SubmittedTurn> {
        let (header, _) = self.inner.resume_issuer.inspect(&request.session)?;
        let header = header.clone();
        let session_id = header.session_id().clone();
        let parent_session_id = header
            .fork_origin()
            .map(|origin| origin.parent.session_id().clone());
        let admissions = self
            .inner
            .submission_admission
            .acquire_pair(&session_id, header.completion_parent())
            .await?;
        if self.inner.submission_admission.defer_claim(&session_id) {
            return Err(TurnError::StaleClaim);
        }
        let execution = self
            .inner
            .execution_messages
            .get(&session_id, &request.message_id);
        let execution_admission = execution_admission::admit(&header, execution.as_ref())?;
        let (resume_admission, admissions) = self
            .reserve_resume_submission(&request.session, admissions)
            .await?;

        let wait = {
            let state = lock_state(&self.inner);
            let session = state
                .sessions
                .get(&session_id)
                .ok_or_else(|| TurnError::SessionNotFound(session_id.to_string()))?;
            DurabilityWait::new(session, session.live_seq().map_err(turn_kernel_error)?)
        };
        self.wait_for_durable(wait)
            .await
            .map_err(turn_kernel_error)?;

        let mut scan = read_mailbox_selection(
            &self.inner,
            &session_id,
            MailboxSelection::Wake(request.message_id.clone()),
        )
        .await?;
        let entry = scan
            .selected
            .take()
            .map(durable_message_entry)
            .ok_or_else(|| {
                TurnError::Invalid(format!(
                    "message `{}` does not exist in session `{}`",
                    request.message_id.as_str(),
                    session_id.as_str()
                ))
            })?;
        match &entry.state {
            MessageState::Claimed {
                activation_id,
                turn_id,
                step_id,
                claimed_fact_seq: _,
            } if activation_id == &request.activation_id
                && turn_id == &request.turn_id
                && step_id == &request.step_id =>
            {
                let accepted_seq =
                    read_turn_boundary_bounded(&self.inner, &session_id, &request.turn_id)
                        .await
                        .map_err(turn_store_error)?
                        .accepted_seq();
                drop(resume_admission);
                return Ok(SubmittedTurn {
                    session_id,
                    turn_id: turn_id.clone(),
                    accepted_seq,
                });
            }
            MessageState::Claimed { .. } => {
                return Err(TurnError::Invalid(
                    "message was already claimed by a different execution boundary".into(),
                ));
            }
            MessageState::Discarded { .. } => {
                return Err(TurnError::Invalid(
                    "discarded message cannot be claimed".into(),
                ));
            }
            MessageState::Pending => {}
        }
        {
            let state = lock_state(&self.inner);
            let session = state
                .sessions
                .get(&session_id)
                .ok_or_else(|| TurnError::SessionNotFound(session_id.to_string()))?;
            if !session.turns.is_empty() || !session.pending.is_empty() {
                return Err(TurnError::Invalid(
                    "next-Turn message cannot be claimed while a Turn is active".into(),
                ));
            }
        }

        if scan.inbox_paused {
            return Err(TurnError::StaleClaim);
        }
        if entry.target != MessageTarget::NextTurn {
            return Err(TurnError::Invalid(
                "next-Step message requires its owning active Turn".into(),
            ));
        }
        if let AgentMessageSource::Program { source } = &entry.message.source
            && !self
                .program_notice_claim_allowed(&session_id, source)
                .await?
        {
            self.discard_continuation_admitted(
                &session_id,
                (scan.durable_fact_seq, scan.durable_control_seq),
                &entry,
                MessageDiscardReason::ProgramInterrupted,
            )
            .await?;
            return Err(TurnError::StaleClaim);
        }
        if let AgentMessageSource::Continuation { source } = &entry.message.source {
            let ordinary_pending = scan.has_ordinary_pending_turn;
            let disposition = if ordinary_pending {
                Some(MessageDiscardReason::Superseded)
            } else {
                match self
                    .continuation_claim(
                        &session_id,
                        &request.message_id,
                        source,
                        Some(&request.session),
                    )
                    .await?
                {
                    continuation::ContinuationClaim::Ready => None,
                    continuation::ContinuationClaim::AwaitingOwner => {
                        return Err(TurnError::ContinuationDisarmed);
                    }
                    continuation::ContinuationClaim::Disarmed => {
                        Some(MessageDiscardReason::ContinuationDisarmed)
                    }
                }
            };
            if let Some(reason) = disposition {
                self.discard_continuation_admitted(
                    &session_id,
                    (scan.durable_fact_seq, scan.durable_control_seq),
                    &entry,
                    reason,
                )
                .await?;
                return Err(TurnError::ContinuationDisarmed);
            }
        }

        let (expected_fact_seq, composition) = {
            let state = lock_state(&self.inner);
            let session = state
                .sessions
                .get(&session_id)
                .ok_or_else(|| TurnError::SessionNotFound(session_id.to_string()))?;
            (session.durable_seq, session.composition.clone())
        };
        let model = entry.message.options.model.clone();
        let sandbox = SessionPermission::bounded_sandbox(
            &header,
            match entry.message.options.sandbox {
                Some(mode) => mode,
                None => self
                    .current_permission(&header, &composition)
                    .await?
                    .map_or(header.settings().sandbox(), |value| value.sandbox),
            },
        );
        let require_approval = header.settings().require_approval();
        let selected = {
            let mut selected = scan.pending;
            // Store owns the cardinality bound and FIFO cursors. Step input comes
            // first even when accepted after the waking message.
            selected.sort_unstable_by_key(|pending| {
                (
                    pending.target != MessageTarget::NextStep,
                    pending.accepted_control_seq,
                )
            });
            selected
        };
        let timestamp_ms = self.inner.clock.now_ms().max(1);
        let fact_bodies = vec![
            SessionFactBody::MessageTurnAccepted {
                turn_id: request.turn_id.clone(),
                activation_id: request.activation_id.clone(),
                message_ids: selected
                    .iter()
                    .map(|pending| pending.message_id.clone())
                    .collect(),
                reasoning_effort: entry.message.options.reasoning_effort.clone(),
                model,
                sandbox,
                require_approval,
            },
            SessionFactBody::StepStarted {
                turn_id: request.turn_id.clone(),
                step_id: request.step_id.clone(),
            },
        ];
        let facts = fact_bodies
            .into_iter()
            .enumerate()
            .map(|(offset, body)| {
                let offset = u64::try_from(offset)
                    .map_err(|_| TurnError::Invariant("Fact offset exceeds u64".into()))?;
                let seq = expected_fact_seq
                    .checked_add(offset + 1)
                    .ok_or_else(|| TurnError::Invariant("Fact sequence exhausted".into()))?;
                SessionFact::new(seq, timestamp_ms, body)
                    .map_err(|error| TurnError::Invalid(error.to_string()))
            })
            .collect::<TurnResult<Vec<_>>>()?;
        let claimed_fact_seq = expected_fact_seq
            .checked_add(3)
            .ok_or_else(|| TurnError::Invariant("message Fact sequence exhausted".into()))?;
        let final_fact_seq = expected_fact_seq
            .checked_add(2 + selected.len() as u64)
            .ok_or_else(|| TurnError::Invariant("message Fact sequence exhausted".into()))?;
        let program_append = self
            .program_child_claim_append(&header, &request.message_id, &request.activation_id)
            .await?;
        let program_sink = program_append.is_some();
        let mailbox_parent = header.completion_parent().filter(|_| !program_sink);
        let parent_activation = if let Some(parent_session_id) = mailbox_parent {
            self.fence_pending_terminal(parent_session_id).await?;
            let active = self
                .inner
                .store
                .active_activation(parent_session_id)
                .await
                .map_err(turn_store_error)?;
            if active.is_none()
                && !entry.message.source.is_direct()
                && !matches!(
                    entry.message.source,
                    AgentMessageSource::Continuation { .. }
                )
            {
                return Err(TurnError::Invalid(
                    "Agent-sourced child activation requires its parent activation to remain active".into(),
                ));
            }
            let pending = self
                .inner
                .store
                .read_agent_mailbox_summary(parent_session_id)
                .await
                .map_err(turn_store_error)?;
            let reservations = self
                .inner
                .store
                .completion_reservation_count(parent_session_id)
                .await
                .map_err(turn_store_error)?;
            if pending.pending_count.saturating_add(reservations) >= MAXIMUM_PENDING_AGENT_MESSAGES
            {
                return Err(TurnError::Capacity);
            }
            active
        } else {
            None
        };
        let mut controls = vec![AgentControlRecord::new(
            scan.durable_control_seq
                .checked_add(1)
                .ok_or(TurnError::Capacity)?,
            timestamp_ms,
            AgentControlRecordBody::ActivationStarted {
                activation_id: request.activation_id.clone(),
                root_session_id: entry.root_session_id,
                parent_session_id: parent_session_id.clone(),
                path: request.path,
            },
        )];
        for (index, pending) in selected.iter().enumerate() {
            controls.push(AgentControlRecord::new(
                scan.durable_control_seq
                    .checked_add(controls.len() as u64 + 1)
                    .ok_or(TurnError::Capacity)?,
                timestamp_ms,
                AgentControlRecordBody::MessageClaimed {
                    message_id: pending.message_id.clone(),
                    activation_id: request.activation_id.clone(),
                    turn_id: request.turn_id.clone(),
                    step_id: request.step_id.clone(),
                    claimed_fact_seq: claimed_fact_seq + index as u64,
                },
            ));
        }
        if let Some(parent_session_id) = mailbox_parent {
            controls.push(AgentControlRecord::new(
                scan.durable_control_seq
                    .checked_add(controls.len() as u64 + 1)
                    .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?,
                timestamp_ms,
                AgentControlRecordBody::CompletionReserved {
                    activation_id: request.activation_id.clone(),
                    parent_session_id: parent_session_id.clone(),
                    maximum_bytes: u64::try_from(
                        rsi_agent_session_protocol::MAXIMUM_AGENT_MESSAGE_BYTES,
                    )
                    .map_err(|_| {
                        TurnError::Invariant("completion byte bound exceeds u64".into())
                    })?,
                },
            ));
        }
        if program_sink {
            let Some(rsi_agent_session_protocol::ExecutionOwner::ProgramRun {
                run_id,
                ordinal,
                ..
            }) = header.execution_owner()
            else {
                unreachable!("program sink requires immutable owner")
            };
            controls.push(AgentControlRecord::new(
                scan.durable_control_seq
                    .checked_add(controls.len() as u64 + 1)
                    .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?,
                timestamp_ms,
                AgentControlRecordBody::ProgramCompletionReserved {
                    activation_id: request.activation_id.clone(),
                    run_id: run_id.clone(),
                    ordinal: *ordinal,
                },
            ));
        }
        let controls = controls
            .into_iter()
            .collect::<rsi_agent_session_protocol::Result<Vec<_>>>()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return Err(TurnError::Cancelled);
        }
        let facts = facts.into_iter().map(Arc::new).collect::<Vec<_>>();
        let _parts = self.inner.resume_issuer.consume(request.session)?;
        let mut sessions = vec![AtomicSessionAppend {
            session_id: session_id.clone(),
            expected_fact_seq,
            expected_control_seq: scan.durable_control_seq,
            header: None,
            facts: facts.clone(),
            controls,
        }];
        if let Some(parent) = program_append {
            sessions.push(parent);
        }
        let kernel = self.clone();
        self.owned_commit(async move {
            let _admissions = admissions;
            let _execution_admission = execution_admission;
            kernel
                .inner
                .commit_agent(AtomicAgentCommit {
                    sessions,
                    required_active_activations: parent_activation
                        .map(|activation| AgentActivationGuard {
                            session_id: parent_session_id
                                .expect("parent activation requires parent session"),
                            activation_id: activation.activation_id,
                        })
                        .into_iter()
                        .collect(),
                    quiescent_descendants_of: None,
                })
                .await
                .map_err(turn_store_error)?;

            {
                let mut state = lock_state(&kernel.inner);
                let session = state
                    .sessions
                    .get_mut(&session_id)
                    .ok_or_else(|| TurnError::SessionNotFound(session_id.to_string()))?;
                if session.durable_seq != expected_fact_seq || !session.pending.is_empty() {
                    return Err(TurnError::Invariant(
                        "resident session changed across atomic message claim".into(),
                    ));
                }
                for fact in &facts {
                    apply_recovered_fact(
                        &mut session.turns,
                        &mut session.turn_order,
                        &session.header,
                        fact,
                    )
                    .map_err(turn_kernel_error)?;
                }
                let turn = session
                    .turns
                    .get_mut(&request.turn_id)
                    .expect("committed Turn is installed");
                for seq in claimed_fact_seq..=final_fact_seq {
                    super::turn_state::apply_claimed_input(turn, &request.step_id, seq)?;
                }
                turn.prepared_lane = lane;
                turn.execution = execution;
                session.durable_seq = final_fact_seq;
                session.flush_status.send_replace(FlushStatus {
                    durable_seq: final_fact_seq,
                    permanent_error: session.permanent_flush_error.clone(),
                });
                publish_live_watermarks(session);
                enqueue(&mut state, session_id.clone(), request.turn_id.clone());
            }
            drop(resume_admission);
            kernel.inner.claim_changed.notify_waiters();
            Ok(SubmittedTurn {
                session_id,
                turn_id: request.turn_id,
                accepted_seq: expected_fact_seq + 1,
            })
        })
        .await
    }
}

impl AgentKernel {
    async fn next_child_path(
        &self,
        parent_session_id: &SessionId,
        parent_path: &AgentPath,
        task_name: &str,
    ) -> TurnResult<AgentPath> {
        let direct = list_direct_agent_children(&self.inner.store, parent_session_id).await?;
        if direct.iter().any(|child| child.task_name == task_name) {
            return Err(TurnError::Invalid(
                "subagent task name is already present below this parent".into(),
            ));
        }
        let used_segments = direct
            .iter()
            .filter_map(|child| child.path.segments().last().copied())
            .collect::<BTreeSet<_>>();
        let segment = (1..=u16::MAX)
            .find(|segment| !used_segments.contains(segment))
            .ok_or(TurnError::Capacity)?;
        let mut segments = parent_path.segments().to_vec();
        segments.push(segment);
        AgentPath::new(segments).map_err(|error| TurnError::Invalid(error.to_string()))
    }

    #[allow(clippy::too_many_lines)] // Child identity, lineage, source admission and initial message form one preparation protocol.
    async fn spawn_agent_prepared(&self, request: SpawnAgentRequest) -> TurnResult<SpawnedAgent> {
        self.validate_agent_caller(&request.caller)?;
        if let Some(role) = &request.role {
            role.validate()
                .map_err(|error| TurnError::Invalid(error.to_string()))?;
        }
        let _ = spawn_selection(&request, None)?;
        validate_identifier("subagent task name", &request.task_name)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        rsi_agent_session_protocol::validate_turn_text(&request.message)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let message = AgentMessage {
            message_id: request.message_id.clone(),
            source: AgentMessageSource::Agent {
                source_session_id: request.caller.session_id().clone(),
            },
            content: vec![AgentMessageContent::Text {
                text: request.message.clone(),
            }],
            options: MessageOptions::default(),
        };
        message
            .validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let admission = self
            .inner
            .submission_admission
            .acquire_pair(&request.child_session_id, Some(request.caller.session_id()))
            .await?;
        if let Some(existing) = self.existing_spawn(&request, &message).await? {
            return Ok(existing);
        }
        let parent_header = request.caller.header();
        let parent_directory = self.current_working_directory(parent_header).await?;
        let record = if let Some(rsi_agent_turn_protocol::SpawnRoleSelection::Reference {
            reference,
            resolver,
        }) = &request.role
        {
            let seed = request
                .cancellation
                .run_until_cancelled(
                    resolver.resolve(
                        parent_directory
                            .as_deref()
                            .unwrap_or_else(|| parent_header.coordinates()),
                        request.caller.execution(),
                        reference,
                        request.cancellation.clone(),
                    ),
                )
                .await
                .ok_or(TurnError::StaleClaim)??;
            seed.validate()
                .map_err(|error| TurnError::Invalid(error.to_string()))?;
            if &seed.reference != reference {
                return Err(TurnError::Invalid(
                    "resolved role reference mismatch".into(),
                ));
            }
            Some(rsi_agent_session_protocol::SpawnRoleRecord {
                seed,
                request_sha256: spawn_request_digest(&request)?,
            })
        } else {
            None
        };
        let selection = spawn_selection(&request, record.as_ref())?;
        let parent_session_id = parent_header.session_id().clone();
        let (root_session_id, parent_path) = agent_root_and_path(parent_header);
        if parent_path.depth() >= rsi_agent_session_protocol::MAXIMUM_AGENT_TREE_DEPTH {
            return Err(TurnError::Capacity);
        }
        let tree = descendant_session_ids(&self.inner.store, &root_session_id).await?;
        if tree.len().saturating_add(1) >= MAXIMUM_DURABLE_AGENT_TREE_NODES {
            return Err(TurnError::Capacity);
        }
        if tree
            .iter()
            .any(|session_id| session_id == &request.child_session_id)
            || request.child_session_id == root_session_id
        {
            return Err(TurnError::Invalid(
                "subagent session identity is already present in this tree".into(),
            ));
        }
        let path = self
            .next_child_path(&parent_session_id, &parent_path, &request.task_name)
            .await?;
        let boundary = self
            .inner
            .store
            .resolve_fork_boundary(
                &parent_session_id,
                &rsi_agent_session_protocol::ForkPoint::before(request.caller.turn_id()),
                request.fork_turns.clone(),
            )
            .await
            .map_err(turn_store_error)?;
        let origin = ForkOrigin {
            parent: rsi_agent_session_protocol::HistoryOrigin {
                parent_session_id: parent_session_id.clone(),
                parent_header_fingerprint: parent_header
                    .fingerprint()
                    .map_err(|error| TurnError::Invalid(error.to_string()))?,
                point: rsi_agent_session_protocol::ForkPoint::BeforeTurn {
                    turn_id: request.caller.turn_id().clone(),
                },
                resolved_after_seq: boundary.resolved_after_seq,
                resolved_terminal_seq: boundary.resolved_terminal_seq,
                terminal_prefix_sha256: boundary.terminal_prefix_sha256.clone(),
                resolved_terminal_control_seq: boundary.resolved_terminal_control_seq,
                requested_turns: request.fork_turns.clone(),
                effective_turns: boundary.effective_turns,
            }
            .into(),
            root_session_id: root_session_id.clone(),
            path: path.clone(),
            task_name: request.task_name,
        };
        let parent_composition = self.composition(request.caller.claim())?;
        let selected = self
            .current_permission(parent_header, &parent_composition)
            .await?;
        let input =
            rsi_agent_turn_protocol::initial_turn_input(self, request.caller.claim()).await?;
        let ceiling = selected.map_or(input.sandbox, |value| {
            SessionPermission::narrower_sandbox(input.sandbox, value.sandbox)
        });
        let child_header = parent_header
            .forked_child(
                request.child_session_id.clone(),
                self.inner.clock.now_ms().max(1),
                origin,
                selection,
                parent_directory.as_deref(),
            )
            .and_then(|header| header.with_agent_preset_id(parent_composition.preset_id().clone()))
            .and_then(|header| {
                header.with_execution_policy(
                    ceiling,
                    input.require_approval || parent_header.settings().require_approval(),
                )
            })
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let composition = self
            .inner
            .composition
            .pin_session(&child_header, None)
            .await
            .map_err(turn_composition_error)?;
        let tool_names: std::collections::BTreeSet<_> = composition
            .tools()
            .definitions()
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect();
        let shadows_output = tool_names
            .iter()
            .any(|name| name == rsi_agent_session_protocol::REPORT_RESULT_TOOL);
        let policy = rsi_agent_session_protocol::DelegationPolicy::freeze(
            record.as_ref().map(|record| &record.seed.role).or_else(|| {
                request
                    .role
                    .as_ref()
                    .and_then(rsi_agent_turn_protocol::SpawnRoleSelection::inline)
            }),
            tool_names,
            parent_header.delegation_policy(),
        )
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let child_header = child_header
            .with_delegation_policy(Some(policy))
            .and_then(|header| header.with_spawn_role(record))
            .and_then(|header| {
                header.with_initial_output(request.output_contract.clone().map(|contract| {
                    rsi_agent_session_protocol::InitialOutputContract {
                        message_id: request.message_id.clone(),
                        contract,
                    }
                }))
            })
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        if request.output_contract.is_some() && shadows_output {
            return Err(TurnError::Invalid(
                "catalog shadows the reserved output Tool".into(),
            ));
        }
        let mut prepared =
            PreparedFreshSession::new(child_header, composition).map_err(turn_composition_error)?;
        let baseline = self
            .fork_baseline(
                parent_header,
                prepared.composition(),
                &request.fork_turns,
                &boundary,
                request.cancellation.clone(),
            )
            .await?;
        prepared = prepared
            .with_baseline(baseline)
            .map_err(turn_composition_error)?;
        self.validate_agent_caller(&request.caller)?;
        let receipt = self
            .submit_message_admitted(
                SubmitMessage {
                    session: SubmitSession::Fresh(prepared),
                    message,
                    delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
                },
                MessageAdmission::Agent(&request.caller, &request.cancellation),
                admission,
            )
            .await?;
        Ok(SpawnedAgent {
            session_id: request.child_session_id,
            path,
            message: receipt,
        })
    }

    async fn existing_spawn(
        &self,
        request: &SpawnAgentRequest,
        message: &AgentMessage,
    ) -> TurnResult<Option<SpawnedAgent>> {
        let header =
            match read_validated_header_bounded(&self.inner, &request.child_session_id).await {
                Ok(header) => header,
                Err(StoreError::NotFound(_)) => return Ok(None),
                Err(error) => return Err(turn_store_error(error)),
            };
        let origin = header
            .fork_origin()
            .filter(|origin| {
                origin.parent.session_id() == request.caller.session_id()
                    && origin.parent.history().is_some_and(|history| {
                        history.point.turn_id() == request.caller.turn_id()
                            && history.requested_turns == request.fork_turns
                    })
                    && origin.task_name == request.task_name
            })
            .ok_or_else(|| {
                TurnError::Invalid("spawn retry disagrees with existing child lineage".into())
            })?;
        let policy = header
            .delegation_policy()
            .ok_or_else(|| TurnError::Invalid("spawn retry lacks delegation policy".into()))?;
        let saved = header.spawn_role();
        let matches = match (&request.role, saved) {
            (
                Some(rsi_agent_turn_protocol::SpawnRoleSelection::Reference { reference, .. }),
                Some(record),
            ) => {
                &record.seed.reference == reference
                    && record.request_sha256 == spawn_request_digest(request)?
            }
            (Some(rsi_agent_turn_protocol::SpawnRoleSelection::Reference { .. }), None)
            | (_, Some(_)) => false,
            (role, None) => policy
                .matches_role(
                    role.as_ref()
                        .and_then(rsi_agent_turn_protocol::SpawnRoleSelection::inline),
                )
                .map_err(|error| TurnError::Invalid(error.to_string()))?,
        };
        if !matches {
            return Err(TurnError::Invalid(
                "spawn retry disagrees with frozen role or request".into(),
            ));
        }
        let parent = request.caller.header();
        let preset = self
            .composition(request.caller.claim())?
            .preset_id()
            .clone();
        let expected = parent
            .forked_child(
                request.child_session_id.clone(),
                header.created_at_ms(),
                origin.clone(),
                spawn_selection(request, saved)?,
                Some(header.coordinates()),
            )
            .and_then(|expected| expected.with_agent_preset_id(preset))
            .and_then(|expected| {
                expected.with_execution_policy(
                    header.settings().sandbox(),
                    header.settings().require_approval(),
                )
            })
            .and_then(|header| header.with_delegation_policy(Some(policy.clone())))
            .and_then(|header| header.with_spawn_role(saved.cloned()))
            .and_then(|header| {
                header.with_initial_output(request.output_contract.clone().map(|contract| {
                    rsi_agent_session_protocol::InitialOutputContract {
                        message_id: request.message_id.clone(),
                        contract,
                    }
                }))
            })
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        if expected != header
            || origin.parent.fingerprint()
                != parent
                    .fingerprint()
                    .map_err(|error| TurnError::Invalid(error.to_string()))?
        {
            return Err(TurnError::Invalid(
                "spawn retry disagrees with existing child Header".into(),
            ));
        }
        let scan = read_mailbox_selection(
            &self.inner,
            &request.child_session_id,
            MailboxSelection::Exact(request.message_id.clone()),
        )
        .await?;
        // A spawned child always publishes its executable selection at control one.
        let acceptance_seq = 2;
        let entry = scan
            .selected
            .map(durable_message_entry)
            .filter(|entry| {
                &entry.message == message
                    && entry.accepted_control_seq == acceptance_seq
                    && entry.root_session_id == origin.root_session_id
                    && entry.target == MessageTarget::NextTurn
                    && entry.wake_required
            })
            .ok_or_else(|| TurnError::MessageConflict {
                session: request.child_session_id.to_string(),
                message: request.message_id.to_string(),
            })?;
        self.validate_agent_caller(&request.caller)?;
        Ok(Some(SpawnedAgent {
            session_id: request.child_session_id.clone(),
            path: origin.path.clone(),
            message: message_receipt(&request.child_session_id, scan.durable_fact_seq, &entry),
        }))
    }
}

impl AgentKernel {
    async fn send_agent_message_prepared(
        &self,
        request: SendAgentMessage,
    ) -> TurnResult<MessageReceipt> {
        self.validate_agent_caller(&request.caller)?;
        rsi_agent_session_protocol::validate_turn_text(&request.message)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let caller_session_id = request.caller.session_id();
        if caller_session_id == &request.target_session_id {
            return Err(TurnError::Invalid(
                "Agent message cannot target its caller".into(),
            ));
        }
        let target_header = read_validated_header_bounded(&self.inner, &request.target_session_id)
            .await
            .map_err(turn_store_error)?;
        let caller_parent = request
            .caller
            .header()
            .fork_origin()
            .map(|origin| origin.parent.session_id());
        let target_parent = target_header
            .fork_origin()
            .map(|origin| origin.parent.session_id());
        if caller_parent != Some(&request.target_session_id)
            && target_parent != Some(caller_session_id)
        {
            return Err(TurnError::Invalid(
                "Agent messaging is limited to one direct parent-child edge".into(),
            ));
        }
        let prepared = self.prepare_resume(&request.target_session_id).await?;
        self.validate_agent_caller(&request.caller)?;
        self.submit_message_authorized(
            SubmitMessage {
                session: SubmitSession::Resume(prepared),
                message: AgentMessage {
                    message_id: request.message_id,
                    source: AgentMessageSource::Agent {
                        source_session_id: caller_session_id.clone(),
                    },
                    content: vec![AgentMessageContent::Text {
                        text: request.message,
                    }],
                    options: MessageOptions::default(),
                },
                delivery: if request.start_new_turn {
                    rsi_agent_session_protocol::MessageDelivery::NextTurn
                } else {
                    rsi_agent_session_protocol::MessageDelivery::NextStep
                },
            },
            Some((&request.caller, &request.cancellation)),
        )
        .await
    }
}

impl AgentKernel {
    async fn interrupt_agent_prepared(
        &self,
        caller: &AgentCallerAuthority,
        target_session_id: &SessionId,
        cancellation: CancellationToken,
    ) -> TurnResult<CancelResult> {
        self.validate_agent_caller(caller)?;
        if caller.session_id() == target_session_id {
            return Err(TurnError::Invalid(
                "Agent cannot interrupt its own calling Turn".into(),
            ));
        }
        let mut candidate = read_validated_header_bounded(&self.inner, target_session_id)
            .await
            .map_err(turn_store_error)?;
        let mut authorized = false;
        for _ in 0..rsi_agent_session_protocol::MAXIMUM_AGENT_TREE_DEPTH {
            let Some(origin) = candidate.fork_origin() else {
                break;
            };
            if origin.parent.session_id() == caller.session_id() {
                authorized = true;
                break;
            }
            candidate = read_validated_header_bounded(&self.inner, origin.parent.session_id())
                .await
                .map_err(turn_store_error)?;
        }
        if !authorized {
            return Err(TurnError::Invalid(
                "Agent interrupt requires a live ancestor caller".into(),
            ));
        }
        let open = self
            .inner
            .store
            .list_open_turns(target_session_id, 0, 1)
            .await
            .map_err(turn_store_error)?;
        let Some(turn) = open.turns.first() else {
            return Ok(CancelResult {
                accepted: false,
                already_terminal: true,
            });
        };
        self.interrupt_descendant(caller, target_session_id, &turn.turn_id, &cancellation)
            .await
    }
}

fn spawn_selection(
    request: &SpawnAgentRequest,
    saved: Option<&rsi_agent_session_protocol::SpawnRoleRecord>,
) -> TurnResult<rsi_agent_session_protocol::ModelSelection> {
    let source = request.caller.source_selection().ok_or_else(|| {
        TurnError::Invalid("spawn requires an authenticated Tool model origin".into())
    })?;
    let selection = match &request.model {
        Some(model) => rsi_agent_session_protocol::ModelSelection {
            model: model.clone(),
            reasoning_effort: request.reasoning_effort.clone(),
        },
        None if request.reasoning_effort.is_none() => saved
            .and_then(|record| record.seed.model.as_ref())
            .unwrap_or(source)
            .clone(),
        None => {
            return Err(TurnError::Invalid(
                "child reasoning effort requires an explicit child model".into(),
            ));
        }
    };
    selection
        .validate()
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
    Ok(selection)
}

fn spawn_request_digest(request: &SpawnAgentRequest) -> TurnResult<String> {
    use sha2::{Digest as _, Sha256};
    let Some(rsi_agent_turn_protocol::SpawnRoleSelection::Reference { reference, .. }) =
        &request.role
    else {
        return Err(TurnError::Invalid(
            "named spawn requires a reference".into(),
        ));
    };
    let value = serde_json::json!({
        "version": 1, "reference": reference, "model": request.model,
        "effort": request.reasoning_effort, "child": request.child_session_id,
        "task": request.task_name, "message_id": request.message_id, "message": request.message,
        "fork": request.fork_turns, "output": request.output_contract,
        "parent": request.caller.header().fingerprint().map_err(|error| TurnError::Invalid(error.to_string()))?,
        "turn": request.caller.turn_id(), "effect": request.caller.tool_effect_id(),
        "selection": request.caller.source_selection(),
    });
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&value).map_err(|error| TurnError::Invalid(error.to_string()))?
        )
    ))
}
