use super::*;
impl LiveRun {
    pub(super) async fn admit_child(&self, request: ProgramAgentRequest) -> TurnResult<u32> {
        rsi_agent_session_protocol::validate_turn_text(&request.message).map_err(session_error)?;
        if let Some(role) = &request.role {
            role.validate().map_err(session_error)?;
        }
        // Parent admission serializes ordinal allocation, creation and run cancellation.
        let admission = self
            .kernel
            .inner
            .submission_admission
            .acquire(&self.descriptor.session_id)
            .await?;
        self.active().await?;
        let state = self.state().await?;
        let ordinal = u32::try_from(state.children.len() + 1).map_err(session_error)?;
        let child_id = self
            .descriptor
            .child_session_id(ordinal)
            .map_err(session_error)?;
        let message_id =
            MessageId::new(format!("{}-initial", child_id.as_str())).map_err(session_error)?;
        let (prepared, root) = self
            .prepare_child(ordinal, &child_id, &message_id, &request)
            .await?;
        let header = prepared.header().clone();
        let message = AgentMessage {
            message_id: message_id.clone(),
            source: AgentMessageSource::Agent {
                source_session_id: self.descriptor.session_id.clone(),
            },
            content: vec![AgentMessageContent::Text {
                text: request.message,
            }],
            options: MessageOptions::default(),
        };
        let mut controls = Vec::new();
        if let Some(baseline) = lifecycle::initial_domain_control(&header, prepared.baseline())? {
            controls.push(baseline);
        }
        controls.push(
            AgentControlRecord::new(
                controls.len() as u64 + 1,
                header.created_at_ms(),
                AgentControlRecordBody::MessageAccepted {
                    message,
                    delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
                    bound_turn_id: None,
                    root_session_id: root,
                    target: MessageTarget::NextTurn,
                    wake_required: true,
                },
            )
            .map_err(session_error)?,
        );
        let parent = self
            .kernel
            .program_append(
                &self.descriptor.session_id,
                &self.descriptor.run_id,
                ProgramRunEvent::ChildAdmitted {
                    ordinal,
                    child_session_id: child_id.clone(),
                    message_id,
                },
            )
            .await?;
        let kernel = self.kernel.clone();
        self.kernel
            .owned_commit(async move {
                let _admission = admission;
                kernel
                    .commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
                        sessions: vec![
                            parent,
                            AtomicSessionAppend {
                                session_id: child_id,
                                expected_fact_seq: 0,
                                expected_control_seq: 0,
                                header: Some(header),
                                facts: vec![],
                                controls,
                            },
                        ],
                        required_active_activations: vec![],
                        quiescent_descendants_of: None,
                    })
                    .await?
                    .map_err(turn_store_error)?;
                // The exact pin is retained by the run and selected again for initial claim.
                drop(prepared);
                kernel.request_ready_scan();
                Ok(ordinal)
            })
            .await
    }
    async fn prepare_child(
        &self,
        ordinal: u32,
        child_id: &SessionId,
        message_id: &MessageId,
        request: &ProgramAgentRequest,
    ) -> TurnResult<(PreparedFreshSession, SessionId)> {
        let (root, parent_path) = agent_root_and_path(&self.header);
        let tree = self
            .kernel
            .inner
            .store
            .read_agent_subtree_snapshot(&root)
            .await
            .map_err(turn_store_error)?;
        tree.validate().map_err(turn_store_error)?;
        if tree.descendants.len() + 1 >= MAXIMUM_DURABLE_AGENT_TREE_NODES
            || parent_path.depth() >= rsi_agent_session_protocol::MAXIMUM_AGENT_TREE_DEPTH
        {
            return Err(TurnError::Capacity);
        }
        let used = tree
            .descendants
            .iter()
            .filter(|child| child.parent_session_id == self.descriptor.session_id)
            .filter_map(|child| child.path.segments().last().copied())
            .collect::<BTreeSet<_>>();
        let segment = (1..=u16::MAX)
            .find(|segment| !used.contains(segment))
            .ok_or(TurnError::Capacity)?;
        let mut segments = parent_path.segments().to_vec();
        segments.push(segment);
        let fork = &self.descriptor.fork;
        let origin = ForkOrigin {
            parent_session_id: self.descriptor.session_id.clone(),
            root_session_id: root.clone(),
            path: AgentPath::new(segments).map_err(session_error)?,
            task_name: format!("{}-{ordinal}", self.descriptor.run_id),
            parent_header_fingerprint: self.descriptor.parent_header_sha256.clone(),
            invoking_turn_id: self.descriptor.creator_turn_id.clone(),
            resolved_after_seq: fork.resolved_after_seq,
            resolved_terminal_seq: fork.resolved_terminal_seq,
            terminal_prefix_sha256: fork.terminal_prefix_sha256.clone(),
            resolved_terminal_control_seq: fork.resolved_terminal_control_seq,
            terminal_control_prefix_sha256: fork.terminal_control_prefix_sha256.clone(),
            requested_turns: fork.requested_turns.clone(),
            effective_turns: fork.effective_turns,
        };
        let policy = rsi_agent_session_protocol::DelegationPolicy::freeze(
            request.role.as_ref(),
            self.composition
                .tools()
                .definitions()
                .iter()
                .map(|tool| tool.name().to_owned())
                .collect(),
            self.header.delegation_policy(),
        )
        .map_err(session_error)?;
        let header = self
            .header
            .forked_child(
                child_id.clone(),
                self.kernel.inner.clock.now_ms().max(1),
                origin,
                self.descriptor.selection.clone(),
            )
            .and_then(|header| {
                header.with_execution_policy(
                    self.descriptor.sandbox,
                    self.descriptor.require_approval,
                )
            })
            .and_then(|header| {
                header.with_execution_owner(ExecutionOwner::ProgramRun {
                    session_id: self.descriptor.session_id.clone(),
                    run_id: self.descriptor.run_id.clone(),
                    ordinal,
                })
            })
            .and_then(|header| header.with_delegation_policy(Some(policy)))
            .and_then(|header| {
                header.with_initial_output(request.output_contract.clone().map(|contract| {
                    rsi_agent_session_protocol::InitialOutputContract {
                        message_id: message_id.clone(),
                        contract,
                    }
                }))
            })
            .map_err(session_error)?;
        if header.initial_output().is_some()
            && self
                .composition
                .tools()
                .definitions()
                .iter()
                .any(|tool| tool.name() == rsi_agent_session_protocol::REPORT_RESULT_TOOL)
        {
            return Err(invalid("catalog shadows the reserved output Tool"));
        }
        let prepared = PreparedFreshSession::new(header.clone(), self.composition.clone())
            .map_err(turn_composition_error)?
            .with_baseline(self.baseline.clone())
            .map_err(turn_composition_error)?;
        Ok((prepared, root))
    }
    pub(super) async fn wait_child(&self, ordinal: u32) -> TurnResult<ProgramAgentResult> {
        let mut watch = self
            .kernel
            .inner
            .session_changes
            .session(&self.descriptor.session_id);
        loop {
            watch.mark_seen();
            let state = self.state().await?;
            let child = state
                .children
                .get(&ordinal)
                .ok_or_else(|| invalid("program child is absent"))?;
            if let Some(receipt) = &child.receipt {
                let structured = if let Some(binding) = &receipt.result {
                    Some(
                        self.kernel
                            .read_program_result(&self.descriptor, &binding.locator)
                            .await?,
                    )
                } else {
                    None
                };
                let reply = if receipt.outcome == ProgramOutcome::Completed && structured.is_none()
                {
                    let active = child
                        .activation
                        .as_ref()
                        .ok_or_else(|| invalid("completed program child has no activation"))?;
                    let message = self
                        .kernel
                        .message_status(&child.session_id, &child.message_id)
                        .await?;
                    if let MessageState::Claimed {
                        activation_id,
                        turn_id,
                        ..
                    } = message.state
                    {
                        if &activation_id != active {
                            return Err(invalid(
                                "program child message changed its initial activation",
                            ));
                        }
                        let boundary = self
                            .kernel
                            .inner
                            .store
                            .read_turn_boundary(&child.session_id, &turn_id)
                            .await
                            .map_err(turn_store_error)?;
                        completion_reply::read(
                            &self.kernel.inner,
                            &child.session_id,
                            &turn_id,
                            boundary
                                .terminal()
                                .ok_or_else(|| invalid("program child has no terminal"))?
                                .seq(),
                        )
                        .await
                    } else {
                        None
                    }
                } else {
                    None
                };
                return Ok(ProgramAgentResult {
                    receipt: receipt.clone(),
                    structured,
                    reply,
                });
            }
            tokio::select! {()=watch.changed()=>{},()=self.cancellation.cancelled()=>{self.cancel_children().await?;return Err(TurnError::Cancelled);}}
        }
    }
}
impl AgentKernel {
    pub(super) async fn discard_program_branch_pending(
        &self,
        root: &SessionId,
    ) -> TurnResult<bool> {
        let mut sessions = descendant_session_ids(&self.inner.store, root).await?;
        sessions.push(root.clone());
        self.discard_program_pending(&sessions).await
    }
    pub(super) async fn discard_program_pending(&self, sessions: &[SessionId]) -> TurnResult<bool> {
        let mut discarded = false;
        for session in sessions {
            // Metadata enumeration is bounded by the mailbox count, not its first
            // body page; a large human message cannot hide a later automatic one.
            let snapshot = self
                .inner
                .store
                .inspect_session(session)
                .await
                .map_err(turn_store_error)?;
            snapshot.validate().map_err(turn_store_error)?;
            for pending in snapshot.pending {
                let entry = self
                    .inner
                    .store
                    .read_agent_message(session, &pending.message_id)
                    .await
                    .map_err(turn_store_error)?
                    .ok_or_else(|| invalid("pending program branch message is absent"))?;
                if matches!(entry.state, StoreAgentMessageState::Pending)
                    && !matches!(entry.message.source, AgentMessageSource::Human)
                {
                    discarded = true;
                    self.cancel_target(session, CancelTarget::Message(pending.message_id), None)
                        .await?;
                }
            }
        }
        Ok(discarded)
    }
    pub(crate) async fn program_child_claim_append(
        &self,
        header: &SessionHeader,
        message: &MessageId,
        activation: &ActivationId,
    ) -> TurnResult<Option<AtomicSessionAppend>> {
        let Some(ExecutionOwner::ProgramRun {
            session_id,
            run_id,
            ordinal,
        }) = header.execution_owner()
        else {
            return Ok(None);
        };
        let state = self.read_program_state(session_id, run_id).await?;
        let child = state
            .children
            .get(ordinal)
            .ok_or_else(|| invalid("program child admission is absent"))?;
        if &child.message_id != message {
            return Ok(None);
        }
        if &child.session_id != header.session_id() || child.receipt.is_some() {
            return Err(invalid("program child initial input is retired"));
        }
        let run = self
            .inner
            .programs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .and_then(Weak::upgrade)
            .filter(|run| &run.descriptor.run_id == run_id)
            .ok_or(TurnError::StaleClaim)?;
        run.active().await?;
        self.program_append(
            session_id,
            run_id,
            ProgramRunEvent::ChildStarted {
                ordinal: *ordinal,
                activation_id: activation.clone(),
            },
        )
        .await
        .map(Some)
    }
    pub(crate) async fn program_completion_append(
        &self,
        header: &SessionHeader,
        activation: &ActivationId,
        outcome: &TurnOutcome,
        result: Option<&rsi_agent_session_protocol::AgentResultRef>,
    ) -> TurnResult<CompletionRoute> {
        let Some(ExecutionOwner::ProgramRun {
            session_id,
            run_id,
            ordinal,
        }) = header.execution_owner()
        else {
            return Ok(CompletionRoute::NotOwned);
        };
        let state = self.read_program_state(session_id, run_id).await?;
        let child = state
            .children
            .get(ordinal)
            .ok_or_else(|| invalid("program completion has no child admission"))?;
        if child.activation.as_ref() != Some(activation) {
            return Ok(CompletionRoute::NotOwned);
        }
        if state.outcome == Some(ProgramOutcome::Interrupted) {
            return Ok(CompletionRoute::Interrupted);
        }
        let outcome = match outcome {
            TurnOutcome::Completed => ProgramOutcome::Completed,
            TurnOutcome::Cancelled => ProgramOutcome::Cancelled,
            TurnOutcome::Interrupted { .. } => ProgramOutcome::Interrupted,
            TurnOutcome::Failed { code, message }
            | TurnOutcome::PartialFailed { code, message, .. } => ProgramOutcome::Failed {
                code: code.clone(),
                message: message.clone(),
            },
            TurnOutcome::BudgetExceeded { .. } => {
                let rsi_agent_session_protocol::ActivationOutcome::Failed { code, message } =
                    observation::activation_outcome(outcome, None)
                else {
                    unreachable!()
                };
                ProgramOutcome::Failed { code, message }
            }
        };
        let receipt = ProgramChildReceipt {
            ordinal: *ordinal,
            child_session_id: header.session_id().clone(),
            activation_id: Some(activation.clone()),
            outcome,
            result: result.map(|result| ProgramResultBinding {
                locator: result.locator(),
                schema_sha256: result.summary.schema_sha256.clone(),
                value_sha256: result.summary.value_sha256.clone(),
            }),
        };
        self.program_append(
            session_id,
            run_id,
            ProgramRunEvent::ChildSettled { receipt },
        )
        .await
        .map(|append| CompletionRoute::Append(Box::new(append)))
    }
    #[allow(clippy::too_many_lines)] // Correlated initial-message discard and its exclusive receipt must share admission and commit.
    pub(crate) async fn cancel_program_message(
        &self,
        header: &SessionHeader,
        message: &MessageId,
        outcome: ProgramOutcome,
    ) -> TurnResult<Option<CancelResult>> {
        let Some(ExecutionOwner::ProgramRun {
            session_id,
            run_id,
            ordinal,
        }) = header.execution_owner()
        else {
            return Ok(None);
        };
        let admission = self
            .inner
            .submission_admission
            .acquire_pair(header.session_id(), Some(session_id))
            .await?;
        let state = self.read_program_state(session_id, run_id).await?;
        let child = state
            .children
            .get(ordinal)
            .ok_or_else(|| invalid("program child admission is absent"))?;
        if &child.message_id != message {
            return Ok(None);
        }
        if child.receipt.is_some() {
            return Ok(Some(CancelResult {
                accepted: false,
                already_terminal: true,
            }));
        }
        let entry = self
            .inner
            .store
            .read_agent_message(header.session_id(), message)
            .await
            .map_err(turn_store_error)?
            .ok_or_else(|| invalid("program initial message is absent"))?;
        if let StoreAgentMessageState::Claimed { turn_id, .. } = entry.state {
            drop(admission);
            return self
                .cancel(header.session_id(), &turn_id, None)
                .await
                .map(Some);
        }
        if !matches!(entry.state, StoreAgentMessageState::Pending) {
            return Err(invalid(
                "program message closed without its exclusive receipt",
            ));
        }
        self.fence_pending_terminal(header.session_id()).await?;
        let tail = self
            .inner
            .store
            .read_watermarks(header.session_id())
            .await
            .map_err(turn_store_error)?;
        let receipt = ProgramChildReceipt {
            ordinal: *ordinal,
            child_session_id: header.session_id().clone(),
            activation_id: None,
            outcome,
            result: None,
        };
        let parent = self
            .program_append(
                session_id,
                run_id,
                ProgramRunEvent::ChildSettled { receipt },
            )
            .await?;
        let child = AtomicSessionAppend {
            session_id: header.session_id().clone(),
            expected_fact_seq: tail.durable_fact_seq,
            expected_control_seq: tail.durable_control_seq,
            header: None,
            facts: vec![],
            controls: vec![
                AgentControlRecord::new(
                    tail.durable_control_seq + 1,
                    self.inner.clock.now_ms().max(1),
                    AgentControlRecordBody::MessageDiscarded {
                        message_id: message.clone(),
                        reason: MessageDiscardReason::Cancelled,
                    },
                )
                .map_err(session_error)?,
            ],
        };
        let kernel = self.clone();
        self.owned_commit(async move {
            let _admission = admission;
            kernel
                .commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
                    sessions: vec![parent, child],
                    required_active_activations: vec![],
                    quiescent_descendants_of: None,
                })
                .await?
                .map_err(turn_store_error)?;
            Ok(Some(CancelResult {
                accepted: true,
                already_terminal: false,
            }))
        })
        .await
    }
}

