//! Staged Agent journal segments with exact canonical receipt verification.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod adapter;
mod arguments;
mod encoding;
mod events;
mod history;
pub use history::project_history_page;
mod questions;
pub use adapter::AgentAdapter;
mod factory;
pub use factory::AgentAdapterFactory;

use rsi_agent_composition_protocol::{
    DomainDefinition, DomainHandle, DomainReferencePolicy, ValidatedDomainProposal,
};
use rsi_agent_session_protocol::{
    DomainIdentity, DomainRevision, DomainStateValue, EXTENSION_JOURNAL_DOMAIN,
    EXTENSION_SEGMENT_TARGET_BYTES, ExtensionJournal, ExtensionSegment,
    MAXIMUM_EXTENSION_JOURNAL_BYTES,
};
use rsi_agent_store_protocol::SessionStore;
use rsi_agent_turn_protocol::DomainMutationReceipt;
use std::sync::Arc;

/// Adapter failure before or after the separate canonical publication boundary.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Temporary Store read admission pressure.
    #[error("extension journal read capacity exhausted")]
    Capacity,
    /// Invalid journal content or an unavailable dependency.
    #[error("extension journal: {0}")]
    Invalid(String),
}
fn error(value: impl std::fmt::Display) -> Error {
    Error::Invalid(value.to_string())
}

/// Defines the same built-in envelope understood by offline reference maintenance.
///
/// # Errors
/// Rejects an invalid initial journal envelope.
pub fn definition() -> Result<DomainDefinition<ExtensionJournal>, Error> {
    DomainDefinition::new(
        DomainIdentity::new(
            EXTENSION_JOURNAL_DOMAIN,
            rsi_agent_session_protocol::EXTENSION_JOURNAL_VERSION,
        )
        .map_err(error)?,
        &ExtensionJournal {
            segments: vec![],
            sessions: vec![],
            checkpoint: DomainStateValue::new(serde_json::Value::Null).map_err(error)?,
        },
        ExtensionJournal::validate,
        DomainReferencePolicy::Visit(ExtensionJournal::references),
    )
    .map(|definition| {
        definition.with_fork_policy(rsi_agent_composition_protocol::DomainForkPolicy::Transform)
    })
    .map_err(error)
}

/// Prepared proposal plus immutable CAS ownership. Dropping it never publishes a mutation.
#[derive(Debug)]
pub struct PreparedJournal {
    proposal: ValidatedDomainProposal,
}
impl PreparedJournal {
    /// Borrows the exact proposal to include in one atomic owner mutation.
    pub const fn proposal(&self) -> &ValidatedDomainProposal {
        &self.proposal
    }

    /// Verifies the canonical update, including predecessor and complete snapshot.
    /// The owner still binds the receipt's Session and request identity before dispatch.
    ///
    /// # Errors
    /// Rejects a missing, conflicting or revision-exhausted update.
    pub fn verify_receipt(&self, receipt: &DomainMutationReceipt) -> Result<DomainRevision, Error> {
        let update = receipt
            .commit()
            .updates()
            .iter()
            .find(|update| update.snapshot().identity() == self.proposal.snapshot().identity())
            .ok_or_else(|| error("receipt omits the staged journal"))?;
        if update.snapshot() != self.proposal.snapshot()
            || update.expected_revision() != self.proposal.expected_revision()
        {
            return Err(error("receipt does not identify the staged journal"));
        }
        self.proposal.expected_revision().next().map_err(error)
    }
}

/// Stages one pure extension operation for the caller's subsequent atomic commit.
///
/// # Errors
/// Rejects invalid or oversized state and propagates CAS staging failures.
pub async fn prepare(
    store: &dyn SessionStore,
    handle: &DomainHandle<ExtensionJournal>,
    revision: DomainRevision,
    current: &ExtensionJournal,
    events: Vec<serde_json::Value>,
    checkpoint: DomainStateValue,
    admission: rsi_api_protocol::ByteAdmission,
) -> Result<PreparedJournal, Error> {
    prepare_encoded(
        store,
        handle,
        revision,
        current,
        events::Events::from_values(events)?,
        checkpoint,
        admission,
    )
    .await
}

