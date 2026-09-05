use super::*;

#[async_trait]
impl TurnService for SessionKernel {
    async fn prepare_resume(&self, session_id: &SessionId) -> TurnResult<PreparedResumeSession> {
        self.prepare_resume_session(session_id).await
    }

    async fn tree_sessions(&self, session_id: &SessionId) -> TurnResult<Vec<SessionId>> {
        let header = self.session_header(session_id).await?;
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
    async fn spawn_agent(&self, request: SpawnAgentRequest) -> TurnResult<SpawnedAgent> {
        self.validate_agent_caller(&request.caller)?;
        validate_identifier("subagent task name", &request.task_name)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        rsi_agent_session_protocol::validate_turn_text(&request.message)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let parent_header = request.caller.header().clone();
        let parent_session_id = parent_header.session_id().clone();
        let (root_session_id, parent_path) = agent_root_and_path(&parent_header);
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
        let direct = list_direct_agent_children(&self.inner.store, &parent_session_id).await?;
        if direct
            .iter()
            .any(|child| child.task_name == request.task_name)
        {
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
        let path =
            AgentPath::new(segments).map_err(|error| TurnError::Invalid(error.to_string()))?;
        let boundary = self
            .inner
            .store
            .resolve_fork_boundary(
                &parent_session_id,
                request.caller.turn_id(),
                request.fork_turns.clone(),
            )
            .await
            .map_err(turn_store_error)?;
        let origin = ForkOrigin {
            parent_session_id: parent_session_id.clone(),
            root_session_id: root_session_id.clone(),
            path: path.clone(),
            task_name: request.task_name,
            parent_header_fingerprint: parent_header
                .fingerprint()
                .map_err(|error| TurnError::Invalid(error.to_string()))?,
            invoking_turn_id: request.caller.turn_id().clone(),
            resolved_after_seq: boundary.resolved_after_seq,
            resolved_terminal_seq: boundary.resolved_terminal_seq,
            terminal_prefix_sha256: boundary.terminal_prefix_sha256,
            requested_turns: request.fork_turns,
            effective_turns: boundary.effective_turns,
        };
        let child_header = parent_header
            .forked_child(
                request.child_session_id.clone(),
                self.inner.clock.now_ms().max(1),
                origin,
            )
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let composition = self
            .inner
            .composition
            .pin(child_header.agent_preset_id())
            .await
            .map_err(turn_composition_error)?;
        self.validate_agent_caller(&request.caller)?;
        let receipt = self
            .submit_message(SubmitMessage {
                session: SubmitSession::Fresh(
                    PreparedFreshSession::new(child_header, composition)
                        .map_err(turn_composition_error)?,
                ),
                message: AgentMessage {
                    message_id: request.message_id,
                    source: AgentMessageSource::Agent {
                        source_session_id: parent_session_id,
                    },
                    content: vec![AgentMessageContent::Text {
                        text: request.message,
                    }],
                    options: MessageOptions::default(),
                },
                target: MessageTarget::NextTurn,
                wake_required: true,
            })
            .await?;
        Ok(SpawnedAgent {
            session_id: request.child_session_id,
            path,
            message: receipt,
        })
    }

    async fn send_agent_message(&self, request: SendAgentMessage) -> TurnResult<MessageReceipt> {
        self.validate_agent_caller(&request.caller)?;
        rsi_agent_session_protocol::validate_turn_text(&request.message)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let caller_session_id = request.caller.session_id();
        if caller_session_id == &request.target_session_id {
            return Err(TurnError::Invalid(
                "Agent message cannot target its caller".into(),
            ));
        }
        let target_header = read_header_bounded(&self.inner, &request.target_session_id)
            .await
            .map_err(turn_store_error)?;
        let caller_parent = request
            .caller
            .header()
            .fork_origin()
            .map(|origin| &origin.parent_session_id);
        let target_parent = target_header
            .fork_origin()
            .map(|origin| &origin.parent_session_id);
        if caller_parent != Some(&request.target_session_id)
            && target_parent != Some(caller_session_id)
        {
            return Err(TurnError::Invalid(
                "Agent messaging is limited to one direct parent-child edge".into(),
            ));
        }
        let prepared = self.prepare_resume(&request.target_session_id).await?;
        self.validate_agent_caller(&request.caller)?;
        self.submit_message(SubmitMessage {
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
            target: if request.start_new_turn {
                MessageTarget::NextTurn
            } else {
                MessageTarget::NextStep
            },
            wake_required: request.start_new_turn,
        })
        .await
    }

    async fn list_agents(
        &self,
        caller: &AgentCallerAuthority,
        scope: AgentListScope,
    ) -> TurnResult<Vec<AgentNode>> {
        self.validate_agent_caller(caller)?;
        let parent_session_id = caller.session_id();
        let (root_session_id, _) = agent_root_and_path(caller.header());
        let ready = ready_sessions_for_root(&self.inner.store, &root_session_id).await?;
        let descriptors = match scope {
            AgentListScope::Children => {
                list_direct_agent_children(&self.inner.store, parent_session_id)
                    .await?
                    .into_iter()
                    .map(|child| (parent_session_id.clone(), child))
                    .collect()
            }
            AgentListScope::Descendants => {
                list_agent_descendants(&self.inner.store, parent_session_id).await?
            }
        };
        let mut nodes = Vec::with_capacity(descriptors.len());
        for (parent, child) in descriptors {
            nodes.push(AgentNode {
                state: durable_agent_node_state(&self.inner.store, &child.session_id, &ready)
                    .await?,
                session_id: child.session_id,
                parent_session_id: parent,
                path: child.path,
                task_name: child.task_name,
            });
        }
        self.validate_agent_caller(caller)?;
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
        self.validate_agent_caller(caller)?;
        let baseline = self
            .inner
            .store
            .read_descendant_control_snapshot(caller.session_id())
            .await
            .map_err(turn_store_error)?;
        baseline.validate().map_err(turn_store_error)?;
        let initial = self
            .list_agents(caller, AgentListScope::Descendants)
            .await?;
        let initial_membership = initial
            .iter()
            .map(|node| node.session_id.clone())
            .collect::<BTreeSet<_>>();
        let baseline_membership = baseline
            .descendants
            .iter()
            .map(|descendant| descendant.session_id.clone())
            .collect::<BTreeSet<_>>();
        let same_membership = initial_membership == baseline_membership;
        if !same_membership {
            return Ok(AgentWaitResult::Changed);
        }
        if initial.is_empty()
            || initial
                .iter()
                .all(|node| node.state == AgentNodeState::Idle)
        {
            return match tokio::time::timeout_at(
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
        }
        let (activation_id, step_id, tree_lane, turn_cancellation) = {
            let state = lock_state(&self.inner);
            let turn = self.validate_claim(&state, caller.claim())?;
            (
                turn.activation_id.clone().ok_or_else(|| {
                    TurnError::Invalid("Agent wait requires an activation-owned Turn".into())
                })?,
                turn.current_step
                    .clone()
                    .ok_or_else(|| TurnError::Invalid("Agent wait requires an open Step".into()))?,
                turn.claim
                    .as_ref()
                    .expect("validated claim owns a lane")
                    .tree_lane
                    .clone(),
                turn.cancellation.clone(),
            )
        };
        let timeout_ms = u64::try_from(timeout.as_millis())
            .map_err(|_| TurnError::Invalid("Agent wait timeout exceeds its bound".into()))?;
        let deadline_ms = self
            .inner
            .clock
            .now_ms()
            .max(1)
            .checked_add(timeout_ms)
            .ok_or_else(|| TurnError::Invalid("Agent wait deadline overflowed".into()))?;
        append_wait_control(
            self,
            caller,
            &activation_id,
            AgentControlRecordBody::WaitParked {
                activation_id: activation_id.clone(),
                turn_id: caller.turn_id().clone(),
                step_id: step_id.clone(),
                deadline_ms,
            },
        )
        .await?;
        tree_lane
            .permit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.inner.claim_changed.notify_waiters();
        let (mut result, mut cause) = 'waiting: loop {
            let changed = self.inner.claim_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            match tokio::time::timeout_at(
                deadline,
                observe_agent_wait_change(self, caller, &baseline),
            )
            .await
            {
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
                () = &mut changed => {}
                () = tokio::time::sleep(DURABLE_OBSERVER_FALLBACK_INTERVAL) => {}
                () = tokio::time::sleep_until(deadline) => {
                    break 'waiting (Ok(AgentWaitResult::TimedOut), WaitResumeCause::Timeout);
                }
            }
        };
        let permit = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                result = Err(TurnError::Cancelled); cause = WaitResumeCause::Cancel; None
            }
            () = turn_cancellation.cancelled() => {
                result = Err(TurnError::Cancelled); cause = WaitResumeCause::Cancel; None
            }
            () = self.inner.stop_worker.cancelled() => {
                result = Err(TurnError::ShuttingDown); cause = WaitResumeCause::Cancel; None
            }
            permit = Arc::clone(&tree_lane.pool).acquire_owned() => {
                Some(permit.map_err(|_| TurnError::ShuttingDown)?)
            }
        };
        append_wait_control(
            self,
            caller,
            &activation_id,
            AgentControlRecordBody::WaitResumed {
                activation_id: activation_id.clone(),
                turn_id: caller.turn_id().clone(),
                step_id,
                cause,
            },
        )
        .await?;
        *tree_lane
            .permit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = permit;
        result
    }

