use super::{Result, SessionError, SessionId, SessionStore, map_store_error};
use futures_util::FutureExt as _;
use rsi_agent_session_protocol::{
    EvidencePart, EvidenceSection, EvidenceUnavailable, MAXIMUM_REQUEST_EVIDENCE_BYTES,
    RequestEvidence, SessionFactBody,
};
use rsi_api_protocol::{ByteBudget, ByteReservation};
use rsi_session_protocol::{EvidencePage, EvidencePageContent, EvidenceRead};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{Semaphore, watch};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const MAXIMUM_COLD_LOADS: usize = 4;
const MAXIMUM_ENTRIES: usize = 64;
const CLEANUP_SECONDS: u64 = 30;
const _: () = assert!(
    MAXIMUM_COLD_LOADS * MAXIMUM_REQUEST_EVIDENCE_BYTES <= rsi_api_protocol::MAXIMUM_API_BYTES
);

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct Key(SessionId, u64, EvidenceSection);
enum Selected {
    Available {
        text: Box<str>,
        sha256: String,
        _reservation: ByteReservation,
    },
    Unavailable(EvidenceUnavailable),
}
impl Selected {
    fn available(text: String, sha256: String, mut reservation: ByteReservation) -> Self {
        let text = text.into_boxed_str();
        reservation
            .shrink(text.len())
            .expect("validated section fits its cold-load reservation");
        Self::Available {
            text,
            sha256,
            _reservation: reservation,
        }
    }
    fn page(&self, request: &EvidenceRead) -> Result<EvidencePage> {
        let content = match self {
            Self::Available { text, sha256, .. } => window(text, sha256, request)?,
            Self::Unavailable(reason) => EvidencePageContent::Unavailable { reason: *reason },
        };
        let page = EvidencePage {
            intent_seq: request.intent_seq,
            section: request.section,
            content,
        };
        page.validate(request)?;
        Ok(page)
    }
}
type Loaded = Result<Arc<Selected>>;
enum Entry {
    Loading(watch::Receiver<Option<Loaded>>),
    Ready(Arc<Selected>),
}
struct Record {
    used: u64,
    entry: Entry,
}
#[derive(Default)]
struct State {
    entries: BTreeMap<Key, Record>,
    clock: u64,
}
impl State {
    fn make_room(&mut self, budget: &ByteBudget) -> Result<()> {
        let mut size = self.entries.len() + 1;
        let mut bytes = budget.used() + MAXIMUM_REQUEST_EVIDENCE_BYTES;
        if size > MAXIMUM_ENTRIES || bytes > budget.limit() {
            let mut candidates = self
                .entries
                .iter()
                .filter_map(|(key, record)| match &record.entry {
                    Entry::Ready(value) if Arc::strong_count(value) == 1 => {
                        let bytes = match value.as_ref() {
                            Selected::Available { text, .. } => text.len(),
                            Selected::Unavailable(_) => 0,
                        };
                        Some((record.used, key, bytes))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            candidates.sort_by_key(|(used, ..)| *used);
            let mut victims = Vec::new();
            for (_, key, weight) in candidates {
                if size <= MAXIMUM_ENTRIES && bytes <= budget.limit() {
                    break;
                }
                if size <= MAXIMUM_ENTRIES && weight == 0 {
                    continue;
                }
                size -= 1;
                bytes -= weight;
                victims.push(key.clone());
            }
            if size > MAXIMUM_ENTRIES || bytes > budget.limit() {
                return Err(SessionError::Capacity);
            }
            for victim in victims {
                self.entries.remove(&victim);
            }
        }
        Ok(())
    }
}
pub(super) struct EvidenceReader {
    state: Mutex<State>,
    budget: ByteBudget,
    workers: Arc<Semaphore>,
    stopped: CancellationToken,
    tasks: TaskTracker,
}
impl Default for EvidenceReader {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            budget: ByteBudget::default(),
            // Four worst-case reservations fill the pool, so the fourth cold
            // load may evict all idle text before its actual size is known.
            workers: Arc::new(Semaphore::new(MAXIMUM_COLD_LOADS)),
            stopped: CancellationToken::new(),
            tasks: TaskTracker::new(),
        }
    }
}
impl EvidenceReader {
    pub(super) async fn read<G: Send + Sync + 'static>(
        self: &Arc<Self>,
        store: Arc<dyn SessionStore>,
        session: SessionId,
        request: &EvidenceRead,
        guard: G,
    ) -> Result<EvidencePage> {
        request.validate()?;
        let guard = Arc::new(guard);
        self.load(
            store,
            Key(session, request.intent_seq, request.section),
            guard.clone(),
        )
        .await?
        .page(request)
    }
    async fn load<G: Send + 'static>(
        self: &Arc<Self>,
        store: Arc<dyn SessionStore>,
        key: Key,
        guard: G,
    ) -> Loaded {
        let mut receiver = {
            let mut state = self.state.lock().expect("evidence admission");
            if self.stopped.is_cancelled() {
                return Err(SessionError::ShuttingDown);
            }
            state.clock += 1;
            let used = state.clock;
            if let Some(record) = state.entries.get_mut(&key) {
                record.used = used;
                match &record.entry {
                    Entry::Ready(value) => return Ok(value.clone()),
                    Entry::Loading(receiver) => receiver.clone(),
                }
            } else {
                let permit = self
                    .workers
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| SessionError::Capacity)?;
                state.make_room(&self.budget)?;
                let reservation = self
                    .budget
                    .reserve(MAXIMUM_REQUEST_EVIDENCE_BYTES)
                    .map_err(|_| SessionError::Capacity)?;
                let (sender, receiver) = watch::channel(None);
                state.entries.insert(
                    key.clone(),
                    Record {
                        used,
                        entry: Entry::Loading(receiver.clone()),
                    },
                );
                let owner = self.clone();
                self.tasks.spawn(async move {
                    let _permit = permit;
                    let _guard = guard;
                    let result = std::panic::AssertUnwindSafe(load_section(
                        store.as_ref(),
                        &key,
                        reservation,
                        &owner.stopped,
                    ))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| {
                        Err(SessionError::Backend("evidence loader panicked".into()))
                    })
                    .map(Arc::new);
                    let result = {
                        let mut state = owner.state.lock().expect("evidence completion");
                        if owner.stopped.is_cancelled() {
                            Err(SessionError::ShuttingDown)
                        } else {
                            match &result {
                                Ok(value) => {
                                    state.clock += 1;
                                    let used = state.clock;
                                    state.entries.insert(
                                        key,
                                        Record {
                                            used,
                                            entry: Entry::Ready(value.clone()),
                                        },
                                    );
                                }
                                Err(_) => {
                                    state.entries.remove(&key);
                                }
                            }
                            result
                        }
                    };
                    sender.send_replace(Some(result));
                    // Release an unobserved completion before returning its worker
                    // slot. Live receivers still pin their own selected result.
                    drop(sender);
                });
                receiver
            }
        };
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            tokio::select! {
                biased;
                () = self.stopped.cancelled() => return Err(SessionError::ShuttingDown),
                result = receiver.changed() => result.map_err(|_|SessionError::Backend("evidence loader disappeared".into()))?,
            }
        }
    }
    pub(super) fn close(&self) {
        let mut state = self.state.lock().expect("evidence close");
        // Admission and completion use this lock: cancelled implies an empty map.
        self.stopped.cancel();
        state.entries.clear();
        self.tasks.close();
    }
    pub(super) async fn stop(&self) -> Result<()> {
        self.close();
        tokio::time::timeout(
            std::time::Duration::from_secs(CLEANUP_SECONDS),
            self.tasks.wait(),
        )
        .await
        .map_err(|_| {
            SessionError::Backend(format!(
                "Evidence cleanup exceeded {CLEANUP_SECONDS} seconds; work remains owned"
            ))
        })
    }
}

