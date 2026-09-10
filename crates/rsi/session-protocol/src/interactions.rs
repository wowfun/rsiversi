use super::{ApprovalRequest, Arc, Result, SessionError, fmt, map_question_error};
use rsi_user_questions_protocol::QuestionRequest;
use serde::Serialize;
use std::sync::atomic::{AtomicUsize, Ordering};

const MAXIMUM_BYTES: usize = 64 * 1024 * 1024;
const COLLECTION_BYTES: usize = 32 * 1024 * 1024 + 4096;
const MAXIMUM_APPROVALS: usize = 1024;
const MAXIMUM_QUESTIONS: usize = 256;

/// Live snapshots whose payload reservation follows the final retained clone.
pub type InteractionStream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<InteractionSnapshot>> + Send>>;

/// Aggregate ownership of retained snapshots for one Session application/decoder.
#[derive(Clone, Debug, Default)]
pub struct InteractionRetention(Arc<AtomicUsize>);

struct Reservation {
    pool: InteractionRetention,
    bytes: usize,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.pool.0.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
impl Reservation {
    fn shrink(&mut self, bytes: usize) {
        assert!(bytes <= self.bytes, "snapshot was bounded before admission");
        self.pool.0.fetch_sub(self.bytes - bytes, Ordering::AcqRel);
        self.bytes = bytes;
    }
}

#[derive(Debug, PartialEq, Serialize)]
struct SnapshotData {
    approvals: Vec<ApprovalRequest>,
    questions: Vec<QuestionRequest>,
}
struct RetainedSnapshot {
    data: SnapshotData,
    _reservation: Reservation,
}

/// Immutable complete live baseline; clones share the payload and its byte lease.
#[derive(Clone)]
pub struct InteractionSnapshot(Arc<RetainedSnapshot>);
impl fmt::Debug for InteractionSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.data.fmt(f)
    }
}
impl PartialEq for InteractionSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.0.data == other.0.data
    }
}
impl Serialize for InteractionSnapshot {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.0.data.serialize(serializer)
    }
}
impl InteractionSnapshot {
    /// Current pending approvals throughout the attached root's tree.
    pub fn approvals(&self) -> &[ApprovalRequest] {
        &self.0.data.approvals
    }
    /// Current pending human questions for the attached root.
    pub fn questions(&self) -> &[QuestionRequest] {
        &self.0.data.questions
    }
}

impl InteractionRetention {
    /// Canonical bytes held by all issued snapshot clones.
    pub fn retained_bytes(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }

    fn reserve(&self, bytes: usize) -> Result<Reservation> {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|total| *total <= MAXIMUM_BYTES)
            })
            .map_err(|_| SessionError::Capacity)?;
        Ok(Reservation {
            pool: self.clone(),
            bytes,
        })
    }

    /// Validates and retains a decoded snapshot before releasing transport admission.
    pub fn retain(
        &self,
        approvals: Vec<ApprovalRequest>,
        questions: Vec<QuestionRequest>,
    ) -> Result<InteractionSnapshot> {
        let data = SnapshotData {
            approvals,
            questions,
        };
        let bytes = validate_snapshot(&data)?;
        Ok(InteractionSnapshot(Arc::new(RetainedSnapshot {
            data,
            _reservation: self.reserve(bytes)?,
        })))
    }
}

