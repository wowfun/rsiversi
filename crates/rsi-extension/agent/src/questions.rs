//! Reverse requests borrow an admitted Tool's authority, never wire-supplied claims.
use async_trait::async_trait;
use rsi_agent_turn_protocol::{AgentCallerAuthority, TurnExecution};
use rsi_extension::{Error, Handler, Result};
use rsi_tools_protocol::{ToolExecution, ToolLaneParkingAuthority};
use rsi_user_questions_protocol::{
    ClosedReview, Question, QuestionRequest, ReviewChoice, UserQuestions,
};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

fn invalid(error: impl std::fmt::Display) -> Error {
    Error::Remote(error.to_string())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    session_id: String,
    operation_id: String,
    questions: Vec<Question>,
    review: Option<Vec<ReviewChoice>>,
}

#[derive(Debug)]
struct Authority {
    caller: Arc<AgentCallerAuthority>,
    parking: Arc<ToolLaneParkingAuthority>,
    cancellation: CancellationToken,
    asking: AtomicBool,
    next: AtomicU64,
}

#[derive(Debug)]
pub(super) struct QuestionRouter {
    turns: Arc<dyn TurnExecution>,
    questions: Arc<dyn UserQuestions>,
    active: Mutex<BTreeMap<(String, String), Arc<Authority>>>,
}
impl QuestionRouter {
    pub fn new(turns: Arc<dyn TurnExecution>, questions: Arc<dyn UserQuestions>) -> Arc<Self> {
        Arc::new(Self {
            turns,
            questions,
            active: Mutex::new(BTreeMap::new()),
        })
    }
    pub fn enter(self: &Arc<Self>, execution: &ToolExecution) -> Result<Option<QuestionScope>> {
        let Some(parking) = execution.extension::<ToolLaneParkingAuthority>() else {
            return Ok(None);
        };
        let caller = execution
            .extension::<AgentCallerAuthority>()
            .ok_or_else(|| invalid("missing Agent authority"))?;
        let effect = caller
            .tool_effect_id()
            .ok_or_else(|| invalid("missing Tool effect"))?;
        let key = (caller.session_id().to_string(), effect.to_string());
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active.len() >= 64 || active.contains_key(&key) {
            return Err(Error::Capacity);
        }
        let cancellation = execution.cancellation.child_token();
        active.insert(
            key.clone(),
            Arc::new(Authority {
                caller,
                parking,
                cancellation: cancellation.clone(),
                asking: AtomicBool::new(false),
                next: AtomicU64::new(0),
            }),
        );
        Ok(Some(QuestionScope {
            router: self.clone(),
            key,
            cancellation,
        }))
    }
}

pub(super) struct QuestionScope {
    router: Arc<QuestionRouter>,
    key: (String, String),
    cancellation: CancellationToken,
}
impl Drop for QuestionScope {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.router
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
    }
}
struct Asking(Arc<Authority>);
impl Drop for Asking {
    fn drop(&mut self) {
        self.0.asking.store(false, Ordering::Release);
    }
}

#[async_trait]
impl Handler for QuestionRouter {
    async fn call(
        &self,
        _: &str,
        payload: Value,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        // Check the smaller business bound before copying or hashing the request.
        if serde_json::to_vec(&payload).map_err(invalid)?.len()
            > rsi_user_questions_protocol::MAXIMUM_QUESTION_BYTES
        {
            return Err(Error::Capacity);
        }
        let wire: Request = serde_json::from_value(payload).map_err(invalid)?;
        let authority = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(wire.session_id, wire.operation_id))
            .cloned()
            .ok_or_else(|| invalid("question has no active Tool authority"))?;
        if authority.cancellation.is_cancelled() || cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if authority.asking.swap(true, Ordering::AcqRel) {
            return Err(invalid("parallel questions for one Tool are not admitted"));
        }
        let _asking = Asking(authority.clone());
        let ordinal = authority.next.fetch_add(1, Ordering::AcqRel);
        if ordinal >= 64 {
            return Err(Error::Capacity);
        }
        let caller = &authority.caller;
        let binding = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    caller.session_id(),
                    caller.turn_id(),
                    caller.tool_effect_id(),
                    ordinal,
                    &wire.questions,
                    &wire.review,
                ))
                .map_err(invalid)?
            )
        );
        let request = QuestionRequest {
            id: binding.clone(),
            session_id: caller.session_id().to_string(),
            turn_id: caller.turn_id().to_string(),
            questions: wire.questions,
            review: wire.review.map(|choices| ClosedReview { binding, choices }),
        };
        request.validate().map_err(invalid)?;
        let token = cancellation.child_token();
        let _cancel = token.clone().drop_guard();
        let answer = async {
            let waiting = self
                .turns
                .park_human_wait(caller.claim(), (*authority.parking).clone())
                .await
                .map_err(invalid)?;
            let answer = self.questions.ask(request.clone(), token.clone()).await;
            waiting.resume(token.clone()).await.map_err(invalid)?;
            if token.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let answer = answer.map_err(invalid)?;
            answer.validate_for(&request).map_err(invalid)?;
            serde_json::to_value(answer).map_err(invalid)
        };
        tokio::select! { biased;
            () = authority.cancellation.cancelled() => Err(Error::Cancelled),
            () = cancellation.cancelled() => Err(Error::Cancelled),
            result = answer => result,
        }
    }
}