impl AgentKernel {
    pub(crate) async fn program_result_reference(
        &self,
        header: &SessionHeader,
        locator: &rsi_agent_session_protocol::AgentResultLocator,
    ) -> TurnResult<rsi_agent_session_protocol::AgentResultRef> {
        let Some(ExecutionOwner::ProgramRun {
            session_id,
            run_id,
            ordinal,
        }) = header.execution_owner()
        else {
            return Err(invalid("result has no program execution owner"));
        };
        let state = self.read_program_state(session_id, run_id).await?;
        let receipt = state
            .children
            .get(ordinal)
            .and_then(|child| child.receipt.as_ref())
            .ok_or_else(|| invalid("program child has no exclusive completion receipt"))?;
        let binding = receipt
            .result
            .as_ref()
            .filter(|binding| {
                &binding.locator == locator && receipt.outcome == ProgramOutcome::Completed
            })
            .ok_or_else(|| invalid("program output differs from exact initial completion"))?;
        let page = read_facts_bounded(&self.inner, header.session_id(), locator.fact_seq - 1, 1)
            .await
            .map_err(turn_store_error)?;
        let fact = page
            .facts
            .first()
            .filter(|fact| fact.seq() == locator.fact_seq)
            .ok_or_else(|| invalid("program result Fact is absent"))?;
        let SessionFactBody::ToolResult {
            conclusion: Some(conclusion),
            ..
        } = fact.body()
        else {
            return Err(invalid("program result is not an accepted conclusion"));
        };
        let summary = conclusion
            .structured
            .as_ref()
            .filter(|summary| {
                summary.schema_sha256 == binding.schema_sha256
                    && summary.value_sha256 == binding.value_sha256
            })
            .ok_or_else(|| invalid("program result digests differ from receipt"))?;
        Ok(rsi_agent_session_protocol::AgentResultRef {
            child_session_id: locator.child_session_id.clone(),
            activation_id: locator.activation_id.clone(),
            turn_id: locator.turn_id.clone(),
            fact_seq: locator.fact_seq,
            summary: summary.clone(),
        })
    }
    async fn read_program_result(
        &self,
        descriptor: &ProgramRunDescriptor,
        locator: &rsi_agent_session_protocol::AgentResultLocator,
    ) -> TurnResult<rsi_agent_turn_protocol::AgentResult> {
        let header = read_validated_header_bounded(&self.inner, &locator.child_session_id)
            .await
            .map_err(turn_store_error)?;
        if !matches!(header.execution_owner(),Some(ExecutionOwner::ProgramRun {session_id,run_id,..}) if session_id==&descriptor.session_id && run_id==&descriptor.run_id)
        {
            return Err(invalid("program result belongs to another run"));
        }
        let reference = self.program_result_reference(&header, locator).await?;
        self.verify_structured_result(&header, locator, reference)
            .await
    }
}
