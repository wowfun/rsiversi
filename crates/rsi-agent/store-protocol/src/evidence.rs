//! Exact binding of references to immutable, original inline evidence.
use crate::{SessionStore, StoreError};
use rsi_agent_session_protocol::{
    EvidencePart, EvidenceSection, SessionFact, SessionFactBody, SessionId,
};

/// A failed Store lookup or an invalid original binding.
#[derive(Debug, thiserror::Error)]
pub enum EvidenceResolveError {
    /// The owning Store could not read the original.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The requested original does not satisfy the reference contract.
    #[error("{0}")]
    Invalid(&'static str),
}

/// Text-free descriptors derived from one validated immutable `ModelIntent`.
#[derive(Clone, Debug)]
pub struct EvidenceDigests([Option<(String, usize)>; 3]);
impl EvidenceDigests {
    /// Checks a reference against an original inline section, never another reference.
    pub fn validate(
        &self,
        section: EvidenceSection,
        sha256: &str,
        bytes: u32,
    ) -> Result<(), EvidenceResolveError> {
        if self.0[index(section)]
            .as_ref()
            .is_some_and(|(digest, length)| digest == sha256 && *length == bytes as usize)
        {
            Ok(())
        } else {
            Err(EvidenceResolveError::Invalid(
                "evidence reference does not match an original inline section",
            ))
        }
    }
}

/// One original with descriptors for admission and borrowed text for paging.
#[derive(Debug)]
pub struct EvidenceOriginal {
    fact: SessionFact,
    digests: EvidenceDigests,
}
impl EvidenceOriginal {
    /// Reads exactly one sequence in the requested Session.
    pub async fn read(
        store: &dyn SessionStore,
        session: &SessionId,
        seq: u64,
    ) -> Result<Self, EvidenceResolveError> {
        let after = seq.checked_sub(1).ok_or(EvidenceResolveError::Invalid(
            "invalid evidence reference sequence",
        ))?;
        let mut page = store.read_facts(session, after, 1).await?;
        let fact = page.facts.pop().filter(|fact| fact.seq() == seq).ok_or(
            EvidenceResolveError::Invalid(
                "evidence reference target is unavailable in this Session",
            ),
        )?;
        let SessionFactBody::ModelIntent { evidence, .. } = fact.body() else {
            return Err(EvidenceResolveError::Invalid(
                "evidence reference target is not a ModelIntent",
            ));
        };
        let mut digests = EvidenceDigests([None, None, None]);
        for (section, part) in evidence.parts() {
            if let EvidencePart::Inline { text, sha256 } = part {
                digests.0[index(section)] = Some((sha256.clone(), text.len()));
            }
        }
        Ok(Self { fact, digests })
    }
    /// Transfers only bounded descriptors, releasing the materialized text.
    pub fn into_digests(self) -> EvidenceDigests {
        self.digests
    }
    /// Returns original text only after the shared binding check succeeds.
    pub fn text(
        &self,
        section: EvidenceSection,
        sha256: &str,
        bytes: u32,
    ) -> Result<&str, EvidenceResolveError> {
        self.digests.validate(section, sha256, bytes)?;
        let SessionFactBody::ModelIntent { evidence, .. } = self.fact.body() else {
            unreachable!("validated original")
        };
        let Some(EvidencePart::Inline { text, .. }) = evidence.part(section) else {
            unreachable!("validated inline binding")
        };
        Ok(text)
    }
}
const fn index(section: EvidenceSection) -> usize {
    match section {
        EvidenceSection::Configuration => 0,
        EvidenceSection::System => 1,
        EvidenceSection::Tools => 2,
    }
}
