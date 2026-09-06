use async_trait::async_trait;
use rsi_agent_session_protocol::SessionId;
use rsi_approval_protocol::{
    ApprovalAnswerer, ApprovalDecision, ApprovalError, ApprovalOutcome, ApprovalRequest,
};
use rsi_session::{Result as SessionResult, SessionApplicationError, SessionApprovalControl};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const MAXIMUM_PENDING_APPROVALS: usize = 1024;
const MAXIMUM_PENDING_APPROVAL_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_SETTLED_APPROVALS: usize = 1024;

/// Host-generation approval broker shared by every capable Session client.
#[derive(Clone, Debug)]
pub struct ApprovalBroker {
    state: Arc<State>,
    stopped: CancellationToken,
}

#[derive(Debug, Default)]
struct State {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    pending_bytes: usize,
    next_generation: u64,
    pending: BTreeMap<(String, String), Pending>,
    settled: BTreeMap<(String, String), ApprovalDecision>,
    settled_order: VecDeque<(String, String)>,
}

#[derive(Debug)]
struct Pending {
    encoded_len: usize,
    generation: u64,
    request: ApprovalRequest,
    answer: Option<oneshot::Sender<ApprovalOutcome>>,
}

struct PendingRegistration {
    broker: ApprovalBroker,
    key: (String, String),
    generation: u64,
}

impl Drop for PendingRegistration {
    fn drop(&mut self) {
        self.broker.remove_if_current(&self.key, self.generation);
    }
}

impl ApprovalBroker {
    /// Creates an empty accepting broker.
    pub fn new() -> Self {
        Self {
            state: Arc::new(State::default()),
            stopped: CancellationToken::new(),
        }
    }

    /// Cancels every pending request and rejects later admissions.
    pub fn stop(&self) {
        self.stopped.cancel();
        let mut inner = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.pending.clear();
        inner.pending_bytes = 0;
    }

    fn remove_if_current(&self, key: &(String, String), generation: u64) {
        let mut inner = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner
            .pending
            .get(key)
            .is_some_and(|pending| pending.generation == generation)
        {
            let pending = inner
                .pending
                .remove(key)
                .expect("current registration exists");
            inner.pending_bytes -= pending.encoded_len;
        }
    }
}

impl Default for ApprovalBroker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ApprovalAnswerer for ApprovalBroker {
    async fn answer(
        &self,
        request: ApprovalRequest,
        cancellation: CancellationToken,
    ) -> rsi_approval_protocol::Result<Option<ApprovalOutcome>> {
        request.validate()?;
        let encoded_len = request.encoded_len()?;
        if self.stopped.is_cancelled() {
            return Err(ApprovalError::Cancelled);
        }
        let key = (request.subject.session_id().to_owned(), request.id.clone());
        let (generation, receiver) = {
            let mut inner = self
                .state
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.stopped.is_cancelled() || cancellation.is_cancelled() {
                return Err(ApprovalError::Cancelled);
            }
            if inner.pending.len() >= MAXIMUM_PENDING_APPROVALS
                || encoded_len > MAXIMUM_PENDING_APPROVAL_BYTES - inner.pending_bytes
            {
                return Err(ApprovalError::Answerer(
                    "Session Host pending approval capacity is exhausted".into(),
                ));
            }
            if inner.pending.contains_key(&key) || inner.settled.contains_key(&key) {
                return Err(ApprovalError::Answerer(
                    "Session Host received a duplicate pending approval identity".into(),
                ));
            }
            inner.next_generation = inner
                .next_generation
                .checked_add(1)
                .ok_or_else(|| ApprovalError::Answerer("approval generation exhausted".into()))?;
            let generation = inner.next_generation;
            let (sender, receiver) = oneshot::channel();
            inner.pending_bytes += encoded_len;
            inner.pending.insert(
                key.clone(),
                Pending {
                    encoded_len,
                    generation,
                    request,
                    answer: Some(sender),
                },
            );
            (generation, receiver)
        };

        let _registration = PendingRegistration {
            broker: self.clone(),
            key,
            generation,
        };
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(ApprovalError::Cancelled),
            () = self.stopped.cancelled() => Err(ApprovalError::Cancelled),
            result = receiver => result.map(Some).map_err(|_| ApprovalError::Cancelled),
        }
    }
}

#[async_trait]
impl SessionApprovalControl for ApprovalBroker {
    async fn pending(&self, session_id: &SessionId) -> SessionResult<Vec<ApprovalRequest>> {
        if self.stopped.is_cancelled() {
            return Err(SessionApplicationError::ShuttingDown);
        }
        Ok(self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .iter()
            .filter_map(|((session, _), pending)| {
                (session == session_id.as_str()).then(|| pending.request.clone())
            })
            .collect())
    }

    async fn answer(
        &self,
        session_id: &SessionId,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> SessionResult<bool> {
        if self.stopped.is_cancelled() {
            return Err(SessionApplicationError::ShuttingDown);
        }
        if approval_id.is_empty()
            || approval_id.len() > rsi_approval_protocol::MAXIMUM_APPROVAL_FIELD_BYTES
        {
            return Err(SessionApplicationError::Invalid(
                "approval id is empty or exceeds its byte limit".into(),
            ));
        }
        let outcome = ApprovalOutcome {
            decision,
            answerer: "rsi.session-host".into(),
            reason: None,
        };
        outcome
            .validate()
            .map_err(|error| SessionApplicationError::Backend(error.to_string()))?;
        let key = (session_id.as_str().to_owned(), approval_id.to_owned());
        let mut inner = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(prior) = inner.settled.get(&key) {
            return if *prior == decision {
                Ok(true)
            } else {
                Err(SessionApplicationError::Invalid(
                    "approval answer conflicts with its settled receipt".into(),
                ))
            };
        }
        let sender = inner.pending.remove(&key).and_then(|mut pending| {
            inner.pending_bytes -= pending.encoded_len;
            pending.answer.take()
        });
        let Some(sender) = sender else {
            return Ok(false);
        };
        if sender.send(outcome).is_err() {
            return Ok(false);
        }
        if inner.settled_order.len() == MAXIMUM_SETTLED_APPROVALS
            && let Some(expired) = inner.settled_order.pop_front()
        {
            inner.settled.remove(&expired);
        }
        inner.settled_order.push_back(key.clone());
        inner.settled.insert(key, decision);
        Ok(true)
    }
}
