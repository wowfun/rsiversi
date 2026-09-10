use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_user_questions_protocol::{
    QuestionAnswer, QuestionError, QuestionRequest, Result, UserQuestions, UserQuestionsContract,
    validate_identity,
};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const CAPACITY: usize = 256;
type Key = (String, String);

/// One Host-generation live question broker.
#[derive(Clone, Debug, Default)]
pub struct QuestionBroker {
    inner: Arc<Mutex<State>>,
    stopped: CancellationToken,
    changes: crate::changes::Changes,
}

#[derive(Debug, Default)]
struct State {
    next_generation: u64,
    pending: BTreeMap<Key, Pending>,
    receipts: BTreeMap<Key, QuestionAnswer>,
    order: VecDeque<Key>,
}

#[derive(Debug)]
struct Pending {
    generation: u64,
    request: QuestionRequest,
    sender: oneshot::Sender<QuestionAnswer>,
}

struct Registration {
    broker: QuestionBroker,
    key: Key,
    generation: u64,
}
impl Drop for Registration {
    fn drop(&mut self) {
        let mut state = self
            .broker
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .pending
            .get(&self.key)
            .is_some_and(|pending| pending.generation == self.generation)
        {
            state.pending.remove(&self.key);
            self.broker.changes.notify(&self.key.0);
        }
    }
}

impl QuestionBroker {
    /// Cancels pending waiters and rejects new operations.
    pub fn stop(&self) {
        self.stopped.cancel();
        *self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = State::default();
        self.changes.notify_all();
    }
}

#[async_trait]
impl UserQuestions for QuestionBroker {
    async fn ask(
        &self,
        request: QuestionRequest,
        cancellation: CancellationToken,
    ) -> Result<QuestionAnswer> {
        request.validate()?;
        let key = (request.session_id.clone(), request.id.clone());
        let (generation, receiver) = {
            let mut state = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.stopped.is_cancelled() || cancellation.is_cancelled() {
                return Err(QuestionError::Cancelled);
            }
            if state.pending.contains_key(&key) || state.receipts.contains_key(&key) {
                return Err(QuestionError::Conflict);
            }
            if state.pending.len() >= CAPACITY {
                return Err(QuestionError::Capacity);
            }
            let (sender, receiver) = oneshot::channel();
            state.next_generation = state
                .next_generation
                .checked_add(1)
                .ok_or(QuestionError::Capacity)?;
            let generation = state.next_generation;
            state.pending.insert(
                key.clone(),
                Pending {
                    generation,
                    request,
                    sender,
                },
            );
            self.changes.notify(&key.0);
            (generation, receiver)
        };
        let _registration = Registration {
            broker: self.clone(),
            key,
            generation,
        };
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(QuestionError::Cancelled),
            () = self.stopped.cancelled() => Err(QuestionError::Cancelled),
            answer = receiver => answer.map_err(|_| QuestionError::Cancelled),
        }
    }

    fn watch_pending(
        &self,
        sessions: &[String],
    ) -> Result<rsi_user_questions_protocol::PendingChanges> {
        for session in sessions {
            validate_identity(session)?;
        }
        self.changes
            .subscribe(sessions)
            .ok_or(QuestionError::Capacity)
    }
    async fn pending(&self, session_id: &str) -> Result<Vec<QuestionRequest>> {
        self.pending_for_sessions(&[session_id.to_owned()]).await
    }
    async fn pending_for_sessions(&self, sessions: &[String]) -> Result<Vec<QuestionRequest>> {
        if sessions.len() > 256 {
            return Err(QuestionError::Capacity);
        }
        for session in sessions {
            validate_identity(session)?;
        }
        let selected = sessions.iter().collect::<std::collections::BTreeSet<_>>();
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stopped.is_cancelled() {
            return Err(QuestionError::Cancelled);
        }
        Ok(state
            .pending
            .iter()
            .filter(|((session, _), _)| selected.contains(session))
            .map(|(_, pending)| pending.request.clone())
            .collect())
    }

    async fn answer(
        &self,
        session_id: &str,
        request_id: &str,
        answer: QuestionAnswer,
    ) -> Result<bool> {
        validate_identity(session_id)?;
        validate_identity(request_id)?;
        answer.validate()?;
        let key = (session_id.to_owned(), request_id.to_owned());
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stopped.is_cancelled() {
            return Err(QuestionError::Cancelled);
        }
        if let Some(prior) = state.receipts.get(&key) {
            return if *prior == answer {
                Ok(true)
            } else {
                Err(QuestionError::Conflict)
            };
        }
        let Some(pending) = state.pending.get(&key) else {
            return Ok(false);
        };
        answer.validate_for(&pending.request)?;
        let pending = state
            .pending
            .remove(&key)
            .expect("validated pending request");
        self.changes.notify(&key.0);
        if pending.sender.send(answer.clone()).is_err() {
            return Ok(false);
        }
        if state.order.len() == CAPACITY
            && let Some(expired) = state.order.pop_front()
        {
            state.receipts.remove(&expired);
        }
        state.order.push_back(key.clone());
        state.receipts.insert(key, answer);
        Ok(true)
    }
}

/// Ordinary lifecycle owner for the standard Host's question broker.
#[derive(Clone, Debug, Default)]
pub struct QuestionBrokerFactory;

#[async_trait]
impl PluginFactory for QuestionBrokerFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "question broker configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::with_state(ConfigValue::Null, (), 0))
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (): () = plan.take_state()?;
        let broker = Arc::new(QuestionBroker::default());
        let cleanup = broker.clone();
        plan.defer(
            "stop human questions",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.stop();
                    Ok(())
                })
            }),
        )?;
        let supply = plan
            .context()
            .provide_local::<UserQuestionsContract>(broker)?;
        plan.defer(
            "withdraw human questions",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