pub(crate) async fn prepare_encoded(
    store: &dyn SessionStore,
    handle: &DomainHandle<ExtensionJournal>,
    revision: DomainRevision,
    current: &ExtensionJournal,
    events: events::Events,
    checkpoint: DomainStateValue,
    admission: rsi_api_protocol::ByteAdmission,
) -> Result<PreparedJournal, Error> {
    current.validate().map_err(error)?;
    let mut next = current.clone();
    next.checkpoint = checkpoint;
    let mut objects = Vec::new();
    let mut _tail_read = None;
    let events::Events {
        arrays,
        credits: _credits,
        ..
    } = events;
    if !arrays.is_empty() {
        let used: u64 = next.segments.iter().map(|segment| segment.byte_len).sum();
        let merge_saving = usize::from(next.segments.last().is_some_and(|tail| {
            tail.count < 4096 && tail.byte_len < EXTENSION_SEGMENT_TARGET_BYTES as u64
        }));
        let mut suffix = encode_segments(
            arrays,
            MAXIMUM_EXTENSION_JOURNAL_BYTES - usize::try_from(used).map_err(error)? + merge_saving,
        )?;
        if let (Some(tail), Some(first)) = (next.segments.last(), suffix.first_mut())
            && tail.count + first.count <= 4096
            && tail.byte_len + first.bytes.len() as u64 - 1 <= EXTENSION_SEGMENT_TARGET_BYTES as u64
        {
            let retained = restore_segment_bytes(store, tail, admission).await?;
            let decoded: Vec<serde_json::Value> =
                serde_json::from_slice(&retained).map_err(error)?;
            if decoded.len() as u64 != tail.count {
                return Err(error("segment event count mismatch"));
            }
            let mut bytes = serde_json::to_vec(&decoded).map_err(error)?;
            drop(decoded);
            _tail_read = Some(retained);
            append_array(&mut bytes, &first.bytes);
            first.bytes = bytes;
            first.count += tail.count;
            next.segments.pop();
        }
        let prefix = next.segments.len();
        let mut cursor = next
            .segments
            .last()
            .map_or(0, |entry| entry.first + entry.count);
        for segment in &suffix {
            next.segments.push(ExtensionSegment {
                first: cursor,
                count: segment.count,
                sha256: "0".repeat(64),
                byte_len: segment.bytes.len() as u64,
            });
            cursor += segment.count;
        }
        // Validate every aggregate before creating any immutable object.
        next.validate().map_err(error)?;
        for (segment, encoded) in next.segments[prefix..].iter_mut().zip(suffix) {
            let object = store
                .put_cas(Arc::from(encoded.bytes))
                .await
                .map_err(error)?;
            segment.sha256.clone_from(&object.sha256);
            objects.push(object);
        }
    }
    for segment in next
        .segments
        .iter()
        .take(next.segments.len() - objects.len())
    {
        objects.push(
            store
                .retain_cas(&rsi_agent_store_protocol::CasObjectRef {
                    sha256: segment.sha256.clone(),
                    byte_len: segment.byte_len,
                })
                .await
                .map_err(error)?,
        );
    }
    let proposal = handle
        .propose(revision, &next)
        .map_err(error)?
        .with_retained(Arc::new(objects));
    Ok(PreparedJournal { proposal })
}

struct EncodedSegment {
    count: u64,
    bytes: Vec<u8>,
}

fn append_array(target: &mut Vec<u8>, source: &[u8]) {
    target.pop();
    target.push(b',');
    target.extend_from_slice(&source[1..]);
}

fn encode_segments(arrays: Vec<Vec<u8>>, maximum: usize) -> Result<Vec<EncodedSegment>, Error> {
    let mut segments: Vec<EncodedSegment> = Vec::new();
    let mut used = 0;
    for bytes in arrays {
        if let Some(tail) = segments.last_mut()
            && tail.count < 4096
            && tail.bytes.len() + bytes.len() - 1 <= EXTENSION_SEGMENT_TARGET_BYTES
            && bytes.len() - 1 <= maximum - used
        {
            used += bytes.len() - 1;
            append_array(&mut tail.bytes, &bytes);
            tail.count += 1;
        } else {
            if bytes.len() > maximum - used || segments.len() == 256 {
                return Err(error("extension journal capacity exceeded"));
            }
            used += bytes.len();
            segments.push(EncodedSegment { count: 1, bytes });
        }
    }
    Ok(segments)
}

