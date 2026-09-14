#![allow(clippy::result_large_err)] // Uses the Driver's existing owned failure taxonomy.
use super::*;
use rsi_agent_session_protocol::{
    EvidenceContentCount, EvidenceContentKind, EvidencePart, EvidenceSection, EvidenceUnavailable,
    RequestEvidence,
};
use rsi_ai_protocol::{LanguageRequest, MessageContent, MessageRole};
use std::collections::VecDeque;

// A message has <=256 direct items, or one ToolResult with <=256 leaf items.
// Capture sees only validated LanguageRequest values; growth of that contract must
// revisit the durable manifest limit before compilation can succeed.
const _: () = assert!(
    rsi_ai_protocol::MAX_MESSAGES * (rsi_ai_protocol::MAX_BLOCKS_PER_MESSAGE + 1)
        <= rsi_agent_session_protocol::MAXIMUM_EVIDENCE_CONTENT_COUNT
);

/// The sole built semantic request contributes these sections before it moves into Prepare.
pub(super) struct Capture {
    configuration: Value,
    system: String,
    tools: String,
    manifest: Vec<EvidenceContentCount>,
}
impl Capture {
    pub(super) fn new(request: &LanguageRequest) -> std::result::Result<Self, DriveFailure> {
        let system = request
            .messages()
            .iter()
            .filter(|message| {
                matches!(message.role(), MessageRole::System | MessageRole::Developer)
            })
            .collect::<Vec<_>>();
        let mut counts = BTreeMap::<EvidenceContentKind, (u32, u64)>::new();
        for message in request.messages().iter().filter(|message| {
            !matches!(message.role(), MessageRole::System | MessageRole::Developer)
        }) {
            for content in message.content() {
                count(content, &mut counts);
            }
        }
        Ok(Self {
            configuration: serde_json::json!({"settings":request.settings(),"response_format":request.response_format(),"extensions":request.extensions()}),
            system: serde_json::to_string(&system).map_err(fatal)?,
            tools: serde_json::to_string(&serde_json::json!({"definitions":request.tools(),"hosted":request.hosted_tools(),"choice":request.tool_choice()})).map_err(fatal)?,
            manifest: counts.into_iter().map(|(kind,(count,bytes))|EvidenceContentCount {kind,count,bytes}).collect(),
        })
    }
    pub(super) fn finish(
        mut self,
        snapshot: &PreparedCallSnapshot,
    ) -> std::result::Result<RequestEvidence, DriveFailure> {
        self.configuration["prepared"] = serde_json::json!({
            "deployment":snapshot.deployment_id,"model":snapshot.model,"endpoint_fingerprint":snapshot.endpoint_fingerprint,
            "provider_family":snapshot.provider_family,"protocol":snapshot.protocol,"transport":snapshot.transport,
            "config_generation":snapshot.config_generation,"language_settings":snapshot.language_settings,
        });
        let evidence = RequestEvidence::Available {
            configuration: EvidencePart::inline(
                serde_json::to_string(&self.configuration).map_err(fatal)?,
            ),
            system: EvidencePart::inline(self.system),
            tools: EvidencePart::inline(self.tools),
            manifest: self.manifest,
        };
        if evidence
            .parts()
            .iter()
            .map(|(_, part)| part.bytes())
            .sum::<usize>()
            > rsi_agent_session_protocol::MAXIMUM_REQUEST_EVIDENCE_BYTES
        {
            return Ok(RequestEvidence::Unavailable {
                reason: EvidenceUnavailable::Budget,
            });
        }
        Ok(evidence)
    }
}

