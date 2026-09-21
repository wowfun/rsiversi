use super::{CancellationToken, Hit, ProductHistorySearch, Result, Scope, check, invalid};
use rsi_acp_protocol::observation::{ConversationId, Record, RecordKind};
use rsi_agent_session_protocol::{
    MAXIMUM_REFERENCE_SCAN_BYTES, MAXIMUM_REFERENCE_TEXT_BYTES, ReferenceBinding,
    ReferenceContentKind, ReferenceRecord, ReferenceSelection, ReferenceSource,
};
use rsi_conversation::ConversationIdentity;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

pub(super) struct Source {
    pub identity: ReferenceSource,
    pub cwd: String,
}
pub(super) struct Original {
    pub text: String,
}
pub(super) struct Document {
    pub hit: Hit,
    pub text: String,
}
pub(super) struct Batch {
    pub documents: Vec<Document>,
    pub through: u64,
    pub horizon: u64,
    pub omissions: u64,
    pub has_more: bool,
    retained: usize,
}
impl Batch {
    fn append_original<'a>(
        &mut self,
        source: &ReferenceSource,
        fields: impl Iterator<Item = (ReferenceRecord, &'a str)>,
        encoded: usize,
    ) -> Result<bool> {
        let mut documents = Vec::new();
        let mut bytes = 0;
        let mut omissions = 0;
        for (record, text) in fields {
            if text.is_empty() {
                continue;
            }
            if text.len() > MAXIMUM_REFERENCE_TEXT_BYTES || record.validate().is_err() {
                omissions += 1;
                continue;
            }
            let document = document(source, record, text, self.horizon, encoded)?;
            bytes += serde_json::to_vec(&document.hit).map_err(invalid)?.len()
                + document.text.len()
                + 128;
            documents.push(document);
        }
        if bytes > 64 * 1024 * 1024 || documents.len() > 8192 {
            return Err(invalid("one original exceeds the derived indexing budget"));
        }
        if self.retained + bytes > 64 * 1024 * 1024 || self.documents.len() + documents.len() > 8192
        {
            return Ok(false);
        }
        self.retained += bytes;
        self.documents.extend(documents);
        self.omissions += omissions;
        Ok(true)
    }
}