    async fn interrupt_agent(
        &self,
        caller: &AgentCallerAuthority,
        target_session_id: &SessionId,
    ) -> TurnResult<CancelResult> {
        self.validate_agent_caller(caller)?;
        if caller.session_id() == target_session_id {
            return Err(TurnError::Invalid(
                "Agent cannot interrupt its own calling Turn".into(),
            ));
        }
        let mut candidate = read_header_bounded(&self.inner, target_session_id)
            .await
            .map_err(turn_store_error)?;
        let mut authorized = false;
        for _ in 0..rsi_agent_session_protocol::MAXIMUM_AGENT_TREE_DEPTH {
            let Some(origin) = candidate.fork_origin() else {
                break;
            };
            if &origin.parent_session_id == caller.session_id() {
                authorized = true;
                break;
            }
            candidate = read_header_bounded(&self.inner, &origin.parent_session_id)
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
        self.validate_agent_caller(caller)?;
        self.cancel(target_session_id, &turn.turn_id, None).await
    }

    #[allow(clippy::too_many_lines)] // Admission validates and commits Header, mailbox payload, and ready index as one operation.
    async fn submit_message(&self, request: SubmitMessage) -> TurnResult<MessageReceipt> {
        request
            .message
            .validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let session_id = request.session.session_id().clone();
        let requested_header = request.session.header().clone();
        let root_session_id = agent_root_and_path(&requested_header).0;
        if let SubmitSession::Resume(prepared) = &request.session {
            self.inner.resume_issuer.inspect(prepared)?;
        }
        let _admission = self.inner.submission_admission.acquire(&session_id).await?;
        if let Some(error) = lock_state(&self.inner)
            .sessions
            .get(&session_id)
            .and_then(|session| session.permanent_flush_error.clone())
        {
            return Err(TurnError::Flush(error));
        }

        let durable_header = match read_header_bounded(&self.inner, &session_id).await {
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
            scan_durable_messages(&self.inner, &session_id, Some(&request.message.message_id))
                .await?
        } else {
            DurableMessageScan {
                selected: None,
                pending_count: 0,
                pending: Vec::new(),
                durable_control_seq: 0,
                durable_fact_seq: 0,
            }
        };
        if let Some(entry) = scan.selected {
            if entry.message != request.message
                || entry.root_session_id != root_session_id
                || entry.target != request.target
                || entry.wake_required != request.wake_required
            {
                return Err(TurnError::MessageConflict {
                    session: session_id.to_string(),
                    message: request.message.message_id.to_string(),
                });
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
        if scan.pending_count.saturating_add(completion_reservations)
            >= MAXIMUM_PENDING_AGENT_MESSAGES
        {
            return Err(TurnError::Capacity);
        }

        let expected_fact_seq = scan.durable_fact_seq;
        let control_seq = scan
            .durable_control_seq
            .checked_add(1)
            .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?;
        let control = AgentControlRecord::new(
            control_seq,
            self.inner.clock.now_ms().max(1),
            AgentControlRecordBody::MessageAccepted {
                message: request.message.clone(),
                root_session_id,
                target: request.target,
                wake_required: request.wake_required,
            },
        )
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let header = match request.session {
            SubmitSession::Fresh(prepared) => {
                let (header, _composition) = prepared.into_parts();
                Some(header)
            }
            SubmitSession::Resume(prepared) => {
                let _parts = self.inner.resume_issuer.consume(prepared)?;
                None
            }
        };
        let commit = self
            .commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: session_id.clone(),
                    expected_fact_seq,
                    expected_control_seq: scan.durable_control_seq,
                    header,
                    facts: Vec::new(),
                    controls: vec![control],
                }],
                required_active_activations: Vec::new(),
                quiescent_sessions: Vec::new(),
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
        self.inner.claim_changed.notify_waiters();
        Ok(MessageReceipt {
            session_id,
            message_id: request.message.message_id,
            accepted_control_seq: control_seq,
            observed_fact_seq,
            state: MessageState::Pending,
        })
    }

    async fn message_status(
        &self,
        session_id: &SessionId,
        message_id: &MessageId,
    ) -> TurnResult<MessageReceipt> {
        let scan = scan_durable_messages(&self.inner, session_id, Some(message_id)).await?;
        scan.selected
            .as_ref()
            .map(|entry| message_receipt(session_id, scan.durable_fact_seq, entry))
            .ok_or_else(|| {
                TurnError::Invalid(format!(
                    "message `{}` does not exist in session `{}`",
                    message_id.as_str(),
                    session_id.as_str()
                ))
            })
    }

    #[allow(clippy::too_many_lines)] // Claim materializes Activation, Turn, Step, context, reservation, and indexes in one transaction.
    async fn claim_message(&self, request: ClaimMessage) -> TurnResult<SubmittedTurn> {
        let (header, _) = self.inner.resume_issuer.inspect(&request.session)?;
        let header = header.clone();
        let session_id = header.session_id().clone();
        let parent_session_id = header
            .fork_origin()
            .map(|origin| origin.parent_session_id.clone());
        let _admissions = self
            .inner
            .submission_admission
            .acquire_many(
                std::iter::once(session_id.clone()).chain(parent_session_id.iter().cloned()),
            )
            .await?;
        let resume_admission = self.reserve_resume_submission(&request.session).await?;

        let live_seq = {
            let state = lock_state(&self.inner);
            state
                .sessions
                .get(&session_id)
                .ok_or_else(|| TurnError::SessionNotFound(session_id.to_string()))?
                .live_seq()
                .map_err(turn_kernel_error)?
        };
        self.wait_for_durable(&session_id, live_seq)
            .await
            .map_err(turn_kernel_error)?;
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

        let scan =
            scan_durable_messages(&self.inner, &session_id, Some(&request.message_id)).await?;
        let entry = scan.selected.ok_or_else(|| {
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
                entered_fact_seq: _,
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
        if entry.target != MessageTarget::NextTurn {
            return Err(TurnError::Invalid(
                "next-Step message requires its owning active Turn".into(),
            ));
        }

        let expected_fact_seq = lock_state(&self.inner)
            .sessions
            .get(&session_id)
            .ok_or_else(|| TurnError::SessionNotFound(session_id.to_string()))?
            .durable_seq;
        let model = entry.message.options.model.clone();
        let sandbox = entry
            .message
            .options
            .sandbox
            .unwrap_or(header.settings().sandbox());
        let require_approval = header.settings().require_approval()
            || sandbox == rsi_sandbox::SandboxMode::DangerFullAccess;
        let timestamp_ms = self.inner.clock.now_ms().max(1);
        let current_context = lock_state(&self.inner)
            .sessions
            .get(&session_id)
            .ok_or_else(|| TurnError::SessionNotFound(session_id.to_string()))?
            .workspace_context
            .clone();
        let context_snapshot = self
            .inner
            .workspace_context
            .snapshot(&header, &[&entry.message])
            .await
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let (background, invocations, next_context) = workspace_context_bodies(
            &request.turn_id,
            &request.step_id,
            &current_context,
            context_snapshot,
        );
        let background_len = background.len();
        let mut fact_bodies = vec![
            SessionFactBody::MessageTurnAccepted {
                turn_id: request.turn_id.clone(),
                activation_id: request.activation_id.clone(),
                message_ids: vec![request.message_id.clone()],
                model,
                sandbox,
                require_approval,
            },
            SessionFactBody::StepStarted {
                turn_id: request.turn_id.clone(),
                step_id: request.step_id.clone(),
            },
        ];
        fact_bodies.extend(background);
        fact_bodies.push(SessionFactBody::InputMessageEntered {
            turn_id: request.turn_id.clone(),
            step_id: request.step_id.clone(),
            source: entered_message_source(&entry.message),
            content: entry.message.content.clone(),
        });
        fact_bodies.extend(invocations);
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
        let entered_fact_seq = expected_fact_seq
            .checked_add(
                u64::try_from(background_len)
                    .map_err(|_| TurnError::Invariant("context Fact count exceeds u64".into()))?
                    .checked_add(3)
                    .ok_or_else(|| TurnError::Invariant("message Fact offset exhausted".into()))?,
            )
            .ok_or_else(|| TurnError::Invariant("message Fact sequence exhausted".into()))?;
        let final_fact_seq = facts
            .last()
            .expect("message claim always creates Facts")
            .seq();
        let parent_activation = if let Some(parent_session_id) = &parent_session_id {
            let active = self
                .inner
                .store
                .active_activation(parent_session_id)
                .await
                .map_err(turn_store_error)?
                .ok_or_else(|| {
                    TurnError::Invalid(
                        "child activation requires its parent activation to remain active".into(),
                    )
                })?;
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
            Some(active)
        } else {
            None
        };
        let mut controls = vec![
            AgentControlRecord::new(
                scan.durable_control_seq
                    .checked_add(1)
                    .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?,
                timestamp_ms,
                AgentControlRecordBody::ActivationStarted {
                    activation_id: request.activation_id.clone(),
                    root_session_id: entry.root_session_id,
                    parent_session_id: parent_session_id.clone(),
                    path: request.path,
                },
            ),
            AgentControlRecord::new(
                scan.durable_control_seq
                    .checked_add(2)
                    .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?,
                timestamp_ms,
                AgentControlRecordBody::MessageClaimed {
                    message_id: request.message_id,
                    activation_id: request.activation_id.clone(),
                    turn_id: request.turn_id.clone(),
                    step_id: request.step_id,
                    entered_fact_seq,
                },
            ),
        ];
        if let Some(parent_session_id) = &parent_session_id {
            controls.push(AgentControlRecord::new(
                scan.durable_control_seq
                    .checked_add(3)
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
        let controls = controls
            .into_iter()
            .collect::<rsi_agent_session_protocol::Result<Vec<_>>>()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let _parts = self.inner.resume_issuer.consume(request.session)?;
        self.inner
            .store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: session_id.clone(),
                    expected_fact_seq,
                    expected_control_seq: scan.durable_control_seq,
                    header: None,
                    facts: facts.clone(),
                    controls,
                }],
                required_active_activations: parent_activation
                    .map(|activation| AgentActivationGuard {
                        session_id: parent_session_id
                            .expect("parent activation requires parent session"),
                        activation_id: activation.activation_id,
                    })
                    .into_iter()
                    .collect(),
                quiescent_sessions: Vec::new(),
            })
            .await
            .map_err(turn_store_error)?;

        {
            let mut state = lock_state(&self.inner);
            let session = state
                .sessions
                .get_mut(&session_id)
                .ok_or_else(|| TurnError::SessionNotFound(session_id.to_string()))?;
            if session.durable_seq != expected_fact_seq || !session.pending.is_empty() {
                return Err(TurnError::Invariant(
                    "resident session changed across atomic message claim".into(),
                ));
            }
            let budget = session.header.settings().turn_budget().clone();
            for fact in &facts {
                apply_recovered_fact(&mut session.turns, &mut session.turn_order, &budget, fact)
                    .map_err(turn_kernel_error)?;
            }
            session.workspace_context = next_context;
            session.durable_seq = final_fact_seq;
            session.flush_status.send_replace(FlushStatus {
                durable_seq: final_fact_seq,
                permanent_error: None,
            });
            publish_live_watermarks(session);
            enqueue(&mut state, session_id.clone(), request.turn_id.clone());
        }
        drop(resume_admission);
        self.inner.claim_changed.notify_waiters();
        Ok(SubmittedTurn {
            session_id,
            turn_id: request.turn_id,
            accepted_seq: expected_fact_seq + 1,
        })
    }

    async fn observe_session(
        &self,
        session_id: &SessionId,
        cursor: ObservationCursor,
    ) -> TurnResult<SessionObservationStream> {
        let observer_lease = ObserverLease::acquire(&self.inner)?;
        let controls = read_controls_bounded(
            &self.inner,
            session_id,
            cursor.control_seq,
            MAXIMUM_FACTS_PER_READ,
        )
        .await
        .map_err(turn_store_error)?;
        let facts = read_facts_bounded(
            &self.inner,
            session_id,
            cursor.fact_seq,
            MAXIMUM_FACTS_PER_READ,
        )
        .await
        .map_err(turn_store_error)?;
        controls.validate().map_err(turn_store_error)?;
        facts.validate().map_err(turn_store_error)?;
        let mut pending = VecDeque::new();
        let mut control_seq = cursor.control_seq;
        for record in controls.records {
            control_seq = record.seq();
            pending.push_back(SessionObservation::Control {
                record: Arc::new(record),
                durable_control_seq: controls.durable_seq,
            });
        }
        let mut fact_seq = cursor.fact_seq;
        for fact in facts.facts {
            fact_seq = fact.seq();
            pending.push_back(SessionObservation::Fact {
                fact: Arc::new(fact),
                durable_fact_seq: facts.durable_seq,
            });
        }
        let state = DurableObservationState {
            inner: Arc::downgrade(&self.inner),
            session_id: session_id.clone(),
            control_seq,
            fact_seq,
            pending,
            stopped: false,
            _observer_lease: observer_lease,
        };
        Ok(stream::unfold(state, durable_observation_next).boxed())
    }

    async fn cancel_target(
        &self,
        session_id: &SessionId,
        target: CancelTarget,
        reason: Option<String>,
    ) -> TurnResult<CancelResult> {
        let CancelTarget::Message(message_id) = target else {
            let CancelTarget::Turn(turn_id) = target else {
                unreachable!("closed cancellation target")
            };
            return self.cancel(session_id, &turn_id, reason).await;
        };
        if reason.is_some() {
            return Err(TurnError::Invalid(
                "unclaimed-message cancellation does not accept a free-form reason".into(),
            ));
        }
        let admission = self.inner.submission_admission.acquire(session_id).await?;
        let scan = scan_durable_messages(&self.inner, session_id, Some(&message_id)).await?;
        let entry = scan.selected.ok_or_else(|| {
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
        let expected_fact_seq = read_facts_bounded(&self.inner, session_id, 0, 1)
            .await
            .map_err(turn_store_error)?
            .durable_seq;
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
            quiescent_sessions: Vec::new(),
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
        let sandbox = sandbox.unwrap_or(profile.sandbox());
        let require_approval =
            profile.require_approval() || sandbox == rsi_sandbox::SandboxMode::DangerFullAccess;
        let body = SessionFactBody::TurnAccepted {
            turn_id: turn_id.clone(),
            text,
            model,
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
        reason: Option<String>,
    ) -> TurnResult<CancelResult> {
        let body = SessionFactBody::CancelRequested {
            turn_id: turn_id.clone(),
            reason,
        };
        body.validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let submission_admission = self.inner.submission_admission.acquire(session_id).await?;
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
            self.ensure_session_loaded(session_id).await?;
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
        let (cancel_seq, cancellation) = {
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
            (cancel_seq, cancellation)
        };
        drop(submission_admission);
        self.wait_for_durable(session_id, cancel_seq)
            .await
            .map_err(turn_kernel_error)?;
        cancellation.cancel();
        Ok(CancelResult {
            accepted: true,
            already_terminal: false,
        })
    }

    async fn observe(&self, session_id: &SessionId, after_seq: u64) -> TurnResult<TurnObservation> {
        let observer_lease = ObserverLease::acquire(&self.inner)?;
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
                let page =
                    read_facts_bounded(&self.inner, session_id, after_seq, MAXIMUM_FACTS_PER_READ)
                        .await
                        .map_err(|error| match error {
                            StoreError::Invalid(_) => TurnError::Invalid(
                                "observation cursor exceeds the durable tail".into(),
                            ),
                            other => turn_store_error(other),
                        })?;
                let page_durable_seq = page.durable_seq;
                let durable_facts = page.facts.into_iter().map(Arc::new).collect();
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
            if let Some(session) = state.sessions.get(session_id) {
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
}

pub(super) async fn append_wait_control(
    kernel: &SessionKernel,
    caller: &AgentCallerAuthority,
    activation_id: &rsi_agent_session_protocol::ActivationId,
    body: AgentControlRecordBody,
) -> TurnResult<()> {
    let session_id = caller.session_id().clone();
    let _admission = kernel
        .inner
        .submission_admission
        .acquire(&session_id)
        .await?;
    kernel.validate_agent_caller(caller)?;
    let expected_fact_seq = read_facts_bounded(&kernel.inner, &session_id, 0, 1)
        .await
        .map_err(turn_store_error)?
        .durable_seq;
    let expected_control_seq = control_tail(&kernel.inner, &session_id).await?;
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
            quiescent_sessions: Vec::new(),
        })
        .await?
        .map_err(turn_store_error)?;
    Ok(())
}
