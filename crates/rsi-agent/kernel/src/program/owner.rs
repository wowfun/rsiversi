use super::*;
#[async_trait]
impl ProgramRun for LiveRun {
    fn descriptor(&self) -> &ProgramRunDescriptor {
        &self.descriptor
    }
    fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
    async fn accept(&self, caller: &AgentCallerAuthority) -> TurnResult<()> {
        let admission = self
            .kernel
            .inner
            .submission_admission
            .acquire(&self.descriptor.session_id)
            .await?;
        self.active().await?;
        self.kernel.validate_agent_caller(caller)?;
        if caller.session_id() != &self.descriptor.session_id
            || caller.turn_id() != &self.descriptor.creator_turn_id
            || caller.tool_effect_id() != Some(&self.descriptor.creator_effect_id)
        {
            return Err(invalid(
                "program acceptance differs from its prepared creator",
            ));
        }
        let source = self
            .kernel
            .admit_agent_mutation(caller, &self.cancellation)?;
        let turn = self.descriptor.creator_turn_id.clone();
        let (existing, _bytes, _lease) =
            store_reads::read(
                &self.kernel.inner,
                &self.descriptor.session_id,
                256,
                true,
                move |store, session| async move {
                    store.program_run_for_creator(&session, &turn).await
                },
            )
            .await
            .map_err(turn_store_error)?;
        if existing.is_some() {
            return Err(invalid(
                "a Program was already accepted for this creator Turn",
            ));
        }
        let append = self
            .kernel
            .program_append(
                &self.descriptor.session_id,
                &self.descriptor.run_id,
                ProgramRunEvent::Accepted {
                    descriptor: Box::new(self.descriptor.clone()),
                },
            )
            .await?;
        let kernel = self.kernel.clone();
        let script = self.script.clone();
        self.kernel
            .owned_commit(async move {
                let _admission = admission;
                let _source = source;
                kernel
                    .inner
                    .store
                    .put_cas(script)
                    .await
                    .map_err(turn_store_error)?;
                kernel.commit_program_append(append).await
            })
            .await
    }
    async fn start(&self) -> TurnResult<()> {
        self.record_live_event(ProgramRunEvent::Started).await
    }
    async fn detach(&self) -> TurnResult<()> {
        self.record_live_event(ProgramRunEvent::Detached).await
    }
    async fn cancel_from_creator(&self) -> TurnResult<bool> {
        {
            let admission = self
                .kernel
                .inner
                .submission_admission
                .acquire(&self.descriptor.session_id)
                .await?;
            let state = self.state().await?;
            if state.detached {
                return Ok(false);
            }
            self.cancellation.cancel();
            if state.outcome.is_some() {
                return Ok(true);
            }
            if !state.cancelling {
                self.append(ProgramRunEvent::CancellationRequested, admission)
                    .await?;
            }
        }
        self.cancel_children().await?;
        Ok(true)
    }
    async fn cancel(&self) -> TurnResult<()> {
        self.request_cancellation().await?;
        self.cancel_children().await
    }
    async fn agent(&self, request: ProgramAgentRequest) -> TurnResult<ProgramAgentResult> {
        let ordinal = self.admit_child(request).await?;
        self.wait_child(ordinal).await
    }
    async fn progress(&self, phase: Option<String>, message: String) -> TurnResult<()> {
        self.record_live_event(ProgramRunEvent::Progress { phase, message })
            .await
    }
    async fn finish(
        &self,
        mut outcome: ProgramOutcome,
        value: Option<serde_json::Value>,
    ) -> TurnResult<ProgramOutcome> {
        let mut cancellation_drained = false;
        if outcome != ProgramOutcome::Completed || self.cancellation.is_cancelled() {
            self.request_cancellation().await?;
            self.drain_children().await?;
            cancellation_drained = true;
            if outcome == ProgramOutcome::Completed {
                outcome = ProgramOutcome::Cancelled;
            }
        }
        if let Some(outcome) = self.wait_for_child_receipts(cancellation_drained).await? {
            return Ok(outcome);
        }
        let admission = self
            .kernel
            .inner
            .submission_admission
            .acquire(&self.descriptor.session_id)
            .await?;
        let state = self.state().await?;
        if let Some(outcome) = state.outcome.clone() {
            return Ok(outcome);
        }
        if (state.cancelling || self.cancellation.is_cancelled())
            && outcome == ProgramOutcome::Completed
        {
            outcome = ProgramOutcome::Cancelled;
        }
        let bytes = if outcome == ProgramOutcome::Completed {
            if let Some(value) = &value {
                let bytes = serde_json::to_vec(&value).map_err(session_error)?;
                if bytes.len() > rsi_agent_session_protocol::MAXIMUM_PROGRAM_RESULT_BYTES {
                    return Err(invalid("program result exceeds 256 KiB"));
                }
                Some(Arc::<[u8]>::from(bytes))
            } else {
                None
            }
        } else {
            None
        };
        let result = bytes.as_ref().map(|bytes| ProgramBlob {
            sha256: format!("{:x}", Sha256::digest(bytes)),
            bytes: bytes.len() as u64,
        });
        let append = self
            .terminal_append(
                &state,
                outcome.clone(),
                result,
                value
                    .as_ref()
                    .filter(|_| outcome == ProgramOutcome::Completed),
            )
            .await?;
        let kernel = self.kernel.clone();
        let session = self.descriptor.session_id.clone();
        let run_id = self.descriptor.run_id.clone();
        let cancellation = self.cancellation.clone();
        self.kernel
            .owned_commit(async move {
                let _admission = admission;
                if let Some(bytes) = bytes {
                    kernel
                        .inner
                        .store
                        .put_cas(bytes)
                        .await
                        .map_err(turn_store_error)?;
                }
                kernel.commit_program_append(append).await?;
                cancellation.cancel();
                let mut registry = kernel
                    .inner
                    .programs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if registry
                    .get(&session)
                    .and_then(Weak::upgrade)
                    .is_some_and(|run| run.run_id() == &run_id)
                {
                    registry.remove(&session);
                }
                drop(registry);
                kernel.request_ready_scan();
                Ok(outcome)
            })
            .await
    }
}
impl LiveRun {
    async fn wait_for_child_receipts(
        &self,
        mut cancellation_drained: bool,
    ) -> TurnResult<Option<ProgramOutcome>> {
        let mut watch = self
            .kernel
            .inner
            .session_changes
            .session(&self.descriptor.session_id);
        loop {
            watch.mark_seen();
            let state = self.state().await?;
            if let Some(outcome) = state.outcome.clone() {
                return Ok(Some(outcome));
            }
            if state.children.values().all(|child| child.receipt.is_some()) {
                return Ok(None);
            }
            tokio::select! {
                () = self.cancellation.cancelled(), if !cancellation_drained => {
                    self.request_cancellation().await?;
                    self.drain_children().await?;
                    cancellation_drained = true;
                }
                () = watch.changed() => {},
                () = tokio::time::sleep(Duration::from_secs(5)) => {},
            }
        }
    }
    async fn request_cancellation(&self) -> TurnResult<()> {
        self.cancellation.cancel();
        {
            let admission = self
                .kernel
                .inner
                .submission_admission
                .acquire(&self.descriptor.session_id)
                .await?;
            let state = self.state().await?;
            if state.outcome.is_some() {
                return Ok(());
            }
            if !state.cancelling {
                self.append(ProgramRunEvent::CancellationRequested, admission)
                    .await?;
            }
        }
        Ok(())
    }
    async fn record_live_event(&self, event: ProgramRunEvent) -> TurnResult<()> {
        let admission = self
            .kernel
            .inner
            .submission_admission
            .acquire(&self.descriptor.session_id)
            .await?;
        self.active().await?;
        if matches!(event, ProgramRunEvent::Started | ProgramRunEvent::Detached)
            && (self.creator_cancellation.is_cancelled() || self.turn_cancellation.is_cancelled())
        {
            self.cancellation.cancel();
            self.append(ProgramRunEvent::CancellationRequested, admission)
                .await?;
            return Err(TurnError::Cancelled);
        }
        let append = self
            .kernel
            .program_append(&self.descriptor.session_id, &self.descriptor.run_id, event)
            .await?;
        let kernel = self.kernel.clone();
        self.kernel
            .owned_commit(async move {
                let _admission = admission;
                kernel.commit_program_append(append).await
            })
            .await
    }
    pub(super) async fn cancel_children(&self) -> TurnResult<()> {
        tokio::time::timeout(DURABILITY_WAIT_TIMEOUT, self.drain_children())
            .await
            .map_err(|_| {
                TurnError::Flush(
                    "program source mutations did not drain before cancellation deadline".into(),
                )
            })?
    }
    async fn drain_children(&self) -> TurnResult<()> {
        for child in self
            .state()
            .await?
            .children
            .values()
            .filter(|child| child.receipt.is_none())
        {
            self.kernel
                .cancel_target(
                    &child.session_id,
                    CancelTarget::Message(child.message_id.clone()),
                    None,
                )
                .await?;
            let mut branch =
                descendant_session_ids(&self.kernel.inner.store, &child.session_id).await?;
            branch.push(child.session_id.clone());
            loop {
                for session in &branch {
                    self.kernel.cancel_open_descendant_turns(session).await?;
                    self.kernel.drain_cancelled_session_mutations(session).await;
                }
                let discarded = self.kernel.discard_program_pending(&branch).await?;
                let mut latest =
                    descendant_session_ids(&self.kernel.inner.store, &child.session_id).await?;
                latest.push(child.session_id.clone());
                if !discarded
                    && latest == branch
                    && !branch
                        .iter()
                        .any(|id| self.kernel.cancelled_session_has_sources(id))
                {
                    break;
                }
                branch = latest;
            }
        }
        Ok(())
    }
}