fn count(content: &MessageContent, counts: &mut BTreeMap<EvidenceContentKind, (u32, u64)>) {
    let (kind, bytes) = match content {
        MessageContent::Text { text } => (EvidenceContentKind::Text, text.len() as u64),
        MessageContent::Reasoning { text, .. } => {
            (EvidenceContentKind::Reasoning, text.len() as u64)
        }
        MessageContent::ToolCall(call) => {
            (EvidenceContentKind::ToolCall, call.arguments.len() as u64)
        }
        MessageContent::Image(media) => (EvidenceContentKind::Image, media.byte_len()),
        MessageContent::Audio(media) => (EvidenceContentKind::Audio, media.byte_len()),
        MessageContent::ToolResult { content, .. } => {
            for item in content {
                count(item, counts);
            }
            (EvidenceContentKind::ToolResult, 0)
        }
    };
    // LanguageRequest admission already bounds content count, text and declared media bytes.
    let entry = counts.entry(kind).or_default();
    entry.0 += 1;
    entry.1 += bytes;
}

#[derive(Debug, Default)]
pub(super) struct EvidenceCache(VecDeque<Metadata>);
#[derive(Debug)]
struct Metadata {
    session: SessionId,
    section: EvidenceSection,
    seq: u64,
    digest: String,
    bytes: u32,
}
impl EvidenceCache {
    pub(super) fn contains(&self, session: &SessionId) -> bool {
        self.0.iter().any(|entry| &entry.session == session)
    }
    pub(super) fn deduplicate(&mut self, session: &SessionId, evidence: &mut RequestEvidence) {
        for section in [
            EvidenceSection::Configuration,
            EvidenceSection::System,
            EvidenceSection::Tools,
        ] {
            let Some(part) = evidence.part_mut(section) else {
                continue;
            };
            let Some(index) = self.0.iter().position(|entry| {
                &entry.session == session
                    && entry.section == section
                    && entry.digest == part.sha256()
                    && entry.bytes as usize == part.bytes()
            }) else {
                continue;
            };
            let entry = self.0.remove(index).expect("matched metadata");
            *part = EvidencePart::Reference {
                seq: entry.seq,
                section,
                sha256: entry.digest.clone(),
                bytes: entry.bytes,
            };
            self.0.push_back(entry);
        }
    }
    pub(super) fn observe(&mut self, session: &SessionId, fact: &SessionFact) {
        let SessionFactBody::ModelIntent { evidence, .. } = fact.body() else {
            return;
        };
        for (section, part) in evidence.parts() {
            {
                let seq = match part {
                    EvidencePart::Inline { .. } => fact.seq(),
                    EvidencePart::Reference { seq, .. } => *seq,
                };
                self.0.retain(|entry| {
                    !(&entry.session == session
                        && entry.section == section
                        && entry.digest == part.sha256())
                });
                if self.0.len() == 64 {
                    self.0.pop_front();
                }
                self.0.push_back(Metadata {
                    session: session.clone(),
                    section,
                    seq,
                    digest: part.sha256().into(),
                    bytes: u32::try_from(part.bytes()).expect("validated evidence fits 16 MiB"),
                });
            }
        }
    }
}

impl Driver {
    pub(super) async fn warm_evidence(
        &self,
        claim: &TurnClaim,
    ) -> std::result::Result<(), DriveFailure> {
        if self
            .evidence
            .lock()
            .map_err(|_| fatal("evidence cache lock poisoned"))?
            .contains(claim.session_id())
        {
            return Ok(());
        }
        // One bounded recent slice suffices even when it contains direct references
        // to older originals. Kernel revalidates those originals before publication.
        let page = self
            .turns
            .read_facts(claim, claim.live_seq().saturating_sub(128), 128)
            .await
            .map_err(fatal)?;
        let mut cache = self
            .evidence
            .lock()
            .map_err(|_| fatal("evidence cache lock poisoned"))?;
        for fact in &page.facts {
            cache.observe(claim.session_id(), fact);
        }
        Ok(())
    }
}