async fn load_section(
    store: &dyn SessionStore,
    key: &Key,
    reservation: ByteReservation,
    stopped: &CancellationToken,
) -> Result<Selected> {
    let Key(session, intent_seq, section) = key;
    let mut page = store
        .read_facts(session, intent_seq - 1, 1)
        .await
        .map_err(map_store_error)?;
    if stopped.is_cancelled() {
        return Err(SessionError::ShuttingDown);
    }
    let fact = page
        .facts
        .pop()
        .filter(|fact| fact.seq() == *intent_seq)
        .ok_or_else(|| SessionError::NotFound("request intent".into()))?;
    let SessionFactBody::ModelIntent { evidence, .. } = fact.into_body() else {
        return Err(SessionError::Invalid(
            "selected source is not a request intent".into(),
        ));
    };
    match evidence {
        RequestEvidence::Unavailable { reason } => Ok(Selected::Unavailable(reason)),
        RequestEvidence::Available { .. } => {
            let part = evidence
                .into_part(*section)
                .expect("available evidence has every section");
            match part {
                EvidencePart::Inline { text, sha256 } => {
                    Ok(Selected::available(text, sha256, reservation))
                }
                EvidencePart::Reference {
                    seq,
                    section,
                    sha256,
                    bytes,
                } => {
                    if seq >= *intent_seq {
                        return Err(SessionError::Invalid(
                            "request evidence original must precede its reference".into(),
                        ));
                    }
                    let original =
                        rsi_agent_store_protocol::EvidenceOriginal::read(store, session, seq)
                            .await
                            .map_err(resolve_error)?;
                    if stopped.is_cancelled() {
                        return Err(SessionError::ShuttingDown);
                    }
                    let text = original
                        .into_text(section, &sha256, bytes)
                        .map_err(resolve_error)?;
                    Ok(Selected::available(text, sha256, reservation))
                }
            }
        }
    }
}
fn window(text: &str, sha256: &str, request: &EvidenceRead) -> Result<EvidencePageContent> {
    let page = rsi_conversation::FieldWindow::text(
        text,
        request.offset as usize,
        request.maximum_bytes as usize,
    )
    .map_err(|error| SessionError::Invalid(error.to_string()))?;
    Ok(EvidencePageContent::Available {
        sha256: sha256.into(),
        total_bytes: u32::try_from(text.len()).expect("validated evidence fits 16 MiB"),
        start: u32::try_from(page.start).expect("page is within validated evidence"),
        text: page.text,
        more: page.more,
    })
}

fn resolve_error(error: rsi_agent_store_protocol::EvidenceResolveError) -> SessionError {
    match error {
        rsi_agent_store_protocol::EvidenceResolveError::Store(error) => map_store_error(error),
        rsi_agent_store_protocol::EvidenceResolveError::Invalid(message) => {
            SessionError::Invalid(message.into())
        }
    }
}

#[cfg(test)]
mod tests;
