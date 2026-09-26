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

/// One stable action in a closed review; labels carry no action authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewChoice {
    /// Stable action identity.
    pub id: String,
    /// Display label.
    pub label: String,
}
/// Product-opaque closed review binding, distinct from suggested answers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosedReview {
    /// Exact content/version binding assigned by the requester.
    pub binding: String,
    /// Closed choices in presentation order.
    pub choices: Vec<ReviewChoice>,
}
impl ClosedReview {
    fn validate(&self) -> Result<()> {
        validate_identity(&self.binding)?;
        let mut ids = std::collections::BTreeSet::new();
        if !(2..=8).contains(&self.choices.len()) {
            return Err(invalid("review requires two to eight choices"));
        }
        for choice in &self.choices {
            validate_identity(&choice.id)?;
            if !ids.insert(&choice.id) || choice.label.trim().is_empty() || choice.label.len() > 256
            {
                return Err(invalid(
                    "review choices require unique identities and bounded labels",
                ));
            }
        }
        Ok(())
    }
}
/// Typed response to a closed review, never inferred by the broker from prose.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewAnswer {
    /// Exact displayed request binding.
    pub binding: String,
    /// One choice identity from that request.
    pub choice_id: String,
    /// Optional human feedback, at most 4 KiB.
    pub feedback: Option<String>,
}
impl ReviewAnswer {
    fn validate(&self) -> Result<()> {
        validate_identity(&self.binding)?;
        validate_identity(&self.choice_id)?;
        if self.feedback.as_ref().is_some_and(|text| text.len() > 4096) {
            return Err(invalid("review feedback exceeds 4 KiB"));
        }
        Ok(())
    }
}

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
    /// Closed review variant, or ordinary questions when absent.
    #[serde(default)]
    pub review: Option<ClosedReview>,
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
        if let Some(review) = &self.review {
            review.validate()?;
            if self.questions.len() != 1 || !self.questions[0].options.is_empty() {
                return Err(invalid(
                    "closed review requires one display question without suggestions",
                ));
            }
        }
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
    /// Closed review answer; ordinary answer strings must be empty in this variant.
    #[serde(default)]
    pub review: Option<ReviewAnswer>,
    /// One nonempty answer for each question, in request order.
    pub answers: Vec<String>,
}

impl QuestionAnswer {
    /// Validates the answer document before request lookup.
    pub fn validate(&self) -> Result<()> {
        if let Some(review) = &self.review {
            review.validate()?;
            if !self.answers.is_empty() {
                return Err(invalid("review cannot contain ordinary answers"));
            }
            return bounded(self);
        }
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
        match (&self.review, &request.review) {
            (Some(answer), Some(review))
                if answer.binding == review.binding
                    && review
                        .choices
                        .iter()
                        .any(|choice| choice.id == answer.choice_id) =>
            {
                return Ok(());
            }
            (None, None) => {}
            _ => return Err(invalid("answer does not match the exact closed review")),
        }
        if self.answers.len() != request.questions.len() {
            return Err(invalid("answer count does not match the request"));
        }
        Ok(())
    }

    /// Converts an explicit numbered terminal selection and optional feedback
    /// for the displayed closed review. Ordinary prose never chooses an action.
    pub fn select_review(request: &QuestionRequest, input: &str) -> Result<Self> {
        let review = request
            .review
            .as_ref()
            .ok_or_else(|| invalid("request is not a closed review"))?;
        let (number, feedback) = input
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((input.trim(), ""));
        let index = number
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .ok_or_else(|| {
                invalid("enter a review choice number, optionally followed by feedback")
            })?;
        let choice = review
            .choices
            .get(index)
            .ok_or_else(|| invalid("review choice is out of range"))?;
        let answer = Self {
            answers: vec![],
            review: Some(ReviewAnswer {
                binding: review.binding.clone(),
                choice_id: choice.id.clone(),
                feedback: (!feedback.trim().is_empty()).then(|| feedback.trim().to_owned()),
            }),
        };
        answer.validate_for(request)?;
        Ok(answer)
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
    #[serde(default)]
    review: Option<ClosedReview>,
    id: String,
    session_id: String,
    turn_id: String,
    questions: Vec<Question>,
}

impl TryFrom<QuestionRequestWire> for QuestionRequest {
    type Error = QuestionError;
    fn try_from(wire: QuestionRequestWire) -> Result<Self> {
        let value = Self {
            review: wire.review,
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
    #[serde(default)]
    review: Option<ReviewAnswer>,
    answers: Vec<String>,
}

impl TryFrom<QuestionAnswerWire> for QuestionAnswer {
    type Error = QuestionError;
    fn try_from(wire: QuestionAnswerWire) -> Result<Self> {
        let value = Self {
            review: wire.review,
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
    fn closed_review_requires_exact_binding_action_and_bounded_feedback() {
        let request = QuestionRequest {
            id: "review".into(),
            session_id: "session".into(),
            turn_id: "turn".into(),
            questions: vec![Question {
                id: "plan".into(),
                prompt: "Exact saved plan".into(),
                options: vec![],
            }],
            review: Some(ClosedReview {
                binding: "plan-and-version".into(),
                choices: vec![
                    ReviewChoice {
                        id: "approve_execute".into(),
                        label: "Approve".into(),
                    },
                    ReviewChoice {
                        id: "decline".into(),
                        label: "Decline".into(),
                    },
                ],
            }),
        };
        request.validate().unwrap();
        for input in ["yes", "approve_execute", "0", "3", ""] {
            assert!(QuestionAnswer::select_review(&request, input).is_err());
        }
        let answer = QuestionAnswer::select_review(&request, "1 please proceed").unwrap();
        assert_eq!(answer.review.as_ref().unwrap().choice_id, "approve_execute");
        assert_eq!(
            answer.review.as_ref().unwrap().feedback.as_deref(),
            Some("please proceed")
        );
        assert!(
            QuestionAnswer {
                review: None,
                answers: vec!["Approve".into()]
            }
            .validate_for(&request)
            .is_err()
        );
        let mut stale = answer.clone();
        stale.review.as_mut().unwrap().binding = "old-plan".into();
        assert!(stale.validate_for(&request).is_err());
        let mut unknown = answer.clone();
        unknown.review.as_mut().unwrap().choice_id = "execute_other".into();
        assert!(unknown.validate_for(&request).is_err());
        assert!(
            QuestionAnswer::select_review(&request, &format!("1 {}", "x".repeat(4097))).is_err()
        );
        let roundtrip: QuestionAnswer =
            serde_json::from_value(serde_json::to_value(&answer).unwrap()).unwrap();
        roundtrip.validate_for(&request).unwrap();
    }
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
            review: None,
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