pub(super) fn budget_fallback(bodies: &[SessionFactBody]) -> Option<Vec<SessionFactBody>> {
    let [
        SessionFactBody::ModelIntent {
            turn_id,
            effect_id,
            snapshot,
            purpose,
            price_quote,
            evidence: RequestEvidence::Available { .. },
        },
    ] = bodies
    else {
        return None;
    };
    Some(vec![SessionFactBody::ModelIntent {
        turn_id: turn_id.clone(),
        effect_id: effect_id.clone(),
        snapshot: snapshot.clone(),
        purpose: purpose.clone(),
        price_quote: price_quote.clone(),
        evidence: RequestEvidence::Unavailable {
            reason: EvidenceUnavailable::Budget,
        },
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inter_session_eviction_keeps_missing_sections_inline_and_repopulates_them() {
        let context = rsi_ai_testkit::language_context(
            "fixture",
            "fixture",
            "model",
            None,
            Arc::new(rsi_ai_testkit::InMemoryMediaResolver::default()),
            0,
        );
        let request = LanguageRequest::new(vec![
            rsi_ai_protocol::Message::user(vec![MessageContent::Text { text: "x".into() }])
                .unwrap(),
        ])
        .unwrap();
        let evidence = Capture::new(&request)
            .unwrap()
            .finish(context.snapshot())
            .unwrap();
        let intent = |seq, evidence| {
            SessionFact::new(
                seq,
                1,
                SessionFactBody::ModelIntent {
                    turn_id: rsi_agent_session_protocol::TurnId::new("turn").unwrap(),
                    effect_id: EffectId::new(format!("model-{seq}")).unwrap(),
                    snapshot: context.snapshot().clone(),
                    purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
                    price_quote: None,
                    evidence,
                },
            )
            .unwrap()
        };
        let original = intent(1, evidence.clone());
        let session = SessionId::new("target").unwrap();
        let mut cache = EvidenceCache::default();
        cache.observe(&session, &original);
        for index in 0..21 {
            cache.observe(
                &SessionId::new(format!("other-{index}")).unwrap(),
                &original,
            );
        }
        assert_eq!(cache.0.len(), 64);
        assert!(cache.contains(&session));
        let mut partial = evidence.clone();
        cache.deduplicate(&session, &mut partial);
        for (section, part) in partial.parts() {
            if section == EvidenceSection::Tools {
                assert!(matches!(part, EvidencePart::Reference { seq: 1, .. }));
            } else {
                assert!(matches!(part, EvidencePart::Inline { .. }));
            }
            let source = evidence.part(section).unwrap();
            assert_eq!(
                (part.sha256(), part.bytes()),
                (source.sha256(), source.bytes())
            );
        }
        cache.observe(&session, &intent(2, partial));
        let mut next = evidence;
        cache.deduplicate(&session, &mut next);
        for (section, part) in next.parts() {
            let original_seq = if section == EvidenceSection::Tools {
                1
            } else {
                2
            };
            assert!(matches!(part, EvidencePart::Reference { seq, .. } if *seq == original_seq));
        }
        assert_eq!(cache.0.len(), 64);
    }

    #[test]
    fn maximum_admitted_message_content_fits_the_evidence_manifest() {
        let message = rsi_ai_protocol::Message::user(vec![
            MessageContent::Text { text: "x".into() };
            rsi_ai_protocol::MAX_BLOCKS_PER_MESSAGE
        ])
        .unwrap();
        let request = LanguageRequest::new(vec![message; rsi_ai_protocol::MAX_MESSAGES]).unwrap();
        let capture = Capture::new(&request).unwrap();
        assert_eq!(capture.manifest.len(), 1);
        assert_eq!(
            capture.manifest[0].count as usize,
            rsi_ai_protocol::MAX_MESSAGES * rsi_ai_protocol::MAX_BLOCKS_PER_MESSAGE
        );
        assert!(
            capture.manifest[0].count as usize
                <= rsi_agent_session_protocol::MAXIMUM_EVIDENCE_CONTENT_COUNT
        );
    }
}
