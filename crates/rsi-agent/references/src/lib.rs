//! Bounded capture and explicit reads of immutable conversation references.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod export;
mod selected;
pub use selected::{ObservedReferenceText, ReferenceText, reference_texts};
mod plugin;
mod tools;
pub use plugin::{ReferencesContract, ReferencesFactory};
use rsi_agent_session_protocol::{
    AgentMessageContent, EffectId, FrozenReference, InputMessageSource,
    MAXIMUM_AGENT_IDENTIFIER_BYTES, MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS,
    MAXIMUM_REFERENCE_PAGE_BYTES, MAXIMUM_REFERENCE_PREVIEW_BYTES, MAXIMUM_REFERENCE_SCAN_BYTES,
    MAXIMUM_REFERENCE_SCAN_FACTS, MAXIMUM_REFERENCE_TEXT_BYTES, ModelEventPurpose,
    ReferenceBinding, ReferenceCapture, ReferenceMetadata, ReferenceOmission, ReferenceReadRequest,
    ReferenceSnapshotEnvelope, ReferenceSnapshotRef, ReferenceSource, ReferenceSuffix,
    ReferenceTextPage, SessionFact, SessionFactBody, SessionHeader, SessionId, TurnId,
};
use rsi_agent_store_protocol::{CasObjectRef, SessionStore, StoreError};
use sha2::{Digest as _, Sha256};
use std::{
    collections::VecDeque,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Semaphore;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
pub use tools::ReferenceToolsFactory;

/// Closed failures for finite reference work.
#[derive(Debug, thiserror::Error)]
pub enum ReferenceError {
    /// Malformed or mismatched reference data.
    #[error("invalid reference: {0}")]
    Invalid(String),
    /// Both finite workers are occupied.
    #[error("reference capacity exhausted")]
    Capacity,
    /// Read waiter or owner was cancelled, or its deadline elapsed.
    #[error("reference read cancelled")]
    Cancelled,
    /// A retained worker failed independently of cancellation.
    #[error("reference worker failed")]
    WorkerFailed,
    /// Mechanical Store operation failed.
    #[error("reference storage: {0}")]
    Store(#[from] StoreError),
}
/// Reference operation result.
pub type Result<T> = std::result::Result<T, ReferenceError>;
fn invalid(error: impl std::fmt::Display) -> ReferenceError {
    ReferenceError::Invalid(error.to_string())
}
fn check(stop: &CancellationToken) -> Result<()> {
    if stop.is_cancelled() {
        Err(ReferenceError::Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Debug, Default)]
struct VerifiedEnvelopes(VecDeque<(ReferenceSnapshotRef, Arc<ReferenceSnapshotEnvelope>)>);
impl VerifiedEnvelopes {
    fn get(&mut self, snapshot: &ReferenceSnapshotRef) -> Option<Arc<ReferenceSnapshotEnvelope>> {
        let index = self.0.iter().position(|(key, _)| key == snapshot)?;
        let entry = self.0.remove(index)?;
        let envelope = entry.1.clone();
        self.0.push_back(entry);
        Some(envelope)
    }
    fn insert(&mut self, snapshot: ReferenceSnapshotRef, envelope: Arc<ReferenceSnapshotEnvelope>) {
        self.0.retain(|(key, _)| key != &snapshot);
        if self.0.len() == 2 {
            self.0.pop_front();
        }
        self.0.push_back((snapshot, envelope));
    }
}

/// One owner of bounded reference workers, independent of Session residency.
#[derive(Debug)]
pub struct References {
    store: Arc<dyn SessionStore>,
    cache: Arc<Mutex<VerifiedEnvelopes>>,
    execution: rsi_meta::Execution,
    slots: Arc<Semaphore>,
    stop: CancellationToken,
    tasks: TaskTracker,
    admission: std::sync::Mutex<()>,
}
impl References {
    /// Constructs one generation with two admitted finite workers.
    pub fn new(store: Arc<dyn SessionStore>, execution: rsi_meta::Execution) -> Self {
        Self {
            store,
            cache: Arc::default(),
            execution,
            slots: Arc::new(Semaphore::new(2)),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            admission: std::sync::Mutex::new(()),
        }
    }
    /// Stops new work and drains dispatched operations before releasing the owner.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned the owner state lock.
    pub async fn close(&self) {
        {
            let _admission = self.admission.lock().expect("reference admission poisoned");
            self.slots.close();
            self.stop.cancel();
            self.tasks.close();
        }
        self.tasks.wait().await;
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
            .clear();
    }
    async fn run<T, F, Fut>(&self, cancellation: CancellationToken, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<dyn SessionStore>, CancellationToken, Arc<Mutex<VerifiedEnvelopes>>) -> Fut
            + Send
            + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let stop = self.stop.child_token();
        let _cancel = stop.clone().drop_guard();
        let worker_stop = stop.clone();
        let store = self.store.clone();
        let cache = self.cache.clone();
        let task = {
            let _admission = self.admission.lock().expect("reference admission poisoned");
            check(&self.stop)?;
            check(&cancellation)?;
            let permit = self
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| ReferenceError::Capacity)?;
            self.execution.spawn(self.tasks.track_future(async move {
                let _permit = permit;
                check(&worker_stop)?;
                // Do not race a Store read against cancellation here. A dispatched
                // read must keep this permit until its actual materialization returns.
                let result = operation(store, worker_stop.clone(), cache).await;
                check(&worker_stop)?;
                result
            }))
        };
        let deadline = self.execution.deadline_after(Duration::from_secs(30));
        tokio::select! { biased;
            () = cancellation.cancelled() => Err(ReferenceError::Cancelled),
            () = self.stop.cancelled() => Err(ReferenceError::Cancelled),
            result = deadline.timeout(task) => result.map_err(|_| ReferenceError::Cancelled)?.map_err(|_| ReferenceError::WorkerFailed)?,
        }
    }
    /// Captures a durable source's current bounded suffix for this actual target.
    pub async fn capture(
        &self,
        source: SessionId,
        target: SessionHeader,
        cancellation: CancellationToken,
    ) -> Result<FrozenReference> {
        target.validate().map_err(invalid)?;
        self.run(cancellation, move |store, stop, _cache| async move {
            let source_header = store.header(&source).await?;
            check(&stop)?;
            let _validation = store.prepare_session(&source).await?;
            check(&stop)?;
            let suffix = store
                .read_fact_suffix(
                    &source,
                    MAXIMUM_REFERENCE_SCAN_FACTS,
                    MAXIMUM_REFERENCE_SCAN_BYTES,
                )
                .await?;
            check(&stop)?;
            let envelope = export::capture(&source_header, &target, suffix)?;
            let bytes: Arc<[u8]> = serde_json::to_vec(&envelope).map_err(invalid)?.into();
            check(&stop)?;
            let object = store.put_cas(bytes).await?;
            check(&stop)?;
            envelope.into_frozen(ReferenceSnapshotRef {
                sha256: object.sha256,
                byte_len: object.byte_len,
            })
        })
        .await
    }
    /// Verifies a descriptor's exact CAS payload and original-target binding.
    pub async fn verify(
        &self,
        target: SessionHeader,
        reference: FrozenReference,
        cancellation: CancellationToken,
    ) -> Result<()> {
        reference.validate().map_err(invalid)?;
        target.validate().map_err(invalid)?;
        self.run(cancellation, move |store, stop, cache| async move {
            load(&*store, &target, &reference, &stop, &cache)
                .await
                .map(|_| ())
        })
        .await
    }
    /// Reads immutable preview contents before submission, bound to the actual target.
    pub async fn preview(
        &self,
        target: SessionHeader,
        reference: FrozenReference,
        offset: usize,
        maximum: usize,
        cancellation: CancellationToken,
    ) -> Result<ReferenceTextPage> {
        rsi_agent_session_protocol::validate_reference_page_bounds(offset, maximum)
            .map_err(invalid)?;
        reference.validate().map_err(invalid)?;
        target.validate().map_err(invalid)?;
        self.run(cancellation, move |store, stop, cache| async move {
            let envelope = load(&*store, &target, &reference, &stop, &cache).await?;
            page(reference, &envelope.text, offset, maximum)
        })
        .await
    }
    /// Resolves only an actual recorded Human reference in the authorized lineage.
    pub async fn read_recorded(
        &self,
        target: SessionHeader,
        request: ReferenceReadRequest,
        cancellation: CancellationToken,
    ) -> Result<ReferenceTextPage> {
        target.validate().map_err(invalid)?;
        request.validate().map_err(invalid)?;
        let binding = request.recorded_binding(&target).map_err(invalid)?;
        let parent =
            (&request.recorded_session_id != target.session_id()).then_some(binding.header_sha256);
        self.run(cancellation, move |store, stop, cache| async move {
            let recorded_header = if let Some(fingerprint) = parent {
                let header = store.header(&request.recorded_session_id).await?;
                check(&stop)?;
                if header.fingerprint().map_err(invalid)? != fingerprint {
                    return Err(invalid("inherited Header changed"));
                }
                header
            } else {
                target
            };
            let _validation = store.prepare_session(&request.recorded_session_id).await?;
            check(&stop)?;
            let facts = store
                .read_facts(&request.recorded_session_id, request.fact_seq - 1, 1)
                .await?;
            check(&stop)?;
            let reference = {
                let [fact] = facts.facts.as_slice() else {
                    return Err(invalid("recorded reference is unavailable"));
                };
                if fact.seq() != request.fact_seq {
                    return Err(invalid("recorded reference Fact mismatch"));
                }
                let SessionFactBody::InputMessageEntered {
                    source: InputMessageSource::Human { .. },
                    content,
                    ..
                } = fact.body()
                else {
                    return Err(invalid("reference must name direct Human input"));
                };
                let Some(AgentMessageContent::Reference { reference }) =
                    content.get(request.content_index)
                else {
                    return Err(invalid("recorded content is not a reference"));
                };
                reference.clone()
            };
            drop(facts);
            let envelope = load(&*store, &recorded_header, &reference, &stop, &cache).await?;
            let mut page = page(reference, &envelope.text, request.offset, request.maximum)?;
            page.recorded = Some(request);
            page.validate().map_err(invalid)?;
            Ok(page)
        })
        .await
    }
}
async fn load(
    store: &dyn SessionStore,
    target: &SessionHeader,
    reference: &FrozenReference,
    stop: &CancellationToken,
    cache: &Mutex<VerifiedEnvelopes>,
) -> Result<Arc<ReferenceSnapshotEnvelope>> {
    reference.validate().map_err(invalid)?;
    if &reference.metadata.target.session_id != target.session_id()
        || reference.metadata.target.header_sha256 != target.fingerprint().map_err(invalid)?
    {
        return Err(invalid("reference belongs to another target Header"));
    }
    let cached = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&reference.snapshot);
    let envelope = if let Some(envelope) = cached {
        envelope
    } else {
        let bytes = store
            .read_cas(&CasObjectRef {
                sha256: reference.snapshot.sha256.clone(),
                byte_len: reference.snapshot.byte_len,
            })
            .await?;
        check(stop)?;
        if bytes.len() as u64 != reference.snapshot.byte_len
            || hex::encode(Sha256::digest(&bytes)) != reference.snapshot.sha256
        {
            return Err(invalid("reference CAS identity mismatch"));
        }
        let envelope: ReferenceSnapshotEnvelope =
            serde_json::from_slice(&bytes).map_err(invalid)?;
        envelope.validate().map_err(invalid)?;
        let envelope = Arc::new(envelope);
        cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(reference.snapshot.clone(), envelope.clone());
        envelope
    };
    if envelope.metadata != reference.metadata || envelope.preview != reference.preview {
        return Err(invalid("reference metadata or preview changed"));
    }
    Ok(envelope)
}
fn page(
    reference: FrozenReference,
    text: &str,
    offset: usize,
    maximum: usize,
) -> Result<ReferenceTextPage> {
    let offset = text.ceil_char_boundary(offset.min(text.len()));
    let end = text.floor_char_boundary(offset + maximum.min(text.len() - offset));
    let page = ReferenceTextPage {
        recorded: None,
        reference,
        offset,
        next_offset: end,
        text: text[offset..end].into(),
        has_more: end < text.len(),
    };
    page.validate().map_err(invalid)?;
    Ok(page)
}

// Capture policy may tighten the mechanical Store seam, never exceed it.
const _: () = assert!(
    rsi_agent_session_protocol::MAXIMUM_REFERENCE_SCAN_FACTS
        <= rsi_agent_store_protocol::MAXIMUM_STORE_SUFFIX_FACTS
);
const _: () = assert!(
    rsi_agent_session_protocol::MAXIMUM_REFERENCE_SCAN_BYTES
        <= rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES
);
