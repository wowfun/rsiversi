use super::*;
use rsi_agent_session_protocol::{ExecutionCoordinates, ExecutionLocation};
use rsi_agent_session_protocol::{MessageAttribution, MessageDelivery};
use std::{io::ErrorKind, path::Path};

async fn available(path: &str) -> TurnResult<bool> {
    let missing = |error: std::io::Error| match error.kind() {
        ErrorKind::NotFound | ErrorKind::NotADirectory => Ok(false),
        _ => Err(TurnError::Invalid(format!(
            "working directory {path:?}: {error}"
        ))),
    };
    let canonical = match tokio::fs::canonicalize(path).await {
        Ok(canonical) => rsi_workspace_path::canonical_native_path(canonical),
        Err(error) => return missing(error),
    };
    if canonical != Path::new(path) {
        return Err(TurnError::Invalid(format!(
            "working directory {path:?} changed its canonical target"
        )));
    }
    match tokio::fs::metadata(canonical).await {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) => missing(error),
    }
}

impl AgentKernel {
    async fn directory_state(
        &self,
        header: &SessionHeader,
    ) -> TurnResult<rsi_agent_store_protocol::StoreRecentSession> {
        let store = &self.inner.store;
        let id = header.session_id();
        let _proof = store.prepare_session(id).await.map_err(turn_store_error)?;
        let cut = store.session_summary(id).await.map_err(turn_store_error)?;
        if cut.header != *header {
            return Err(TurnError::Invalid(
                "directory owner differs from its durable Header".into(),
            ));
        }
        Ok(cut)
    }

    pub(super) async fn stored_working_directory(
        &self,
        header: &SessionHeader,
    ) -> TurnResult<Option<Arc<ExecutionCoordinates>>> {
        match self.inner.store.session_summary(header.session_id()).await {
            Ok(cut) => Ok(cut
                .working_directory
                .map(|change| Arc::new(change.coordinates))),
            Err(rsi_agent_store_protocol::StoreError::NotFound(_)) => Ok(None),
            Err(error) => Err(turn_store_error(error)),
        }
    }

    pub(super) async fn current_working_directory(
        &self,
        header: &SessionHeader,
    ) -> TurnResult<Option<Arc<ExecutionCoordinates>>> {
        if let Some(directory) = lock_state(&self.inner)
            .sessions
            .get(header.session_id())
            .map(|session| session.working_directory.clone())
        {
            return Ok(directory);
        }
        self.stored_working_directory(header).await
    }

    pub(super) async fn ensure_coordinates(
        &self,
        header: &SessionHeader,
        directory: Option<Arc<ExecutionCoordinates>>,
    ) -> TurnResult<Option<Arc<ExecutionCoordinates>>> {
        if directory.is_none() && !header.is_redirected() {
            return Ok(None);
        }
        let current = directory.as_deref().unwrap_or_else(|| header.coordinates());
        if available(current.path()).await? {
            return Ok(directory);
        }
        self.change_working_directory(header, None, None)
            .await
            .map(|coordinates| Some(Arc::new(coordinates)))
    }

