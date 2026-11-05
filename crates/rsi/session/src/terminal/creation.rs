use super::{
    Operation, PtyError, Reply, Result, SessionError, SessionId, TerminalLaunch, Terminals,
};
use rsi_session_protocol::terminal::{Attachment, Phase, Size};

pub(super) type Identity = (SessionId, Option<rsi_api_protocol::DeviceId>, String);

#[derive(Debug)]
pub(super) enum Receipt {
    Reserved(Size),
    Issued { size: Size, attachment: Attachment },
    ClosingAllocation,
    Retired { cleanup: Option<String> },
}
impl Receipt {
    fn retired(&self) -> bool {
        matches!(self, Self::ClosingAllocation | Self::Retired { .. })
    }
    fn retire(&mut self) {
        match self {
            Self::Reserved(_) => *self = Self::ClosingAllocation,
            Self::Issued { attachment, .. } => {
                *self = Self::Retired {
                    cleanup: Some(std::mem::take(&mut attachment.terminal.id)),
                };
            }
            Self::ClosingAllocation | Self::Retired { .. } => {}
        }
    }
    fn terminal_id(&self) -> Option<&str> {
        match self {
            Self::Issued { attachment, .. } => Some(&attachment.terminal.id),
            Self::Retired { cleanup } => cleanup.as_deref(),
            Self::Reserved(_) | Self::ClosingAllocation => None,
        }
    }
}
impl super::Registry {
    fn insert_creation(&mut self, identity: Identity, receipt: Receipt) -> Result<()> {
        if self.creations.len() >= 8192 && !self.creations.contains_key(&identity) {
            return Err(PtyError::Capacity.into());
        }
        self.creations.insert(identity, receipt);
        Ok(())
    }
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
                let id = receipt.terminal_id()?;
                let terminal = live.iter().find(|row| row.id == id)?.clone();
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
        let scope = {
            let mut state = self.running()?;
            let prior = state.creations.get(&identity);
            if prior.is_some_and(Receipt::retired) {
                return Err(SessionError::TerminalKeyRetired);
            }
            let new_key = prior.is_none();
            self.scope_locked(&mut state, &identity.0, new_key)?
                .ok_or(SessionError::TerminalKeyRetired)?
        };
        let _mutation = scope.entry.mutation.lock().await;
        let prior = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match state.creations.get(&identity) {
                Some(Receipt::ClosingAllocation | Receipt::Retired { .. }) => {
                    return Err(SessionError::TerminalKeyRetired);
                }
                Some(Receipt::Reserved(initial) | Receipt::Issued { size: initial, .. })
                    if *initial != size =>
                {
                    return Err(PtyError::Invalid(
                        "terminal creation key has different initial dimensions".into(),
                    )
                    .into());
                }
                Some(Receipt::Reserved(_)) => return Err(PtyError::OutcomeUnknown.into()),
                Some(Receipt::Issued { attachment, .. }) => Some(attachment.clone()),
                None => {
                    state.insert_creation(identity.clone(), Receipt::Reserved(size))?;
                    None
                }
            }
        };
        if let Some(issued) = prior {
            let Reply::List(rows) = scope.entry.scope.execute(Operation::List).await? else {
                return Err(PtyError::OutcomeUnknown.into());
            };
            if !self
                .running()?
                .creations
                .get(&identity)
                .is_some_and(Receipt::retired)
                && rows
                    .iter()
                    .any(|row| row.id == issued.terminal.id && matches!(row.phase, Phase::Running))
            {
                return Ok(Reply::Attached(issued));
            }
            return Err(SessionError::TerminalKeyRetired);
        }
        let result = match launch {
            TerminalLaunch::Native(spec) => scope.entry.scope.create(spec).await,
            TerminalLaunch::Execution(spec) => scope.entry.scope.create_execution(spec).await,
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let receipt = state
            .creations
            .get_mut(&identity)
            .expect("allocation owns its reservation");
        match result {
            Ok(issued) if receipt.retired() => {
                *receipt = Receipt::Retired {
                    cleanup: Some(issued.terminal.id),
                };
                Err(SessionError::TerminalKeyRetired)
            }
            Ok(issued) => {
                *receipt = Receipt::Issued {
                    size,
                    attachment: issued.clone(),
                };
                Ok(Reply::Attached(issued))
            }
            Err(error) => {
                if error != PtyError::OutcomeUnknown {
                    if receipt.retired() {
                        *receipt = Receipt::Retired { cleanup: None };
                    } else {
                        state.creations.remove(&identity);
                    }
                }
                Err(error.into())
            }
        }
    }
    pub(super) async fn close_keyed(&self, identity: Identity) -> Result<Reply> {
        let scope = {
            let mut state = self.running()?;
            let scope = match state.creations.get_mut(&identity) {
                Some(Receipt::Retired { cleanup: None }) => return Ok(Reply::Done),
                Some(receipt) => {
                    receipt.retire();
                    self.scope_locked(&mut state, &identity.0, false)?
                }
                None => None,
            };
            let Some(scope) = scope else {
                state.insert_creation(identity, Receipt::Retired { cleanup: None })?;
                return Ok(Reply::Done);
            };
            scope
        };
        let _mutation = scope.entry.mutation.lock().await;
        let terminal = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match state.creations.get(&identity) {
                Some(Receipt::Retired { cleanup }) => cleanup.clone(),
                // An uncertain allocation never supplied a terminal ID.
                _ => return Err(PtyError::OutcomeUnknown.into()),
            }
        };
        if let Some(terminal) = terminal {
            scope
                .entry
                .scope
                .execute(Operation::Close { terminal })
                .await?;
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .creations
                .insert(identity, Receipt::Retired { cleanup: None });
            self.changed.notify_waiters();
        }
        Ok(Reply::Done)
    }
}
