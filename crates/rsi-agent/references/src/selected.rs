use super::{
    AgentMessageContent, CancellationToken, FrozenReference, InputMessageSource,
    MAXIMUM_REFERENCE_PREVIEW_BYTES, MAXIMUM_REFERENCE_SCAN_BYTES, MAXIMUM_REFERENCE_TEXT_BYTES,
    ModelEventPurpose, ReferenceBinding, ReferenceCapture, ReferenceMetadata,
    ReferenceSnapshotEnvelope, ReferenceSnapshotRef, ReferenceSource, References, Result,
    SessionFact, SessionFactBody, SessionHeader, SessionStore, Sha256, check, export, invalid,
};
use rsi_agent_session_protocol::{ReferenceContentKind, ReferenceRecord, ReferenceSelection};
use rsi_ai_protocol::{ContentDelta, LanguageEvent};
use sha2::Digest as _;

/// Borrowed explicit text export; no provider inputs, reasoning or raw Tool JSON.
#[derive(Debug)]
pub struct ReferenceText<'a> {
    /// Original source coordinate, never an index-generated identity.
    pub record: ReferenceRecord,
    /// Full original field. Callers must enforce their own aggregate response budget.
    pub text: &'a str,
}

/// Enumerates the closed export surface of one already validated Fact.
/// Oversized text remains borrowed so the indexing owner can report an omission.
pub fn reference_texts(fact: &SessionFact) -> Vec<ReferenceText<'_>> {
    let item = |kind, content_index, text| ReferenceText {
        record: ReferenceRecord {
            sequence: fact.seq(),
            kind,
            content_index,
        },
        text,
    };
    match fact.body() {
        SessionFactBody::TurnAccepted { text, .. } => {
            vec![item(ReferenceContentKind::Human, 0, text)]
        }
        SessionFactBody::ImageRequested { request, .. } => {
            vec![item(ReferenceContentKind::Human, 0, request.prompt())]
        }
        SessionFactBody::InputMessageEntered {
            source: InputMessageSource::Human { .. },
            content,
            ..
        } => content
            .iter()
            .enumerate()
            .filter_map(|(i, part)| match part {
                AgentMessageContent::Text { text } => {
                    Some(item(ReferenceContentKind::Human, i, text))
                }
                _ => None,
            })
            .collect(),
        SessionFactBody::ModelEvent {
            purpose: ModelEventPurpose::Conversation,
            event:
                LanguageEvent::ContentDelta {
                    index,
                    delta: ContentDelta::Text(text),
                },
            ..
        } => vec![item(ReferenceContentKind::Assistant, *index as usize, text)],
        SessionFactBody::ToolResult { result, .. } => result
            .content
            .iter()
            .enumerate()
            .filter_map(|(i, part)| match part {
                rsi_tools_protocol::ToolContent::Text { text } => {
                    Some(item(ReferenceContentKind::ToolEvidence, i, text))
                }
                rsi_tools_protocol::ToolContent::Image { .. } => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn envelope(
    source: ReferenceSource,
    target: &SessionHeader,
    selection: ReferenceSelection,
    text: &str,
) -> Result<export::ValidatedEnvelope> {
    selection.validate().map_err(invalid)?;
    if text.len() > MAXIMUM_REFERENCE_TEXT_BYTES
        || hex::encode(Sha256::digest(text.as_bytes())) != selection.text_sha256
    {
        return Err(invalid("selected original text mismatch or limit"));
    }
    let text = text
        .get(selection.start..selection.end)
        .ok_or_else(|| invalid("selection must be within the original at UTF-8 boundaries"))?
        .to_owned();
    export::ValidatedEnvelope::new(ReferenceSnapshotEnvelope {
        version: 2,
        metadata: ReferenceMetadata {
            source,
            target: ReferenceBinding {
                session_id: target.session_id().clone(),
                header_sha256: target.fingerprint().map_err(invalid)?,
            },
            capture: ReferenceCapture::Selected { selection },
            text_bytes: text.len(),
        },
        preview: text[..text.floor_char_boundary(text.len().min(MAXIMUM_REFERENCE_PREVIEW_BYTES))]
            .into(),
        text,
    })
}

async fn publish(
    store: &dyn SessionStore,
    envelope: export::ValidatedEnvelope,
    stop: &CancellationToken,
) -> Result<FrozenReference> {
    check(stop)?;
    let bytes = serde_json::to_vec(&envelope).map_err(invalid)?.into();
    let object = store.put_cas(bytes).await?;
    check(stop)?;
    envelope.into_frozen(ReferenceSnapshotRef {
        sha256: object.sha256,
        byte_len: object.byte_len,
    })
}

/// Original observed text reread by its product owner after authorization.
/// This trusted in-process value is never deserialized from a search hit or API input.
/// The caller owns workspace authorization, epoch/record lookup and pre-body limits.
#[derive(Debug)]
pub struct ObservedReferenceText {
    /// Observed provenance (native is rejected).
    pub source: ReferenceSource,
    /// Canonical workspace established by the source owner.
    pub canonical_cwd: String,
    /// Exact original field obtained from the journal.
    pub text: String,
    /// Validated original coordinate, cutoff and encoded bytes.
    pub selection: ReferenceSelection,
}

impl References {
    /// Rereads one precise native original, even outside the current suffix.
    pub async fn capture_selected(
        &self,
        source: ReferenceBinding,
        target: SessionHeader,
        selection: ReferenceSelection,
        cancellation: CancellationToken,
    ) -> Result<FrozenReference> {
        source.validate().map_err(invalid)?;
        target.validate().map_err(invalid)?;
        selection.validate().map_err(invalid)?;
        self.run(cancellation, move |store, stop, _| async move {
            let header = store.header(&source.session_id).await?;
            check(&stop)?;
            if header.fingerprint().map_err(invalid)? != source.header_sha256
                || header.canonical_cwd() != target.canonical_cwd()
            {
                return Err(invalid("selected source Header or workspace mismatch"));
            }
            let _validation = store.prepare_session(&source.session_id).await?;
            check(&stop)?;
            let window = store
                .read_fact_window(
                    &source.session_id,
                    selection.record.sequence - 1,
                    1,
                    MAXIMUM_REFERENCE_SCAN_BYTES,
                )
                .await?;
            check(&stop)?;
            if window.durable_seq < selection.through_seq
                || !window.omitted.is_empty()
                || window.encoded_bytes != selection.scanned_bytes
            {
                return Err(invalid(
                    "selected original cutoff or encoded length mismatch",
                ));
            }
            let fact = window
                .facts
                .first()
                .ok_or_else(|| invalid("selected original is unavailable"))?;
            let original = reference_texts(fact)
                .into_iter()
                .find(|text| text.record == selection.record)
                .ok_or_else(|| invalid("selected content is not exportable"))?;
            let envelope = envelope(
                ReferenceSource::Native { binding: source },
                &target,
                selection,
                original.text,
            )?;
            publish(&*store, envelope, &stop).await
        })
        .await
    }

    /// Freezes a trusted observed original without reinterpreting it as native Facts.
    pub async fn capture_observed(
        &self,
        original: ObservedReferenceText,
        target: SessionHeader,
        cancellation: CancellationToken,
    ) -> Result<FrozenReference> {
        target.validate().map_err(invalid)?;
        if original.source.native().is_some() || original.canonical_cwd != target.canonical_cwd() {
            return Err(invalid("observed source kind or workspace mismatch"));
        }
        let envelope = envelope(original.source, &target, original.selection, &original.text)?;
        self.run(cancellation, move |store, stop, _| async move {
            publish(&*store, envelope, &stop).await
        })
        .await
    }
}