    pub(super) async fn change_working_directory(
        &self,
        header: &SessionHeader,
        path: Option<&str>,
        caller: Option<&AgentCallerAuthority>,
    ) -> TurnResult<ExecutionCoordinates> {
        if header.coordinates().location() != &ExecutionLocation::Local {
            return Err(TurnError::Invalid(
                "working-directory changes require local execution".into(),
            ));
        }
        let id = header.session_id();
        let admission = self.inner.submission_admission.acquire(id).await?;
        if let Some(caller) = caller {
            self.validate_agent_caller(caller)?;
        }
        let cut = self.directory_state(header).await?;
        let previous = cut.working_coordinates();
        let directory = if let Some(path) = path {
            if path.is_empty()
                || path.len() > rsi_workspace_path::MAXIMUM_HOST_PATH_BYTES
                || path.contains('\0')
            {
                return Err(TurnError::Invalid("invalid working-directory path".into()));
            }
            let resolved = tokio::fs::canonicalize(Path::new(previous.path()).join(path))
                .await
                .map_err(|e| TurnError::Invalid(format!("working directory {path:?}: {e}")))?;
            let canonical = rsi_workspace_path::canonical_native_path(resolved);
            if !tokio::fs::metadata(&canonical)
                .await
                .map_err(|e| TurnError::Invalid(e.to_string()))?
                .is_dir()
            {
                return Err(TurnError::Invalid(format!(
                    "working directory {path:?} is not a directory"
                )));
            }
            let cwd = canonical
                .into_os_string()
                .into_string()
                .map_err(|_| TurnError::Invalid("working directory is not UTF-8".into()))?;
            ExecutionCoordinates::new(ExecutionLocation::Local, cwd)
                .map_err(|e| TurnError::Invalid(e.to_string()))?
        } else {
            if available(previous.path()).await? {
                return Ok(previous.clone());
            }
            if previous == header.creation_coordinates()
                || !available(header.creation_cwd()).await?
            {
                return Err(TurnError::Invalid(format!(
                    "creation directory {:?} is unavailable",
                    header.creation_cwd()
                )));
            }
            header.creation_coordinates().clone()
        };
        if &directory == previous {
            return Ok(directory);
        }
        let mut controls = if cut.is_blank() {
            let pin = self
                .inner
                .composition
                .pin_session(header, None)
                .await
                .map_err(turn_composition_error)?;
            let fresh =
                PreparedFreshSession::new(header.clone(), pin).map_err(turn_composition_error)?;
            lifecycle::initial_controls(&fresh)?
        } else {
            Vec::new()
        };
        let seq = cut
            .durable_control_seq
            .checked_add(controls.len() as u64 + 1)
            .ok_or(TurnError::Capacity)?;
        let time = self.inner.clock.now_ms().max(1);
        controls.push(
            AgentControlRecord::new(
                seq,
                time,
                AgentControlRecordBody::WorkingDirectoryChanged {
                    cwd: directory.path().to_owned(),
                },
            )
            .map_err(|error| TurnError::Invalid(error.to_string()))?,
        );
        let has_room = if cut.is_blank() {
            true
        } else {
            let (mailbox, reserved) = tokio::try_join!(
                self.inner.store.read_agent_mailbox_summary(id),
                self.inner.store.completion_reservation_count(id),
            )
            .map_err(turn_store_error)?;
            mailbox.pending_count.saturating_add(reserved) < MAXIMUM_PENDING_AGENT_MESSAGES
        };
        if has_room {
            let notice = directory_notice(previous.path(), directory.path(), path.is_none())?;
            controls.push(
                AgentControlRecord::new(
                    seq.checked_add(1).ok_or(TurnError::Capacity)?,
                    time,
                    AgentControlRecordBody::MessageAccepted {
                        message: notice,
                        delivery: MessageDelivery::NextStep,
                        bound_turn_id: None,
                        root_session_id: agent_root_and_path(header).0,
                        target: MessageTarget::NextStep,
                        wake_required: false,
                    },
                )
                .map_err(|error| TurnError::Invalid(error.to_string()))?,
            );
        }
        let append = AtomicSessionAppend {
            session_id: id.clone(),
            expected_fact_seq: cut.durable_fact_seq,
            expected_control_seq: cut.durable_control_seq,
            header: cut.is_blank().then(|| header.clone()),
            facts: vec![],
            controls,
        };
        let kernel = self.clone();
        let caller = caller.cloned();
        self.owned_commit(async move {
            let _admission = admission;
            if let Some(caller) = &caller {
                kernel.validate_agent_caller(caller)?;
            }
            kernel
                .commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
                    sessions: vec![append],
                    required_active_activations: vec![],
                    quiescent_descendants_of: None,
                })
                .await?
                .map_err(turn_store_error)?;
            if let Some(session) = lock_state(&kernel.inner)
                .sessions
                .get_mut(cut.header.session_id())
            {
                session.working_directory = Some(Arc::new(directory.clone()));
            }
            if !has_room {
                eprintln!(
                    "working directory changed for {}; its mailbox is full",
                    cut.header.session_id()
                );
            }
            Ok(directory)
        })
        .await
    }
}

fn directory_notice(previous: &str, current: &str, recovered: bool) -> TurnResult<AgentMessage> {
    let previous = serde_json::Value::from(previous);
    let current = serde_json::Value::from(current);
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce).map_err(|error| TurnError::Invariant(error.to_string()))?;
    let id = format!("cwd-{:032x}", u128::from_le_bytes(nonce));
    Ok(AgentMessage {
        message_id: MessageId::new(&id).map_err(|error| TurnError::Invalid(error.to_string()))?,
        source: AgentMessageSource::Application {
            attribution: Some(MessageAttribution {
                id,
                source: serde_json::json!({"kind":"working-directory"}),
            }),
        },
        content: vec![AgentMessageContent::Text {
            text: if recovered {
                format!(
                    "The working directory {previous} is unavailable. The working directory is now {current}."
                )
            } else {
                format!("The working directory changed from {previous} to {current}.")
            },
        }],
        options: MessageOptions::default(),
    })
}