struct Counter(usize);
impl std::io::Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|total| *total <= MAXIMUM_BYTES)
            .ok_or_else(|| std::io::Error::other("interaction snapshot exceeds byte bound"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn validate_snapshot(data: &SnapshotData) -> Result<usize> {
    if data.approvals.len() > MAXIMUM_APPROVALS || data.questions.len() > MAXIMUM_QUESTIONS {
        return Err(SessionError::Invalid(
            "interaction count exceeds its independent bound".into(),
        ));
    }
    let mut approval_bytes = 0_usize;
    let mut identities = std::collections::BTreeSet::new();
    for request in &data.approvals {
        request
            .validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        approval_bytes = approval_bytes
            .checked_add(
                request
                    .encoded_len()
                    .map_err(|error| SessionError::Invalid(error.to_string()))?,
            )
            .filter(|bytes| *bytes <= 16 * 1024 * 1024)
            .ok_or_else(|| SessionError::Invalid("approval snapshot exceeds byte bound".into()))?;
        if !identities.insert((request.subject.session_id(), request.id.as_str())) {
            return Err(SessionError::Invalid("duplicate approval tuple".into()));
        }
    }
    identities.clear();
    for request in &data.questions {
        request.validate().map_err(map_question_error)?;
        if !identities.insert((request.session_id.as_str(), request.id.as_str())) {
            return Err(SessionError::Invalid("duplicate question tuple".into()));
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, data)
        .map_err(|error| SessionError::Invalid(error.to_string()))?;
    Ok(counter.0)
}

/// Pre-admitted collection budget released unless completed into a snapshot.
pub struct InteractionCollection(Reservation);
impl fmt::Debug for InteractionCollection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InteractionCollection")
            .finish_non_exhaustive()
    }
}
impl InteractionCollection {
    /// Validates the complete collected payload and transfers its exact lease.
    pub fn retain(
        mut self,
        approvals: Vec<ApprovalRequest>,
        questions: Vec<QuestionRequest>,
    ) -> Result<InteractionSnapshot> {
        let data = SnapshotData {
            approvals,
            questions,
        };
        self.0.shrink(validate_snapshot(&data)?);
        Ok(InteractionSnapshot(Arc::new(RetainedSnapshot {
            data,
            _reservation: self.0,
        })))
    }
}
impl InteractionRetention {
    /// Reserves before copying any broker payload into a new live snapshot.
    pub fn reserve_collection(&self) -> Result<InteractionCollection> {
        self.reserve(COLLECTION_BYTES).map(InteractionCollection)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalRequest, InteractionRetention, MAXIMUM_BYTES, QuestionRequest, SessionError,
    };
    use rsi_approval_protocol::ApprovalSubject;

    fn approval(index: usize) -> ApprovalRequest {
        ApprovalRequest {
            review: None,
            subject: ApprovalSubject::new("session", "turn", "effect").unwrap(),
            id: format!("approval-{index}"),
            action: "write".into(),
            reason: "test".into(),
        }
    }
    fn question(index: usize, bytes: usize) -> QuestionRequest {
        QuestionRequest {
            id: format!("question-{index}"),
            session_id: "session".into(),
            turn_id: "turn".into(),
            questions: vec![rsi_user_questions_protocol::Question {
                id: "choice".into(),
                prompt: "x".repeat(bytes),
                options: Vec::new(),
            }],
        }
    }

    #[test]
    fn approval_and_question_counts_have_independent_full_capacity() {
        let pool = InteractionRetention::default();
        let snapshot = pool
            .retain(
                (0..1024).map(approval).collect(),
                (0..256).map(|index| question(index, 1)).collect(),
            )
            .unwrap();
        assert_eq!(
            snapshot.approvals().len() + snapshot.questions().len(),
            1280
        );
        let before = pool.retained_bytes();
        assert!(
            pool.retain((0..1025).map(approval).collect(), Vec::new())
                .is_err()
        );
        assert!(
            pool.retain(
                Vec::new(),
                (0..257).map(|index| question(index, 1)).collect()
            )
            .is_err()
        );
        assert_eq!(pool.retained_bytes(), before);
        drop(snapshot);
        assert_eq!(pool.retained_bytes(), 0);
    }

    #[test]
    fn maximum_valid_snapshot_fits_the_collection_reservation() {
        let approvals = (0..1024)
            .map(|index| {
                let mut value = approval(index);
                let padding = 16 * 1024 - value.encoded_len().unwrap();
                value.reason.push_str(&"\0".repeat(padding / 6));
                value.reason.push_str(&"x".repeat(padding % 6));
                value.validate().unwrap();
                assert_eq!(value.encoded_len().unwrap(), 16 * 1024);
                value
            })
            .collect();
        let questions = (0..256)
            .map(|index| {
                let mut value = question(index, 1);
                let padding = rsi_user_questions_protocol::MAXIMUM_QUESTION_BYTES
                    - serde_json::to_vec(&value).unwrap().len();
                value.questions[0].prompt.push_str(&"x".repeat(padding));
                value.validate().unwrap();
                assert_eq!(serde_json::to_vec(&value).unwrap().len(), 64 * 1024);
                value
            })
            .collect();
        let data = super::SnapshotData {
            approvals,
            questions,
        };
        let size = super::validate_snapshot(&data).unwrap();
        assert_eq!(size, 32 * 1024 * 1024 + 1309);
        let pool = InteractionRetention::default();
        let mut reservation = pool.reserve(super::COLLECTION_BYTES).unwrap();
        reservation.shrink(size);
        assert_eq!(pool.retained_bytes(), size);
        drop(reservation);
        assert_eq!(pool.retained_bytes(), 0);
        assert!(question(0, 70_000).validate().is_err());
    }

    #[test]
    fn snapshot_capacity_is_atomic_and_follows_the_last_clone() {
        let pool = InteractionRetention::default();
        let make = || (0..256).map(|index| question(index, 63_000)).collect();
        let mut retained = Vec::new();
        for _ in 0..4 {
            retained.push(pool.retain(Vec::new(), make()).unwrap());
        }
        let bytes = pool.retained_bytes();
        assert!(bytes <= MAXIMUM_BYTES);
        assert!(matches!(
            pool.retain(Vec::new(), make()),
            Err(SessionError::Capacity)
        ));
        assert_eq!(pool.retained_bytes(), bytes);
        let last = retained.pop().unwrap();
        let clone = last.clone();
        drop(last);
        assert_eq!(pool.retained_bytes(), bytes);
        assert!(matches!(
            pool.retain(Vec::new(), make()),
            Err(SessionError::Capacity)
        ));
        drop(clone);
        let replacement = pool.retain(Vec::new(), make()).unwrap();
        drop(replacement);
        drop(retained);
        assert_eq!(pool.retained_bytes(), 0);
    }
}
