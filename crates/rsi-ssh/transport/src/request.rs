use crate::{
    Connection, Control, Error, Message, Result, Role, Shared,
    state::{Class, Outgoing, Pending},
};
use rsi_ssh_protocol::frame::{MAXIMUM_CONTROL_BYTES, MAXIMUM_MESSAGE_BYTES};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

/// An ordinary opaque message, or a validated reserved lifecycle operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestKind {
    Ordinary,
    Control(Control),
}

/// Helper request queues. Control delivery has priority over queued ordinary work.
#[derive(Debug)]
pub struct Incoming {
    pub(crate) shared: Arc<Shared>,
    pub(crate) ordinary: mpsc::Receiver<IncomingRequest>,
    pub(crate) controls: mpsc::Receiver<IncomingRequest>,
}
impl Incoming {
    /// Receives one request without blocking frame intake on handler work.
    pub async fn next(&mut self) -> Option<IncomingRequest> {
        tokio::select! { biased;
            () = self.shared.stop.cancelled() => None,
            request = self.controls.recv() => request,
            request = self.ordinary.recv() => request,
        }
    }
}

/// An admitted request retaining both its pending slot and complete input budget.
#[derive(Debug)]
pub struct IncomingRequest {
    pub(crate) shared: Arc<Shared>,
    pub(crate) identity: u64,
    pub(crate) kind: RequestKind,
    pub(crate) message: Message,
    pub(crate) replied: bool,
}
impl IncomingRequest {
    /// Returns the validated transport operation class.
    pub fn kind(&self) -> RequestKind {
        self.kind
    }
    /// Borrows bytes for ordinary helper-schema validation.
    pub fn payload(&self) -> &[u8] {
        self.message.as_bytes()
    }
    /// Publishes a complete reply. Failure retires the unanswered connection.
    ///
    /// # Errors
    /// Rejects an empty/oversized payload, a full outgoing budget or a retired call.
    pub fn reply(mut self, payload: Vec<u8>) -> Result<()> {
        let class = match self.kind {
            RequestKind::Ordinary => Class::Ordinary,
            RequestKind::Control(_) => Class::Control,
        };
        validate_payload(&payload, class)?;
        {
            let mut state = self.shared.lock();
            state.ensure_open()?;
            if state.inbound.get(&self.identity) != Some(&class) {
                return Err(Error::Invalid);
            }
            let message = Message::new(payload, &state.outgoing_budgets[class.index()])?;
            state.enqueue(Outgoing::new(self.identity, class, true, message));
        }
        self.replied = true;
        self.shared.changed.notify_waiters();
        Ok(())
    }
}
impl Drop for IncomingRequest {
    fn drop(&mut self) {
        if !self.replied {
            self.shared.close();
        }
    }
}

pub(crate) fn validate_payload(bytes: &[u8], class: Class) -> Result<()> {
    if bytes.is_empty() {
        return Err(Error::Invalid);
    }
    let maximum = match class {
        Class::Ordinary => MAXIMUM_MESSAGE_BYTES,
        Class::Control => MAXIMUM_CONTROL_BYTES,
    };
    if bytes.len() > maximum {
        return Err(Error::Capacity);
    }
    Ok(())
}
impl Connection {
    /// Admits a complete ordinary request. A dropped waiter does not free capacity.
    ///
    /// # Errors
    /// Rejects invalid payloads/roles, full budgets and retired connections.
    /// A dispatched request without its complete reply returns `OutcomeUnknown`.
    pub async fn call(&self, payload: Vec<u8>) -> Result<Message> {
        self.request(payload, Class::Ordinary).await
    }
    /// Admits only a closed lifecycle operation to reserved capacity.
    ///
    /// # Errors
    /// Rejects invalid control values and the same lifecycle failures as [`Self::call`],
    /// using the independent reserved capacity.
    pub async fn control(&self, control: Control) -> Result<Message> {
        control.validate()?;
        let bytes = serde_json::to_vec(&control).map_err(|_| Error::Invalid)?;
        self.request(bytes, Class::Control).await
    }
    async fn request(&self, payload: Vec<u8>, class: Class) -> Result<Message> {
        validate_payload(&payload, class)?;
        let shared = self.shared();
        if shared.role != Role::Client {
            return Err(Error::Invalid);
        }
        let receive = {
            let mut state = shared.lock();
            state.ensure_open()?;
            if state
                .pending
                .values()
                .filter(|pending| pending.class == class)
                .count()
                >= class.limit()
            {
                return Err(Error::Capacity);
            }
            let next = state
                .next_request
                .checked_add(1)
                .filter(|next| *next <= u64::MAX >> 1)
                .ok_or(Error::Capacity)?;
            let message = Message::new(payload, &state.outgoing_budgets[class.index()])?;
            let identity = (state.next_request << 1) | u64::from(class == Class::Control);
            let (reply, receive) = oneshot::channel();
            state.next_request = next;
            state.pending.insert(
                identity,
                Pending {
                    deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(30),
                    class,
                    dispatched: false,
                    final_fragment_selected: false,
                    reply,
                },
            );
            state.enqueue(Outgoing::new(identity, class, false, message));
            receive
        };
        shared.changed.notify_waiters();
        shared.requests_changed.notify_one();
        receive.await.map_err(|_| Error::OutcomeUnknown)?
    }
}
