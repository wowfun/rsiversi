use super::{Result, SessionError, SessionId, SessionStore, map_store_error};
use rsi_agent_session_protocol::{EvidencePart, RequestEvidence, SessionFactBody};
use rsi_session_protocol::{EvidencePage, EvidencePageContent, EvidenceRead};

pub(super) async fn read(
    store: &dyn SessionStore,
    session: &SessionId,
    request: &EvidenceRead,
) -> Result<EvidencePage> {
    request.validate()?;
    let page = store
        .read_facts(session, request.intent_seq - 1, 1)
        .await
        .map_err(map_store_error)?;
    let fact = page
        .facts
        .first()
        .filter(|fact| fact.seq() == request.intent_seq)
        .ok_or_else(|| SessionError::NotFound("request intent".into()))?;
    let SessionFactBody::ModelIntent { evidence, .. } = fact.body() else {
        return Err(SessionError::Invalid(
            "selected source is not a request intent".into(),
        ));
    };
    let content = match evidence {
        RequestEvidence::Unavailable { reason } => {
            EvidencePageContent::Unavailable { reason: *reason }
        }
        RequestEvidence::Available { .. } => {
            let part = evidence
                .part(request.section)
                .expect("available evidence has every section");
            match part {
                EvidencePart::Inline { text, sha256 } => window(text, sha256, request)?,
                EvidencePart::Reference {
                    seq,
                    section,
                    sha256,
                    bytes,
                } => {
                    if *seq >= request.intent_seq {
                        return Err(SessionError::Invalid(
                            "request evidence original must precede its reference".into(),
                        ));
                    }
                    let original =
                        rsi_agent_store_protocol::EvidenceOriginal::read(store, session, *seq)
                            .await
                            .map_err(resolve_error)?;
                    let text = original
                        .text(*section, sha256, *bytes)
                        .map_err(resolve_error)?;
                    window(text, sha256, request)?
                }
            }
        }
    };
    let response = EvidencePage {
        intent_seq: request.intent_seq,
        section: request.section,
        content,
    };
    response.validate(request)?;
    Ok(response)
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