fn document(
    source: &ReferenceSource,
    record: ReferenceRecord,
    text: &str,
    through: u64,
    encoded: usize,
) -> Result<Document> {
    let hit = Hit {
        source: source.clone(),
        original: ReferenceSelection {
            record,
            through_seq: through,
            start: 0,
            end: text.len(),
            text_sha256: hex::encode(Sha256::digest(text.as_bytes())),
            scanned_bytes: encoded,
        },
        preview: text[..text.floor_char_boundary(text.len().min(2048))].into(),
    };
    hit.validate()?;
    Ok(Document {
        hit,
        text: text.into(),
    })
}
fn observed(source: &ReferenceSource) -> Result<(ConversationId, u64)> {
    match source {
        ReferenceSource::Observed { owner, id, epoch } if owner == "acp" => {
            Ok((ConversationId::new(id).map_err(invalid)?, *epoch))
        }
        _ => Err(invalid("unsupported observed history source")),
    }
}
/// Closed, explicitly visible external text fields. No rawInput/rawOutput or thoughts.
fn external_texts(kind: RecordKind, value: &Value) -> Vec<(ReferenceContentKind, usize, &str)> {
    fn text(part: &Value) -> Option<&str> {
        part.get("text").and_then(Value::as_str)
    }
    match kind {
        RecordKind::User => value
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(index, part)| {
                if part["type"] == "text" {
                    text(part).map(|s| (ReferenceContentKind::Human, index, s))
                } else {
                    None
                }
            })
            .collect(),
        RecordKind::Update => match value["sessionUpdate"].as_str() {
            Some("agent_message_chunk" | "user_message_chunk")
                if value["content"]["type"] == "text" =>
            {
                text(&value["content"])
                    .map(|s| {
                        vec![(
                            if value["sessionUpdate"] == "user_message_chunk" {
                                ReferenceContentKind::Human
                            } else {
                                ReferenceContentKind::Assistant
                            },
                            0,
                            s,
                        )]
                    })
                    .unwrap_or_default()
            }
            Some("tool_call" | "tool_call_update") => value["content"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(|(index, part)| {
                    if part["type"] == "content" && part["content"]["type"] == "text" {
                        text(&part["content"])
                            .map(|s| (ReferenceContentKind::ToolEvidence, index, s))
                    } else {
                        None
                    }
                })
                .collect(),
            _ => Vec::new(),
        },
        RecordKind::Permission => Vec::new(),
    }
}
impl ProductHistorySearch {
    pub(super) async fn authorize(
        &self,
        scope: &Scope,
        stop: &CancellationToken,
    ) -> Result<Source> {
        let workspace = self
            .workspaces
            .get(&scope.workspace)
            .await
            .map_err(invalid)?;
        check(stop)?;
        let source = match &scope.conversation {
            ConversationIdentity::Native(id) => {
                let header = self.store.header(id).await.map_err(invalid)?;
                Source {
                    identity: ReferenceSource::Native {
                        binding: ReferenceBinding {
                            session_id: id.clone(),
                            header_sha256: header.fingerprint().map_err(invalid)?,
                        },
                    },
                    cwd: header.canonical_cwd().into(),
                }
            }
            ConversationIdentity::External(id) => {
                let view = self.external.view(id).await.map_err(invalid)?;
                Source {
                    identity: ReferenceSource::Observed {
                        owner: "acp".into(),
                        id: id.as_str().into(),
                        epoch: view.snapshot.epoch,
                    },
                    cwd: view.snapshot.cwd,
                }
            }
        };
        check(stop)?;
        if workspace.path.to_str() != Some(&source.cwd) {
            return Err(invalid("history source is outside the requested workspace"));
        }
        Ok(source)
    }
    pub(super) async fn horizon(
        &self,
        source: &Source,
        after: u64,
        stop: &CancellationToken,
    ) -> Result<(u64, bool)> {
        let result = match &source.identity {
            ReferenceSource::Native { binding } => {
                let page = self
                    .store
                    .read_fact_window(&binding.session_id, after, 1, 1)
                    .await
                    .map_err(invalid)?;
                (page.durable_seq, page.durable_seq > after)
            }
            ReferenceSource::Observed { .. } => {
                let (id, epoch) = observed(&source.identity)?;
                let page = self
                    .external
                    .page(&id, epoch, after)
                    .await
                    .map_err(invalid)?;
                (
                    page.records.last().map_or(after, |r| r.sequence),
                    !page.records.is_empty(),
                )
            }
        };
        check(stop)?;
        Ok(result)
    }
    async fn external_value(
        &self,
        id: &ConversationId,
        epoch: u64,
        record: Record,
        stop: &CancellationToken,
    ) -> Result<Value> {
        if record.bytes == 0 || record.bytes > MAXIMUM_REFERENCE_SCAN_BYTES || record.epoch != epoch
        {
            return Err(invalid("observed original exceeds source limits"));
        }
        if let Some(value) = record.value {
            return Ok(value);
        }
        let mut bytes = Vec::with_capacity(record.bytes);
        while bytes.len() < record.bytes {
            check(stop)?;
            let part = self
                .external
                .window(id, epoch, record.sequence, bytes.len())
                .await
                .map_err(invalid)?;
            check(stop)?;
            if part.is_empty() || part.len() > 65536 || part.len() > record.bytes - bytes.len() {
                return Err(invalid("invalid observed original window"));
            }
            bytes.extend(part);
        }
        serde_json::from_slice(&bytes).map_err(invalid)
    }
    pub(super) async fn batch(
        &self,
        source: &Source,
        after: u64,
        stop: &CancellationToken,
    ) -> Result<Batch> {
        let mut batch = Batch {
            documents: Vec::new(),
            through: after,
            horizon: after,
            omissions: 0,
            has_more: false,
            retained: 0,
        };
        match &source.identity {
            ReferenceSource::Native { binding } => {
                let _validation = self
                    .store
                    .prepare_session(&binding.session_id)
                    .await
                    .map_err(invalid)?;
                check(stop)?;
                let window = self
                    .store
                    .read_fact_window(
                        &binding.session_id,
                        after,
                        256,
                        MAXIMUM_REFERENCE_SCAN_BYTES,
                    )
                    .await
                    .map_err(invalid)?;
                check(stop)?;
                batch.horizon = window.durable_seq;
                batch.has_more = window.through_seq < batch.horizon;
                batch.through = window.through_seq;
                for fact in &window.facts {
                    check(stop)?;
                    let fields = rsi_agent_references::reference_texts(fact)
                        .into_iter()
                        .map(|field| (field.record, field.text));
                    if !batch.append_original(&source.identity, fields, fact.encoded_len())? {
                        batch.through = fact.seq() - 1;
                        batch.has_more = true;
                        break;
                    }
                }
                batch.omissions += window
                    .omitted
                    .iter()
                    .filter(|record| record.seq <= batch.through)
                    .count() as u64;
            }
            ReferenceSource::Observed { .. } => {
                let (id, epoch) = observed(&source.identity)?;
                let page = self
                    .external
                    .page(&id, epoch, after)
                    .await
                    .map_err(invalid)?;
                check(stop)?;
                batch.horizon = page.records.last().map_or(after, |r| r.sequence);
                batch.has_more = page.has_more;
                let mut bytes = 256 * 1024;
                for record in page.records {
                    if record.bytes > MAXIMUM_REFERENCE_SCAN_BYTES {
                        batch.omissions += 1;
                        batch.through = record.sequence;
                        continue;
                    }
                    if bytes + record.bytes > MAXIMUM_REFERENCE_SCAN_BYTES {
                        batch.has_more = true;
                        break;
                    }
                    bytes += record.bytes;
                    let sequence = record.sequence;
                    let kind = record.kind;
                    let encoded = record.bytes;
                    let value = self.external_value(&id, epoch, record, stop).await?;
                    let fields =
                        external_texts(kind, &value)
                            .into_iter()
                            .map(|(kind, index, text)| {
                                (
                                    ReferenceRecord {
                                        sequence,
                                        kind,
                                        content_index: index,
                                    },
                                    text,
                                )
                            });
                    if !batch.append_original(&source.identity, fields, encoded)? {
                        batch.has_more = true;
                        break;
                    }
                    batch.through = sequence;
                }
            }
        }
        Ok(batch)
    }
    pub(super) async fn original(
        &self,
        source: &Source,
        hit: &Hit,
        stop: &CancellationToken,
    ) -> Result<Original> {
        hit.validate()?;
        if hit.source != source.identity {
            return Err(invalid("history source Header or observed epoch changed"));
        }
        let selected = &hit.original;
        let (text, encoded) = match &source.identity {
            ReferenceSource::Native { binding } => {
                let window = self
                    .store
                    .read_fact_window(
                        &binding.session_id,
                        selected.record.sequence - 1,
                        1,
                        MAXIMUM_REFERENCE_SCAN_BYTES,
                    )
                    .await
                    .map_err(invalid)?;
                check(stop)?;
                if window.durable_seq < selected.through_seq {
                    return Err(invalid("original cutoff is unavailable"));
                }
                let fact = window
                    .facts
                    .first()
                    .ok_or_else(|| invalid("original was omitted by the scan budget"))?;
                let field = rsi_agent_references::reference_texts(fact)
                    .into_iter()
                    .find(|field| field.record == selected.record)
                    .ok_or_else(|| invalid("original content is not exportable"))?;
                if field.text.len() > MAXIMUM_REFERENCE_TEXT_BYTES {
                    return Err(invalid("original text exceeds limit"));
                }
                (field.text.to_owned(), window.encoded_bytes)
            }
            ReferenceSource::Observed { .. } => {
                let (id, epoch) = observed(&source.identity)?;
                let page = self
                    .external
                    .page(&id, epoch, selected.record.sequence - 1)
                    .await
                    .map_err(invalid)?;
                check(stop)?;
                let record = page
                    .records
                    .into_iter()
                    .next()
                    .filter(|r| r.sequence == selected.record.sequence)
                    .ok_or_else(|| invalid("observed original is unavailable"))?;
                if record.bytes > MAXIMUM_REFERENCE_SCAN_BYTES - 512 * 1024 {
                    return Err(invalid("original exceeds retained read budget"));
                }
                if selected.through_seq != selected.record.sequence {
                    let horizon = self
                        .external
                        .page(&id, epoch, selected.through_seq - 1)
                        .await
                        .map_err(invalid)?;
                    check(stop)?;
                    if horizon
                        .records
                        .first()
                        .is_none_or(|r| r.sequence != selected.through_seq)
                    {
                        return Err(invalid("observed original cutoff is unavailable"));
                    }
                }
                let bytes = record.bytes;
                let kind = record.kind;
                let value = self.external_value(&id, epoch, record, stop).await?;
                let text = external_texts(kind, &value)
                    .into_iter()
                    .find(|(kind, index, _)| {
                        *kind == selected.record.kind && *index == selected.record.content_index
                    })
                    .map(|(_, _, text)| text)
                    .ok_or_else(|| invalid("observed content is not exportable"))?;
                if text.len() > MAXIMUM_REFERENCE_TEXT_BYTES {
                    return Err(invalid("original text exceeds limit"));
                }
                (text.to_owned(), bytes)
            }
        };
        if encoded != selected.scanned_bytes
            || text.len() != selected.end
            || hex::encode(Sha256::digest(text.as_bytes())) != selected.text_sha256
        {
            return Err(invalid("indexed candidate does not match the original"));
        }
        Ok(Original { text })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn external_text_export_excludes_thoughts_permissions_and_raw_tool_json() {
        let visible = json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"可见🦀"}});
        assert_eq!(
            external_texts(RecordKind::Update, &visible),
            vec![(ReferenceContentKind::Assistant, 0, "可见🦀")]
        );
        let thought = json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hidden"}});
        assert!(external_texts(RecordKind::Update, &thought).is_empty());
        assert!(external_texts(RecordKind::Permission, &visible).is_empty());
        let tool = json!({"sessionUpdate":"tool_call_update","rawInput":{"private":"secret"},"rawOutput":{"private":"secret"},"content":[{"type":"content","content":{"type":"text","text":"explicit evidence"}},{"type":"diff","path":"secret","newText":"excluded"}]});
        assert_eq!(
            external_texts(RecordKind::Update, &tool),
            vec![(ReferenceContentKind::ToolEvidence, 0, "explicit evidence")]
        );
    }
    #[test]
    fn many_small_fields_pause_before_the_original_without_losing_progress_or_omitting_text() {
        let source = ReferenceSource::Observed {
            owner: "acp".into(),
            id: "source".into(),
            epoch: 1,
        };
        let mut batch = Batch {
            documents: vec![],
            through: 0,
            horizon: 10,
            omissions: 0,
            has_more: true,
            retained: 0,
        };
        let fields = |sequence| {
            (0..1024).map(move |content_index| {
                (
                    ReferenceRecord {
                        sequence,
                        kind: ReferenceContentKind::Human,
                        content_index,
                    },
                    "x",
                )
            })
        };
        for sequence in 1..=8 {
            assert!(
                batch
                    .append_original(&source, fields(sequence), 8192)
                    .unwrap()
            );
        }
        let retained = batch.retained;
        assert!(!batch.append_original(&source, fields(9), 8192).unwrap());
        assert_eq!(batch.documents.len(), 8192);
        assert_eq!(batch.retained, retained);
        assert_eq!(batch.omissions, 0);
        let mut next = Batch {
            documents: vec![],
            through: 8,
            horizon: 10,
            omissions: 0,
            has_more: true,
            retained: 0,
        };
        assert!(next.append_original(&source, fields(9), 8192).unwrap());
        assert_eq!(next.documents[0].hit.original.record.sequence, 9);
    }
}
