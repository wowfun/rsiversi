use super::*;
use rsi_agent_session_protocol::EvidencePart;

/// Cache misses use one bounded Store read per distinct original sequence.
/// Fact construction owns inline digest validation; this stage owns external lookups.
pub(super) async fn validate_references(
    store: &dyn SessionStore,
    cache: &Mutex<Cache>,
    session: &SessionId,
    body: &SessionFactBody,
) -> TurnResult<()> {
    let SessionFactBody::ModelIntent { evidence, .. } = body else {
        return Ok(());
    };
    let mut originals = BTreeMap::new();
    for (expected, part) in evidence.parts() {
        if let EvidencePart::Reference {
            seq,
            section,
            sha256,
            bytes,
        } = part
        {
            if *seq == 0 || *section != expected {
                return Err(TurnError::Invalid(
                    "invalid evidence reference sequence or section".into(),
                ));
            }
            originals
                .entry(*seq)
                .or_insert_with(Vec::new)
                .push((*section, sha256, *bytes));
        }
    }
    for (seq, parts) in originals {
        let key = (session.clone(), seq);
        let cached = cache.lock().expect("evidence cache poisoned").get(&key);
        let original = if let Some(cached) = cached {
            cached
        } else {
            let original = rsi_agent_store_protocol::EvidenceOriginal::read(store, session, seq)
                .await
                .map_err(resolve_error)?
                .into_digests();
            cache
                .lock()
                .expect("evidence cache poisoned")
                .insert(key, original.clone());
            original
        };
        for (section, sha256, bytes) in parts {
            original
                .validate(section, sha256, bytes)
                .map_err(resolve_error)?;
        }
    }
    Ok(())
}

fn resolve_error(error: rsi_agent_store_protocol::EvidenceResolveError) -> TurnError {
    match error {
        rsi_agent_store_protocol::EvidenceResolveError::Store(error) => turn_store_error(error),
        rsi_agent_store_protocol::EvidenceResolveError::Invalid(message) => {
            TurnError::Invalid(message.into())
        }
    }
}

const MAX_ORIGINALS: usize = 1024;
type Key = (SessionId, u64);
#[derive(Default)]
pub(super) struct Cache {
    entries: BTreeMap<Key, rsi_agent_store_protocol::EvidenceDigests>,
    recent: VecDeque<Key>,
}
impl Cache {
    fn get(&mut self, key: &Key) -> Option<rsi_agent_store_protocol::EvidenceDigests> {
        let value = self.entries.get(key)?.clone();
        self.recent.retain(|old| old != key);
        self.recent.push_back(key.clone());
        Some(value)
    }
    fn insert(&mut self, key: Key, value: rsi_agent_store_protocol::EvidenceDigests) {
        self.recent.retain(|old| old != &key);
        self.recent.push_back(key.clone());
        self.entries.insert(key, value);
        while self.entries.len() > MAX_ORIGINALS {
            self.entries
                .remove(&self.recent.pop_front().expect("entry has recency"));
        }
    }
}
