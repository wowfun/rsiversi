use super::{
    Operation, PtyError, Reply, Result, SessionId, TerminalLaunch, Terminals, unavailable,
};
use rsi_session_protocol::terminal::{Attachment, Phase, Size};

pub(super) type Identity = (SessionId, Option<rsi_api_protocol::DeviceId>, String);

#[derive(Debug)]
pub(super) struct Receipt {
    size: Size,
    attachment: Option<Attachment>,
}
#[derive(Debug)]
pub(super) struct Held {
    pub(super) scope: std::sync::Arc<dyn rsi_pty_protocol::PtyScope>,
    pub(super) attachment: Attachment,
}
impl rsi_session_protocol::terminal::AttachmentLease for Held {
    fn receipt(&self) -> &Attachment {
        &self.attachment
    }
}
impl Drop for Held {
    fn drop(&mut self) {
        self.scope
            .release_attachment(&self.attachment.terminal.id, &self.attachment.id);
    }
}

impl Terminals {
    pub(super) async fn bindings(
        &self,
        session: &SessionId,
        owner: Option<&rsi_api_protocol::DeviceId>,
    ) -> Result<Vec<rsi_session_protocol::terminal::Binding>> {
        let Reply::List(live) = self.operate(session, Operation::List, None).await? else {
            return Err(PtyError::OutcomeUnknown.into());
        };
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(state
            .creations
            .iter()
            .filter(|((id, principal, _), _)| id == session && principal.as_ref() == owner)
            .filter_map(|((_, _, key), receipt)| {
                let id = &receipt.attachment.as_ref()?.terminal.id;
                let terminal = live.iter().find(|row| &row.id == id)?.clone();
                Some(rsi_session_protocol::terminal::Binding {
                    key: key.clone(),
                    terminal,
                })
            })
            .collect())
    }
    pub(super) async fn create_keyed(
        &self,
        identity: Identity,
        size: Size,
        launch: TerminalLaunch,
    ) -> Result<Reply> {
        let known = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .creations
            .contains_key(&identity);
        let Some(scope) = self.scope(&identity.0, !known)? else {
            return Err(unavailable("terminal creation key has retired"));
        };
        let _mutation = scope.entry.mutation.lock().await;
        let prior = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(row) = state.creations.get(&identity) {
                if row.size != size {
                    return Err(PtyError::Invalid(
                        "terminal creation key has different initial dimensions".into(),
                    )
                    .into());
                }
                Some(row.attachment.clone().ok_or(PtyError::OutcomeUnknown)?)
            } else {
                if state.creations.len() >= 8192 {
                    return Err(PtyError::Capacity.into());
                }
                state.creations.insert(
                    identity.clone(),
                    Receipt {
                        size,
                        attachment: None,
                    },
                );
                None
            }
        };
        if let Some(issued) = prior {
            let Reply::List(rows) = scope.entry.scope.execute(Operation::List).await? else {
                return Err(PtyError::OutcomeUnknown.into());
            };
            if rows
                .iter()
                .any(|row| row.id == issued.terminal.id && matches!(row.phase, Phase::Running))
            {
                return Ok(Reply::Attached(issued));
            }
            return Err(unavailable("terminal creation key has retired"));
        }
        let result = match launch {
            TerminalLaunch::Native(spec) => scope.entry.scope.create(spec).await,
            TerminalLaunch::Execution(spec) => scope.entry.scope.create_execution(spec).await,
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match result {
            Ok(issued) => {
                state
                    .creations
                    .get_mut(&identity)
                    .expect("reserved terminal creation")
                    .attachment = Some(issued.clone());
                Ok(Reply::Attached(issued))
            }
            Err(error) => {
                if error != PtyError::OutcomeUnknown {
                    state.creations.remove(&identity);
                }
                Err(error.into())
            }
        }
    }
}
