//! Bounded synchronous human questions, independent of Agent durability and UI.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Maximum encoded size of a request or answer.
pub const MAXIMUM_QUESTION_BYTES: usize = 64 * 1024;

/// One prompt with optional suggestions; free text is always accepted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    /// Unique identifier within the request.
    pub id: String,
    /// Human-readable prompt.
    pub prompt: String,
    /// Suggested answers in display order.
    #[serde(default)]
    pub options: Vec<String>,
}

/// Exact live request exposed to all attached clients.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "QuestionRequestWire")]
pub struct QuestionRequest {
    /// Opaque identity allocated by the requester for this Host generation.
    pub id: String,
    /// Exact owning Session.
    pub session_id: String,
    /// Exact owning Turn.
    pub turn_id: String,
    /// One to three questions in answer order.
    pub questions: Vec<Question>,
}

impl QuestionRequest {
    /// Validates identities, content, cardinality, and aggregate encoded size.
    pub fn validate(&self) -> Result<()> {
        for identity in [&self.id, &self.session_id, &self.turn_id] {
            validate_identity(identity)?;
        }
        validate_questions(&self.questions)?;
        bounded(self)
    }
}

/// Validates one question batch before it enters a request.
pub fn validate_questions(questions: &[Question]) -> Result<()> {
    if !(1..=3).contains(&questions.len()) {
        return Err(invalid("expected one to three questions"));
    }
    let mut ids = std::collections::BTreeSet::new();
    for question in questions {
        validate_identity(&question.id)?;
        if !ids.insert(&question.id)
            || question.prompt.trim().is_empty()
            || question.options.len() > 8
            || question
                .options
                .iter()
                .any(|option| option.trim().is_empty())
        {
            return Err(invalid(
                "question identifiers must be unique; prompts and suggestions must be nonempty; at most eight suggestions are allowed",
            ));
        }
    }
    bounded(&questions)
}

/// Ordered free-text answers. Selecting a suggestion sends its exact text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "QuestionAnswerWire")]
pub struct QuestionAnswer {
    /// One nonempty answer for each question, in request order.
    pub answers: Vec<String>,
}

impl QuestionAnswer {
    /// Validates the answer document before request lookup.
    pub fn validate(&self) -> Result<()> {
        if !(1..=3).contains(&self.answers.len())
            || self.answers.iter().any(|answer| answer.trim().is_empty())
        {
            return Err(invalid("expected one to three nonempty answers"));
        }
        bounded(self)
    }

    /// Validates correspondence to this exact request.
    pub fn validate_for(&self, request: &QuestionRequest) -> Result<()> {
        self.validate()?;
        if self.answers.len() != request.questions.len() {
            return Err(invalid("answer count does not match the request"));
        }
        Ok(())
    }
}

/// Validates an opaque routing identity at a control boundary.
pub fn validate_identity(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(invalid(
            "identity must contain 1..=256 bytes without control characters",
        ));
    }
    Ok(())
}

fn bounded(value: &impl Serialize) -> Result<()> {
    if serde_json::to_vec(value)
        .map_err(|error| invalid(&error.to_string()))?
        .len()
        > MAXIMUM_QUESTION_BYTES
    {
        return Err(invalid("question document exceeds 64 KiB"));
    }
    Ok(())
}
fn invalid(message: &str) -> QuestionError {
    QuestionError::Invalid(message.to_owned())
}

/// Coalesced payload-free changes for one bounded Session selection.
pub type PendingChanges = std::pin::Pin<Box<dyn futures_util::Stream<Item = ()> + Send>>;

/// Live provider and client-control seam. Implementations revalidate all inputs.
#[async_trait]
pub trait UserQuestions: std::fmt::Debug + Send + Sync + 'static {
    /// Waits for the first valid answer until cancellation.
    async fn ask(
        &self,
        request: QuestionRequest,
        cancellation: CancellationToken,
    ) -> Result<QuestionAnswer>;
    /// Lists live pending requests for one exact Session.
    async fn pending(&self, session_id: &str) -> Result<Vec<QuestionRequest>>;
    /// Collects selected Sessions in one bounded registry pass.
    async fn pending_for_sessions(&self, sessions: &[String]) -> Result<Vec<QuestionRequest>>;
    /// Registers before snapshot collection; changes coalesce without a payload queue.
    fn watch_pending(&self, sessions: &[String]) -> Result<PendingChanges>;
    /// Settles or retries one answer; false means unavailable.
    async fn answer(
        &self,
        session_id: &str,
        request_id: &str,
        answer: QuestionAnswer,
    ) -> Result<bool>;
}

/// Typed process-local service marker.
#[derive(Debug)]
pub struct UserQuestionsContract;
impl LocalContract for UserQuestionsContract {
    const KEY: &'static str = "rsi.user-questions";
    type Service = dyn UserQuestions;
}

/// Closed question error taxonomy.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QuestionError {
    /// Invalid or oversized input.
    #[error("invalid human question: {0}")]
    Invalid(String),
    /// Conflicting request identity or settled answer.
    #[error("human question identity or answer conflicts")]
    Conflict,
    /// Live broker capacity exhausted.
    #[error("human question capacity exhausted")]
    Capacity,
    /// Waiter or provider cancelled.
    #[error("human question cancelled")]
    Cancelled,
}

/// Question operation result.
pub type Result<T> = std::result::Result<T, QuestionError>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionRequestWire {
    id: String,
    session_id: String,
    turn_id: String,
    questions: Vec<Question>,
}

impl TryFrom<QuestionRequestWire> for QuestionRequest {
    type Error = QuestionError;
    fn try_from(wire: QuestionRequestWire) -> Result<Self> {
        let value = Self {
            id: wire.id,
            session_id: wire.session_id,
            turn_id: wire.turn_id,
            questions: wire.questions,
        };
        value.validate()?;
        Ok(value)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionAnswerWire {
    answers: Vec<String>,
}

impl TryFrom<QuestionAnswerWire> for QuestionAnswer {
    type Error = QuestionError;
    fn try_from(wire: QuestionAnswerWire) -> Result<Self> {
        let value = Self {
            answers: wire.answers,
        };
        value.validate()?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decoding_rejects_invalid_answer_and_request_documents() {
        for answers in [
            vec![],
            vec![String::new()],
            vec!["x".repeat(MAXIMUM_QUESTION_BYTES)],
            vec!["x".to_owned(); 4],
        ] {
            assert!(
                serde_json::from_value::<QuestionAnswer>(serde_json::json!({"answers":answers}))
                    .is_err()
            );
        }
        let valid = QuestionAnswer {
            answers: vec!["one".into()],
        };
        assert_eq!(
            serde_json::from_value::<QuestionAnswer>(serde_json::to_value(&valid).unwrap())
                .unwrap(),
            valid
        );
        assert!(serde_json::from_value::<QuestionRequest>(serde_json::json!({"id":"id","session_id":"session","turn_id":"turn","questions":[]})).is_err());
    }
}
