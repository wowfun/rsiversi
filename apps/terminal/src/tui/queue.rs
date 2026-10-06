//! Pending-only queue actions and a modal editor that leaves the composer untouched.
use super::*;
use rsi_agent_session_protocol::{
    AgentMessage, AgentMessageContent, QueueMutation, QueueMutationOutcome, QueueMutationReceipt,
    QueueMutationRequest, QueueOperationId,
};
use rsi_agent_store_protocol::StorePendingMessage;
use std::sync::atomic::{AtomicBool, Ordering};
use termina::event::{KeyCode, Modifiers};

pub(super) struct Edit {
    pub label: String,
    pub editor: editor::Editor,
    message: Arc<AgentMessage>,
    selected: StorePendingMessage,
    index: usize,
}
#[derive(Clone)]
pub(super) struct Pending {
    owner: Arc<dyn SessionHandle>,
    header: String,
    request: QueueMutationRequest,
    busy: Arc<AtomicBool>,
    rejected: bool,
    session: rsi_agent_session_protocol::SessionId,
}
struct Working(Arc<AtomicBool>);
impl Drop for Working {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Client {
    pub(super) fn queue_message_menu(&mut self, message: StorePendingMessage) {
        if self.state.header.protection().is_some() {
            let handle = self.handle.clone();
            self.spawn_detail(async move {
                read(|| handle.read_message(&message.message_id, message.accepted_control_seq))
                    .await
                    .map(Update::Message)
            });
            return;
        }
        let item = rsi_client::QueueItem::new(&message, self.presented_turn.as_ref());
        let mut items = vec![(
            item.read_label.into(),
            if item.editable {
                Action::EditQueue(message.clone())
            } else {
                Action::ReadQueue(message.clone())
            },
        )];
        if let Some(label) = item.withdraw_label {
            items.push((
                label.into(),
                Action::MutateQueue(message.clone(), QueueMutation::Withdraw),
            ));
        }
        if let (Some(turn), Some(label)) = (item.convert_turn, item.convert_label) {
            match generated_cli_message_id() {
                Ok(id) => items.push((
                    format!("{label} · displayed Turn {turn}"),
                    Action::MutateQueue(
                        message.clone(),
                        QueueMutation::ConvertToSteer {
                            new_message_id: id,
                            expected_turn_id: turn.clone(),
                        },
                    ),
                )),
                Err(problem) => self.state.notice(problem.to_string()),
            }
        }
        self.state.menu = Some(Menu {
            title: format!(
                "{} · {:?} · {}",
                message.message_id, message.source_kind, item.delivery_label
            ),
            selected: 0,
            items,
        });
        let handle = self.handle.clone();
        self.spawn_detail(async move {
            read(|| handle.read_message(&message.message_id, message.accepted_control_seq))
                .await
                .map(Update::Message)
        });
    }
    pub(super) fn queue_content(&mut self, selected: &StorePendingMessage, message: AgentMessage) {
        if self.state.header.protection().is_some() {
            return;
        }
        let message = Arc::new(message);
        let mut items = message
            .content
            .iter()
            .enumerate()
            .filter(|&(_, content)| matches!(content, AgentMessageContent::Text { .. }))
            .map(|(index, _)| {
                (
                    format!("Edit text block {}", index + 1),
                    Action::EditQueueBlock(selected.clone(), message.clone(), index),
                )
            })
            .collect::<Vec<_>>();
        if message.content.len() < rsi_agent_session_protocol::MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS
        {
            items.push((
                "Append text block".into(),
                Action::EditQueueBlock(selected.clone(), message.clone(), message.content.len()),
            ));
        }
        self.state.detail = None;
        self.state.menu = Some(Menu {
            title: "Edit queued content · images and references retained".into(),
            items,
            selected: 0,
        });
    }
    pub(super) fn edit_queue_block(
        &mut self,
        selected: StorePendingMessage,
        message: Arc<AgentMessage>,
        index: usize,
    ) {
        if self.state.header.protection().is_some() {
            return;
        }
        let text = match message.content.get(index) {
            Some(AgentMessageContent::Text { text }) => text.clone(),
            None if index == message.content.len() => String::new(),
            _ => return,
        };
        self.state.detail = None;
        self.state.menu = None;
        self.state.queue_edit = Some(Edit {
            label: format!("Queued input · text {} · other content retained", index + 1),
            editor: editor::Editor::with_text(
                text,
                rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES,
            ),
            message,
            selected,
            index,
        });
    }
    pub(super) fn queue_key(&mut self, key: termina::event::KeyEvent) -> bool {
        if self.state.queue_edit.is_none() {
            return false;
        }
        let save = key.code == KeyCode::Enter && !key.modifiers.contains(Modifiers::SHIFT)
            || key.code == KeyCode::Char('s') && key.modifiers.contains(Modifiers::CONTROL);
        if save {
            if self.queue_pending.is_some() {
                self.state.notice("Resolve the previous queue edit first");
                return true;
            }
            let edit = self.state.queue_edit.as_ref().expect("queue editor");
            let mut content = edit.message.content.clone();
            let text = edit.editor.text().to_owned();
            if edit.index == content.len() {
                if !text.is_empty() {
                    content.push(AgentMessageContent::Text { text });
                }
            } else if text.is_empty() {
                content.remove(edit.index);
            } else {
                content[edit.index] = AgentMessageContent::Text { text };
            }
            match generated_cli_message_id() {
                Ok(id) => {
                    let selected = edit.selected.clone();
                    if self.begin_queue_mutation(
                        selected,
                        QueueMutation::Replace {
                            new_message_id: id,
                            content,
                        },
                    ) {
                        self.state.queue_edit = None;
                    }
                }
                Err(problem) => self.state.notice(problem.to_string()),
            }
        } else if let Err(problem) = self
            .state
            .queue_edit
            .as_mut()
            .expect("queue editor")
            .editor
            .key(key)
        {
            self.state.notice(problem);
        }
        true
    }
    pub(super) fn begin_queue_mutation(
        &mut self,
        selected: StorePendingMessage,
        mutation: QueueMutation,
    ) -> bool {
        if self.state.header.protection().is_some() {
            self.state
                .notice("Protected investigation Sessions are read-only");
            return false;
        }
        if self.queue_pending.is_some() {
            self.queue_result_menu();
            return false;
        }
        if selected.source_kind != rsi_agent_session_protocol::AgentMessageSourceKind::Human {
            self.state.notice("Only Human pending inputs are editable");
            return false;
        }
        let prepared = (|| {
            let operation_id = QueueOperationId::new(format!(
                "queue-{}",
                generated_cli_message_id().map_err(error)?
            ))
            .map_err(error)?;
            let request = QueueMutationRequest {
                operation_id,
                slot_id: selected.queue_slot.id,
                expected_message_id: selected.message_id,
                mutation,
            };
            request.validate().map_err(error)?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(Pending {
                owner: self.handle.clone(),
                header: self.state.header.fingerprint().map_err(error)?,
                request,
                busy: Arc::new(AtomicBool::new(false)),
                rejected: false,
                session: self.state.header.session_id().clone(),
            })
        })();
        match prepared {
            Ok(pending) => {
                self.queue_pending = Some(pending);
                self.run_queue(false, false);
                true
            }
            Err(problem) => {
                self.state.notice(problem.to_string());
                false
            }
        }
    }
    pub(super) fn queue_result_menu(&mut self) {
        let Some(pending) = &self.queue_pending else {
            self.state.info("No unresolved queue edit");
            return;
        };
        let mut items = vec![
            (
                "Check original result".into(),
                Action::ReconcileQueue(false),
            ),
            (
                "Retry identical request".into(),
                Action::ReconcileQueue(true),
            ),
        ];
        if pending.rejected {
            items.push(("Discard unadmitted queue edit".into(), Action::DiscardQueue));
        }
        self.state.menu = Some(Menu {
            title: format!(
                "Queue operation {} · {}",
                pending.request.operation_id, pending.session
            ),
            selected: 0,
            items,
        });
    }
    pub(super) fn discard_queue(&mut self) {
        if self
            .queue_pending
            .as_ref()
            .is_some_and(|pending| pending.rejected && !pending.busy.load(Ordering::Acquire))
        {
            self.queue_pending = None;
            self.state
                .info("Unadmitted queue edit discarded; composer retained");
        } else {
            self.state
                .notice("Query an unknown queue edit before discarding it");
        }
    }
    pub(super) fn run_queue(&mut self, reconcile: bool, retry: bool) {
        let Some(pending) = self.queue_pending.clone() else {
            return;
        };
        if pending.busy.swap(true, Ordering::AcqRel) {
            self.state.info("Queue operation is awaiting its receipt");
            return;
        }
        self.queue_pending
            .as_mut()
            .expect("pending queue edit")
            .rejected = false;
        let working = Working(pending.busy.clone());
        self.spawn_as(WorkKind::Queue, async move {
            let _working = working;
            let result = async {
                if pending
                    .owner
                    .header()
                    .await?
                    .fingerprint()
                    .map_err(|error| {
                        rsi_session_protocol::SessionError::Invalid(error.to_string())
                    })?
                    != pending.header
                {
                    return Err(rsi_session_protocol::SessionError::Invalid(
                        "Original queue Session Header changed".into(),
                    ));
                }
                let receipt = if reconcile {
                    rsi_client::reconcile_queue(
                        pending.owner.as_ref(),
                        pending.request.clone(),
                        retry,
                    )
                    .await?
                } else {
                    let receipt = pending.owner.mutate_queue(pending.request.clone()).await?;
                    rsi_client::validate_queue_receipt(&pending.request, &receipt)?;
                    Some(receipt)
                };
                Ok(receipt)
            }
            .await;
            Ok(Update::QueueSettled(
                pending.request.operation_id,
                result,
                !reconcile,
            ))
        });
    }
    pub(super) fn queue_settled(
        &mut self,
        operation: &QueueOperationId,
        result: rsi_session_protocol::Result<Option<QueueMutationReceipt>>,
        initial_dispatch: bool,
    ) {
        if self
            .queue_pending
            .as_ref()
            .is_none_or(|pending| &pending.request.operation_id != operation)
        {
            return;
        }
        match result {
            Ok(Some(receipt)) => {
                self.queue_pending = None;
                if let QueueMutationOutcome::Rejected { reason, .. } = receipt.outcome {
                    self.state.notice(format!(
                        "Queue edit rejected: {reason:?}; reopen the current slot"
                    ));
                }
                self.inspect();
            }
            Ok(None) => self
                .state
                .notice("No queue receipt yet; Pending queue edit can retry the identical request"),
            Err(problem) => {
                if initial_dispatch
                    && matches!(
                        problem,
                        rsi_session_protocol::SessionError::Invalid(_)
                            | rsi_session_protocol::SessionError::NotFound(_)
                            | rsi_session_protocol::SessionError::QueueOperationConflict
                    )
                {
                    self.queue_pending
                        .as_mut()
                        .expect("pending queue edit")
                        .rejected = true;
                }
                self.state.notice(format!(
                    "Queue edit unresolved: {problem}; use Pending queue edit"
                ));
            }
        }
    }
}
