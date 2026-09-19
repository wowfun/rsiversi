//! Controller epochs and input receipts remain authoritative in Rust.
use super::{
    Arc, Digest, InputReceipt, InputState, Operation, Ordering, Phase, PtyError, RECEIPTS, Record,
    Reply, Result, Scope, Sha256, Term, TermState, Terminal, lock, unavailable,
};
impl Scope {
    pub(super) async fn dispatch(&self, operation: Operation) -> Result<Reply> {
        operation.validate()?;
        if self.shared.stopped.load(Ordering::Acquire) || lock(&self.inner).retired {
            return Err(unavailable());
        }
        match operation {
            Operation::List => Ok(Reply::List(
                lock(&self.inner)
                    .terminals
                    .values()
                    .map(|term| lock(&term.inner).status.clone())
                    .collect(),
            )),
            Operation::Attach { terminal } => Ok(Reply::Attached(self.term(&terminal)?.attach()?)),
            Operation::Read {
                terminal,
                attachment,
                stream_epoch,
                cursor,
            } => Ok(Reply::Output(
                self.term(&terminal)?
                    .read(&attachment, stream_epoch, cursor)
                    .await?,
            )),
            Operation::Input {
                terminal,
                attachment,
                epoch,
                sequence,
                bytes,
            } => Ok(Reply::Input(
                self.term(&terminal)?
                    .input(&attachment, epoch, sequence, bytes)
                    .await?,
            )),
            Operation::Receipt {
                terminal,
                epoch,
                sequence,
            } => Ok(Reply::Input(self.term(&terminal)?.receipt(epoch, sequence))),
            Operation::Takeover {
                terminal,
                attachment,
            } => Ok(Reply::Terminal(
                self.term(&terminal)?.takeover(&attachment)?,
            )),
            Operation::Resize {
                terminal,
                attachment,
                epoch,
                size,
            } => Ok(Reply::Terminal(self.term(&terminal)?.resize(
                &attachment,
                epoch,
                size,
            )?)),
            Operation::Detach {
                terminal,
                attachment,
                epoch,
            } => {
                self.term(&terminal)?.detach(&attachment, epoch)?;
                Ok(Reply::Done)
            }
            Operation::Close { terminal } => {
                let _closing = self.closing.lock().await;
                let term = lock(&self.inner).terminals.get(&terminal).cloned();
                if let Some(term) = term {
                    term.process.terminate();
                    let result = term.closed().await;
                    lock(&self.inner).terminals.remove(&terminal);
                    result?;
                }
                Ok(Reply::Done)
            }
            Operation::CloseAll => {
                self.close_all(false).await?;
                Ok(Reply::Done)
            }
        }
    }
}
impl Term {
    pub(super) fn controller(state: &TermState, attachment: &str, epoch: u64) -> Result<()> {
        if state.status.controller.as_deref() != Some(attachment)
            || state.status.controller_epoch != epoch
            || !state.followers.contains_key(attachment)
        {
            return Err(PtyError::StaleController);
        }
        if !matches!(state.status.phase, Phase::Running) {
            return Err(unavailable());
        }
        Ok(())
    }
    fn takeover(&self, attachment: &str) -> Result<Terminal> {
        let mut state = lock(&self.inner);
        if !state.followers.contains_key(attachment)
            || !matches!(state.status.phase, Phase::Running)
        {
            return Err(unavailable());
        }
        if state.inflight {
            return Err(PtyError::Capacity);
        }
        state.status.controller_epoch = state
            .status
            .controller_epoch
            .checked_add(1)
            .ok_or(PtyError::Capacity)?;
        state.status.controller = Some(attachment.into());
        state.next_input = 1;
        let status = state.status.clone();
        drop(state);
        self.changed.notify_waiters();
        Ok(status)
    }
    fn detach(&self, attachment: &str, epoch: u64) -> Result<()> {
        let mut state = lock(&self.inner);
        // A delayed detach from an older controller epoch must not remove a newer owner.
        if state.status.controller.as_deref() == Some(attachment) {
            if state.status.controller_epoch != epoch {
                return Err(PtyError::StaleController);
            }
            state.status.controller_epoch = state
                .status
                .controller_epoch
                .checked_add(1)
                .ok_or(PtyError::Capacity)?;
            state.status.controller = None;
            state.next_input = 1;
        }
        state.followers.remove(attachment);
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }
    fn receipt(&self, epoch: u64, sequence: u64) -> InputReceipt {
        let state = lock(&self.inner);
        InputReceipt {
            epoch,
            sequence,
            result: state
                .receipts
                .iter()
                .find(|record| record.epoch == epoch && record.sequence == sequence)
                .map_or(InputState::Unknown, |record| record.receipt.clone()),
        }
    }
    async fn input(
        self: Arc<Self>,
        attachment: &str,
        epoch: u64,
        sequence: u64,
        bytes: Vec<u8>,
    ) -> Result<InputReceipt> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| PtyError::Unavailable("Tokio runtime is unavailable".into()))?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        {
            let mut state = lock(&self.inner);
            Self::controller(&state, attachment, epoch)?;
            if let Some(record) = state
                .receipts
                .iter()
                .find(|record| record.epoch == epoch && record.sequence == sequence)
            {
                if record.digest != digest {
                    return Err(PtyError::InputConflict);
                }
                return Ok(InputReceipt {
                    epoch,
                    sequence,
                    result: record.receipt.clone(),
                });
            }
            if state.inflight {
                return Err(PtyError::Capacity);
            }
            if sequence != state.next_input {
                return Err(PtyError::InputConflict);
            }
            state.next_input = state.next_input.checked_add(1).ok_or(PtyError::Capacity)?;
            while state.receipts.len() >= RECEIPTS {
                state.receipts.pop_front();
            }
            state.receipts.push_back(Record {
                epoch,
                sequence,
                digest,
                receipt: InputState::Pending,
            });
            state.inflight = true;
        }
        let guard = InputGuard {
            term: self.clone(),
            epoch,
            sequence,
        };
        let (reply, wait) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let result = match guard.term.process.write(&bytes).await {
                Ok(bytes) => InputState::Accepted { bytes },
                Err(_) => InputState::Unknown,
            };
            {
                let mut state = lock(&guard.term.inner);
                if let Some(record) = state
                    .receipts
                    .iter_mut()
                    .find(|record| record.epoch == epoch && record.sequence == sequence)
                {
                    record.receipt = result.clone();
                }
            }
            let receipt = InputReceipt {
                epoch,
                sequence,
                result,
            };
            drop(guard);
            let _ = reply.send(receipt);
        });
        wait.await
            .map_err(|_| PtyError::Io("input receipt is unknown; query its sequence".into()))
    }
}
struct InputGuard {
    term: Arc<Term>,
    epoch: u64,
    sequence: u64,
}
impl Drop for InputGuard {
    fn drop(&mut self) {
        let mut state = lock(&self.term.inner);
        if let Some(record) = state
            .receipts
            .iter_mut()
            .find(|record| record.epoch == self.epoch && record.sequence == self.sequence)
            && matches!(record.receipt, InputState::Pending)
        {
            record.receipt = InputState::Unknown;
        }
        state.inflight = false;
        drop(state);
        self.term.changed.notify_waiters();
    }
}