/// Authenticates all immutable segments before returning the complete bounded event prefix.
///
/// # Errors
/// Rejects invalid state, unavailable or corrupt CAS bytes, and count mismatches.
pub async fn restore(
    store: &dyn SessionStore,
    journal: &ExtensionJournal,
    admission: impl Fn() -> rsi_api_protocol::ByteAdmission,
) -> Result<Vec<serde_json::Value>, Error> {
    journal.validate().map_err(error)?;
    let mut events = Vec::new();
    for segment in &journal.segments {
        events.extend(restore_segment(store, segment, admission()).await?);
    }
    Ok(events)
}

async fn restore_segment(
    store: &dyn SessionStore,
    segment: &ExtensionSegment,
    admission: rsi_api_protocol::ByteAdmission,
) -> Result<Vec<serde_json::Value>, Error> {
    let bytes = restore_segment_bytes(store, segment, admission).await?;
    let part: Vec<serde_json::Value> = serde_json::from_slice(bytes.as_ref()).map_err(error)?;
    if part.len() as u64 != segment.count {
        return Err(error("segment event count mismatch"));
    }
    Ok(part)
}

/// Keeps authenticated encoded bytes and Store admission together for transport.
pub(crate) async fn restore_segment_bytes(
    store: &dyn SessionStore,
    segment: &ExtensionSegment,
    admission: rsi_api_protocol::ByteAdmission,
) -> Result<rsi_api_protocol::RetainedBytes, Error> {
    let object = rsi_agent_store_protocol::CasObjectRef {
        sha256: segment.sha256.clone(),
        byte_len: segment.byte_len,
    };
    store
        .read_cas(&object, admission)
        .await
        .map_err(|failure| match failure {
            rsi_agent_store_protocol::StoreError::ReadCapacity => Error::Capacity,
            other => error(other),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transport_pages_preserve_canonical_segments_and_hashes() {
        let store = rsi_agent_testkit::MemoryStore::default();
        let definition = definition().unwrap();
        let catalog =
            rsi_agent_composition_protocol::DomainCatalog::new([definition.registration()])
                .unwrap();
        let handle = catalog.bind(&definition).unwrap();
        let initial = handle.decode(&catalog.baseline()[0]).unwrap();
        let values = vec![
            serde_json::json!({"z":"x".repeat(130_000),"a":0}),
            serde_json::json!({"b":1e2,"a":"界"}),
            serde_json::json!("y".repeat(140_000)),
            serde_json::json!(null),
        ];
        let budget = rsi_api_protocol::ByteBudget::new(1024 * 1024).unwrap();
        let expected = prepare(
            &store,
            &handle,
            DomainRevision::default(),
            &initial,
            values.clone(),
            initial.checkpoint.clone(),
            budget.clone().into(),
        )
        .await
        .unwrap();
        let expected_journal = handle.decode(expected.proposal().snapshot()).unwrap();
        assert_eq!(expected_journal.segments.len(), 2);
        for (segment, group) in expected_journal
            .segments
            .iter()
            .zip([&values[..2], &values[2..]])
        {
            use sha2::Digest;
            let canonical = serde_json::to_vec(group).unwrap();
            assert_eq!(segment.count, 2);
            assert_eq!(segment.byte_len, canonical.len() as u64);
            assert_eq!(
                segment.sha256,
                format!("{:x}", sha2::Sha256::digest(canonical))
            );
        }
        for split in 1..values.len() {
            let mut pages: events::Events =
                serde_json::from_slice(&serde_json::to_vec_pretty(&values[..split]).unwrap())
                    .unwrap();
            let second =
                serde_json::from_slice(&serde_json::to_vec(&values[split..]).unwrap()).unwrap();
            pages.append(second).unwrap();
            let actual = prepare_encoded(
                &store,
                &handle,
                DomainRevision::default(),
                &initial,
                pages,
                initial.checkpoint.clone(),
                budget.clone().into(),
            )
            .await
            .unwrap();
            assert_eq!(actual.proposal().snapshot(), expected.proposal().snapshot());
        }
    }

    #[tokio::test]
    async fn encoded_event_credits_survive_cas_reads_and_release_on_cancellation_or_failure() {
        let store = Arc::new(rsi_agent_testkit::MemoryStore::default());
        let definition = definition().unwrap();
        let catalog =
            rsi_agent_composition_protocol::DomainCatalog::new([definition.registration()])
                .unwrap();
        let handle = catalog.bind(&definition).unwrap();
        let initial = handle.decode(&catalog.baseline()[0]).unwrap();
        let budget = rsi_api_protocol::ByteBudget::new(1024).unwrap();
        let first = prepare(
            store.as_ref(),
            &handle,
            DomainRevision::default(),
            &initial,
            vec![serde_json::json!(1)],
            initial.checkpoint.clone(),
            budget.clone().into(),
        )
        .await
        .unwrap();
        let journal = handle.decode(first.proposal().snapshot()).unwrap();
        let pool = Arc::new(tokio::sync::Semaphore::new(1024));
        let charged = || {
            let mut events = events::Events::from_values(vec![serde_json::json!(2)]).unwrap();
            events.credits.push(
                pool.clone()
                    .try_acquire_many_owned(u32::try_from(events.bytes).unwrap())
                    .unwrap(),
            );
            events
        };
        let events = charged();
        let (entered, _release) = store.pause_next_cas_read();
        let pending = {
            let store = store.clone();
            let handle = handle.clone();
            let journal = journal.clone();
            let budget = budget.clone();
            tokio::spawn(async move {
                prepare_encoded(
                    store.as_ref(),
                    &handle,
                    DomainRevision::default(),
                    &journal,
                    events,
                    journal.checkpoint.clone(),
                    budget.into(),
                )
                .await
            })
        };
        entered.await.unwrap();
        assert_eq!(pool.available_permits(), 1021);
        pending.abort();
        assert!(pending.await.unwrap_err().is_cancelled());
        assert_eq!(pool.available_permits(), 1024);
        prepare_encoded(
            store.as_ref(),
            &handle,
            DomainRevision::default(),
            &journal,
            charged(),
            journal.checkpoint.clone(),
            budget.clone().into(),
        )
        .await
        .unwrap();
        assert_eq!(pool.available_permits(), 1024);
        assert_eq!(budget.used(), 0);
        let mut invalid = journal;
        invalid.segments[0].byte_len = MAXIMUM_EXTENSION_JOURNAL_BYTES as u64;
        assert!(
            prepare_encoded(
                store.as_ref(),
                &handle,
                DomainRevision::default(),
                &invalid,
                charged(),
                invalid.checkpoint.clone(),
                budget.into()
            )
            .await
            .is_err()
        );
        assert_eq!(pool.available_permits(), 1024);
    }

    #[tokio::test]
    async fn exact_journal_limit_accepts_framing_allowance_and_a_small_tail_merge() {
        let store = rsi_agent_testkit::MemoryStore::default();
        let definition = definition().unwrap();
        let catalog =
            rsi_agent_composition_protocol::DomainCatalog::new([definition.registration()])
                .unwrap();
        let handle = catalog.bind(&definition).unwrap();
        let initial = handle.decode(&catalog.baseline()[0]).unwrap();
        let budget = rsi_api_protocol::ByteBudget::new(MAXIMUM_EXTENSION_JOURNAL_BYTES).unwrap();
        let mut events = events::Events::from_values(vec![
            serde_json::json!("x".repeat(MAXIMUM_EXTENSION_JOURNAL_BYTES - 15)),
            serde_json::json!(null),
            serde_json::json!(null),
        ])
        .unwrap();
        assert_eq!(events.bytes, MAXIMUM_EXTENSION_JOURNAL_BYTES + 1);
        let pool = Arc::new(tokio::sync::Semaphore::new(
            MAXIMUM_EXTENSION_JOURNAL_BYTES + EXTENSION_SEGMENT_TARGET_BYTES,
        ));
        events.credits.push(
            pool.clone()
                .try_acquire_many_owned(u32::try_from(events.bytes).unwrap())
                .unwrap(),
        );
        let first = prepare_encoded(
            &store,
            &handle,
            DomainRevision::default(),
            &initial,
            events,
            initial.checkpoint.clone(),
            budget.clone().into(),
        )
        .await
        .unwrap();
        let mut journal = handle.decode(first.proposal().snapshot()).unwrap();
        let small = store
            .put_cas(Arc::from(b"[null]".as_slice()))
            .await
            .unwrap();
        let tail = journal.segments.last_mut().unwrap();
        tail.sha256.clone_from(&small.sha256);
        tail.byte_len = small.byte_len;
        tail.count = 1;
        let next = prepare(
            &store,
            &handle,
            DomainRevision::default(),
            &journal,
            vec![serde_json::json!(null)],
            journal.checkpoint.clone(),
            budget.into(),
        )
        .await
        .unwrap();
        let final_journal = handle.decode(next.proposal().snapshot()).unwrap();
        assert_eq!(
            final_journal
                .segments
                .iter()
                .map(|segment| segment.byte_len)
                .sum::<u64>(),
            MAXIMUM_EXTENSION_JOURNAL_BYTES as u64
        );
        assert_eq!(final_journal.segments.last().unwrap().count, 2);
        assert_eq!(next.proposal().snapshot(), first.proposal().snapshot());
        assert_eq!(
            pool.available_permits(),
            MAXIMUM_EXTENSION_JOURNAL_BYTES + EXTENSION_SEGMENT_TARGET_BYTES
        );
    }

    #[tokio::test]
    async fn predicted_merge_saving_cannot_publish_an_oversized_unmerged_journal() {
        let store = rsi_agent_testkit::MemoryStore::default();
        let definition = definition().unwrap();
        let catalog =
            rsi_agent_composition_protocol::DomainCatalog::new([definition.registration()])
                .unwrap();
        let handle = catalog.bind(&definition).unwrap();
        let mut journal = handle.decode(&catalog.baseline()[0]).unwrap();
        let tail = EXTENSION_SEGMENT_TARGET_BYTES - 1;
        journal.segments = vec![
            ExtensionSegment {
                first: 0,
                count: 1,
                sha256: "a".repeat(64),
                byte_len: (MAXIMUM_EXTENSION_JOURNAL_BYTES - tail - 5) as u64,
            },
            ExtensionSegment {
                first: 1,
                count: 1,
                sha256: "b".repeat(64),
                byte_len: tail as u64,
            },
        ];
        journal.validate().unwrap();
        let error = prepare(
            &store,
            &handle,
            DomainRevision::default(),
            &journal,
            vec![serde_json::json!(null)],
            journal.checkpoint.clone(),
            rsi_api_protocol::ByteBudget::new(1024).unwrap().into(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("byte capacity"), "{error}");
    }

    #[test]
    fn event_budget_counts_json_escaping_and_array_framing() {
        let event = serde_json::json!("\\".repeat(4));
        assert!(
            encode_segments(
                events::Events::from_values(vec![event.clone()])
                    .unwrap()
                    .arrays,
                11
            )
            .is_err()
        );
        let segments =
            encode_segments(events::Events::from_values(vec![event]).unwrap().arrays, 12).unwrap();
        assert_eq!(segments[0].bytes.len(), 12);
        let packed = encode_segments(
            events::Events::from_values(vec![serde_json::json!(1), serde_json::json!(2)])
                .unwrap()
                .arrays,
            5,
        )
        .unwrap();
        assert_eq!(packed[0].bytes, b"[1,2]");
        assert_eq!(packed[0].count, 2);
    }
}

#[cfg(test)]
rsi_persistence_testkit::payload_contract!(
    persistence_extension_journal,
    ExtensionJournal,
    rsi_agent_session_protocol::EXTENSION_JOURNAL_VERSION,
    format!(
        "tests/fixtures/persistence/extension_journal-v{}.json",
        rsi_agent_session_protocol::EXTENSION_JOURNAL_VERSION
    ),
    {
        let samples: Vec<ExtensionJournal> = vec![
            ExtensionJournal {
                segments: vec![],
                sessions: vec![],
                checkpoint: DomainStateValue::new(serde_json::Value::Null).unwrap(),
            },
            ExtensionJournal {
                segments: vec![ExtensionSegment {
                    first: 0,
                    count: 1,
                    sha256: "a".repeat(64),
                    byte_len: 9,
                }],
                sessions: vec![rsi_agent_session_protocol::SessionId::new("fixture").unwrap()],
                checkpoint: DomainStateValue::new(
                    serde_json::json!({"z":1,"a":"中🦀","optional":null}),
                )
                .unwrap(),
            },
        ];
        for sample in &samples {
            sample.validate().unwrap();
        }
        samples
    }
);
