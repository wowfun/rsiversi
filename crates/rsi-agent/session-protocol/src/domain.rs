//! Mechanically bounded complete domain state; semantic validation belongs to its codec.

use crate::{Result, SessionError, compact_json_len, validate_identifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Opaque current state with its exact CAS predecessor for the next mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainStateView {
    /// Current durable revision.
    pub revision: DomainRevision,
    /// Complete state; only its exact codec may perform semantic decode.
    pub snapshot: DomainSnapshot,
}

/// Maximum encoded bytes in one complete domain value.
pub const MAXIMUM_DOMAIN_STATE_BYTES: usize = 256 * 1024;
/// Maximum distinct domains admitted by one Session composition.
pub const MAXIMUM_SESSION_DOMAINS: usize = 64;
/// Maximum encoded bytes in one frozen initial domain set.
pub const MAXIMUM_DOMAIN_BASELINE_BYTES: usize = 1024 * 1024;

/// Exact domain name and semantic state codec version.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DomainIdentity {
    id: String,
    version: u32,
}

impl DomainIdentity {
    /// Validates one name and nonzero version. Versions have no implicit compatibility.
    pub fn new(id: impl Into<String>, version: u32) -> Result<Self> {
        let id = id.into();
        validate_identifier("domain", &id)?;
        if version == 0 {
            return Err(SessionError::Invalid(
                "domain codec version must be nonzero".into(),
            ));
        }
        Ok(Self { id, version })
    }

    /// Returns the durable domain name.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the exact required state codec version.
    pub const fn version(&self) -> u32 {
        self.version
    }
}

impl<'de> Deserialize<'de> for DomainIdentity {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            id: String,
            version: u32,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.id, wire.version).map_err(serde::de::Error::custom)
    }
}

/// Checked per-domain revision. Zero denotes absence; the first commit has revision one.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct DomainRevision(u64);

impl DomainRevision {
    /// Creates a revision from an exact durable counter or expected value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    /// Returns the exact counter.
    pub const fn get(self) -> u64 {
        self.0
    }
    /// Advances once, rejecting overflow before mutation.
    pub fn next(self) -> Result<Self> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| SessionError::Invalid("domain revision overflow".into()))
    }
}

/// Complete bounded JSON value. Null is a value, never an absent-state sentinel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainStateValue {
    value: std::sync::Arc<serde_json::Value>,
    encoded_len: usize,
}

impl DomainStateValue {
    /// Validates JSON structure and canonical byte size at the durable boundary.
    pub fn new(value: serde_json::Value) -> Result<Self> {
        rsi_ai_protocol::validate_json_structure(&value)
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let encoded_len = compact_json_len(&value)?;
        if encoded_len > MAXIMUM_DOMAIN_STATE_BYTES {
            return Err(SessionError::TooLarge {
                kind: "domain state",
                maximum: MAXIMUM_DOMAIN_STATE_BYTES,
                actual: encoded_len,
            });
        }
        Ok(Self {
            value: std::sync::Arc::new(value),
            encoded_len,
        })
    }

    /// Serializes typed linked state with a bounded output writer before allocating JSON.
    pub fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Self> {
        struct Writer(Vec<u8>);
        impl std::io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() > MAXIMUM_DOMAIN_STATE_BYTES.saturating_sub(self.0.len()) {
                    return Err(std::io::Error::other(
                        "domain state exceeds encoded byte limit",
                    ));
                }
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Writer(Vec::new());
        serde_json::to_writer(&mut writer, value)
            .map_err(|error| SessionError::Encoding(error.to_string()))?;
        let value = serde_json::from_slice(&writer.0)
            .map_err(|error| SessionError::Encoding(error.to_string()))?;
        Self::new(value)
    }

    /// Borrows the immutable opaque value without requiring a domain codec.
    pub fn value(&self) -> &serde_json::Value {
        &self.value
    }
    /// Returns canonical encoded bytes, excluding the containing control envelope.
    pub const fn encoded_len(&self) -> usize {
        self.encoded_len
    }
}

impl Serialize for DomainStateValue {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DomainStateValue {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Self::new(serde_json::Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Opaque complete state paired with the codec required for typed interpretation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DomainSnapshot {
    identity: DomainIdentity,
    state: DomainStateValue,
}

impl DomainSnapshot {
    /// Pairs already bounded values without asserting codec availability or semantic validity.
    pub const fn new(identity: DomainIdentity, state: DomainStateValue) -> Self {
        Self { identity, state }
    }
    /// Returns the required codec identity.
    pub const fn identity(&self) -> &DomainIdentity {
        &self.identity
    }
    /// Returns the opaque state.
    pub const fn state(&self) -> &DomainStateValue {
        &self.state
    }
    /// Returns the canonical snapshot size, including its identity and state wrapper.
    pub fn encoded_len(&self) -> Result<usize> {
        compact_json_len(self)
    }
}

/// Durable provenance assigned by Kernel admission, never by a typed proposal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DomainMutationSource {
    /// Frozen complete initial domain set in the fresh Session commit.
    Baseline,
    /// Authenticated application command routed by its owning Session service.
    Command {
        /// Exact logical request selected by Kernel dispatch, including CAS and arguments.
        invocation: crate::SessionCommandInvocation,
    },
    /// Execution contribution charged to its actual admitted Turn.
    Turn {
        /// Kernel-bound originating Turn, including Hook and Tool contributions.
        turn_id: crate::TurnId,
    },
}

impl DomainMutationSource {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Baseline | Self::Turn { .. } => Ok(()),
            Self::Command { invocation } => {
                if matches!(
                    invocation.expected_revision,
                    crate::CommandRevision::Draft { .. }
                ) {
                    return Err(SessionError::Invalid(
                        "durable command has a draft revision".into(),
                    ));
                }
                Ok(())
            }
        }
    }
}

/// Complete replacement at one exact predecessor revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DomainStateUpdate {
    expected_revision: DomainRevision,
    snapshot: DomainSnapshot,
}

impl DomainStateUpdate {
    /// Creates a replacement whose successor revision is representable.
    pub fn new(expected_revision: DomainRevision, snapshot: DomainSnapshot) -> Result<Self> {
        expected_revision.next()?;
        Ok(Self {
            expected_revision,
            snapshot,
        })
    }
    /// Returns the exact required predecessor revision.
    pub const fn expected_revision(&self) -> DomainRevision {
        self.expected_revision
    }
    /// Returns the committed successor, checked at construction and decode.
    pub fn revision(&self) -> DomainRevision {
        DomainRevision::new(self.expected_revision.get() + 1)
    }
    /// Returns the full state and its exact codec identity.
    pub const fn snapshot(&self) -> &DomainSnapshot {
        &self.snapshot
    }
}

impl<'de> Deserialize<'de> for DomainStateUpdate {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            expected_revision: DomainRevision,
            snapshot: DomainSnapshot,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.expected_revision, wire.snapshot).map_err(serde::de::Error::custom)
    }
}

/// Exact contiguous canonical Facts belonging to a mixed domain request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DomainFactSpan {
    first_seq: u64,
    count: u64,
    bodies_sha256: String,
}

impl DomainFactSpan {
    /// Binds a bounded contiguous nonempty sequence without copying Fact bodies.
    pub fn from_facts<'a>(facts: impl IntoIterator<Item = &'a crate::SessionFact>) -> Result<Self> {
        let mut builder = DomainFactSpanBuilder::default();
        for fact in facts {
            builder.push(fact)?;
        }
        builder.finish()
    }
    fn new(first_seq: u64, count: u64, bodies_sha256: String) -> Result<Self> {
        if first_seq == 0
            || count == 0
            || count > crate::MAXIMUM_FACTS_PER_READ as u64
            || first_seq.checked_add(count - 1).is_none()
            || bodies_sha256.len() != 64
            || !bodies_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(SessionError::Invalid("invalid domain Fact span".into()));
        }
        Ok(Self {
            first_seq,
            count,
            bodies_sha256,
        })
    }
    /// Returns the first bound canonical Fact sequence.
    pub const fn first_seq(&self) -> u64 {
        self.first_seq
    }
    /// Returns the final bound canonical Fact sequence.
    pub const fn last_seq(&self) -> u64 {
        self.first_seq + (self.count - 1)
    }
    /// Returns the number of bound Facts.
    pub const fn count(&self) -> u64 {
        self.count
    }
    /// Returns their source-ordered body digest, excluding sequence and timestamp.
    pub fn bodies_sha256(&self) -> &str {
        &self.bodies_sha256
    }
}

/// Streaming verifier for a mixed request; retains no Fact bodies.
#[derive(Debug)]
pub struct DomainFactSpanBuilder {
    first_seq: Option<u64>,
    count: u64,
    writer: DigestWriter,
}

impl Default for DomainFactSpanBuilder {
    fn default() -> Self {
        let mut writer = DigestWriter(Sha256::new());
        writer.0.update(b"rsi-agent-domain-facts-v1\0[");
        Self {
            first_seq: None,
            count: 0,
            writer,
        }
    }
}

impl DomainFactSpanBuilder {
    /// Adds the next canonical Fact without retaining or copying its body.
    pub fn push(&mut self, fact: &crate::SessionFact) -> Result<()> {
        let first = self.first_seq.unwrap_or(fact.seq());
        if self.count >= crate::MAXIMUM_FACTS_PER_READ as u64
            || first.checked_add(self.count) != Some(fact.seq())
        {
            return Err(SessionError::Invalid(
                "domain Fact span is not bounded and contiguous".into(),
            ));
        }
        if self.count > 0 {
            self.writer.0.update(b",");
        }
        serde_json::to_writer(&mut self.writer, fact.body())
            .map_err(|error| SessionError::Encoding(error.to_string()))?;
        self.first_seq = Some(first);
        self.count += 1;
        Ok(())
    }
    /// Completes the nonempty source-ordered body digest.
    pub fn finish(mut self) -> Result<DomainFactSpan> {
        let first = self
            .first_seq
            .ok_or_else(|| SessionError::Invalid("domain Fact span is empty".into()))?;
        self.writer.0.update(b"]");
        DomainFactSpan::new(first, self.count, hex::encode(self.writer.0.finalize()))
    }
}

impl<'de> Deserialize<'de> for DomainFactSpan {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            first_seq: u64,
            count: u64,
            bodies_sha256: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.first_seq, wire.count, wire.bodies_sha256).map_err(serde::de::Error::custom)
    }
}

/// One bounded canonical domain request; all replacements and its receipt commit together.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DomainStateCommit {
    request_id: Option<crate::DomainRequestId>,
    source: DomainMutationSource,
    updates: Vec<DomainStateUpdate>,
    fact_span: Option<DomainFactSpan>,
    request_sha256: String,
}

impl DomainStateCommit {
    /// Validates provenance, distinct names, revision classes and aggregate size.
    /// Replacements must already be ordered by durable domain name.
    pub fn new(
        request_id: Option<crate::DomainRequestId>,
        source: DomainMutationSource,
        updates: Vec<DomainStateUpdate>,
    ) -> Result<Self> {
        source.validate()?;
        let baseline = matches!(source, DomainMutationSource::Baseline);
        if baseline != request_id.is_none() {
            return Err(SessionError::Invalid(
                "only a domain baseline omits a request identity".into(),
            ));
        }
        if let DomainMutationSource::Command { invocation } = &source
            && Some(&invocation.request_id) != request_id.as_ref()
        {
            return Err(SessionError::Invalid(
                "command and domain request identities differ".into(),
            ));
        }
        if updates.is_empty() || updates.len() > MAXIMUM_SESSION_DOMAINS {
            return Err(SessionError::Invalid(
                "domain commit must contain 1..=64 complete replacements".into(),
            ));
        }
        if updates
            .windows(2)
            .any(|pair| pair[0].snapshot.identity.id() >= pair[1].snapshot.identity.id())
        {
            return Err(SessionError::Invalid(
                "domain replacements must use distinct ascending names".into(),
            ));
        }
        let mut bytes = 0_usize;
        for update in &updates {
            if baseline != (update.expected_revision.get() == 0) {
                return Err(SessionError::Invalid(
                    "only a baseline can create domain revision one".into(),
                ));
            }
            bytes = bytes
                .checked_add(update.snapshot.encoded_len()?)
                .ok_or_else(|| SessionError::Invalid("domain aggregate size overflow".into()))?;
        }
        if bytes > MAXIMUM_DOMAIN_BASELINE_BYTES {
            return Err(SessionError::TooLarge {
                kind: "domain replacement set",
                maximum: MAXIMUM_DOMAIN_BASELINE_BYTES,
                actual: bytes,
            });
        }
        let request_sha256 = request_digest(&source, &updates, None)?;
        Ok(Self {
            request_id,
            source,
            updates,
            fact_span: None,
            request_sha256,
        })
    }
    /// Binds all accompanying Facts of a Turn-origin request to its durable receipt.
    pub fn with_facts<'a>(
        mut self,
        facts: impl IntoIterator<Item = &'a crate::SessionFact>,
    ) -> Result<Self> {
        let DomainMutationSource::Turn { turn_id } = &self.source else {
            return Err(SessionError::Invalid(
                "only a Turn domain request binds Facts".into(),
            ));
        };
        let facts = facts
            .into_iter()
            .take(crate::MAXIMUM_FACTS_PER_READ + 1)
            .collect::<Vec<_>>();
        if facts.iter().any(|fact| fact.body().turn_id() != turn_id) {
            return Err(SessionError::Invalid(
                "domain request Facts changed the originating Turn".into(),
            ));
        }
        self.fact_span = if facts.is_empty() {
            None
        } else {
            Some(DomainFactSpan::from_facts(facts)?)
        };
        self.request_sha256 = request_digest(&self.source, &self.updates, self.fact_span.as_ref())?;
        Ok(self)
    }
    /// Returns the exact canonical Fact interval, absent for control-only requests.
    pub const fn fact_span(&self) -> Option<&DomainFactSpan> {
        self.fact_span.as_ref()
    }
    /// Returns the idempotent request identity; baseline has no client request.
    pub const fn request_id(&self) -> Option<&crate::DomainRequestId> {
        self.request_id.as_ref()
    }
    /// Returns the admitted provenance.
    pub const fn source(&self) -> &DomainMutationSource {
        &self.source
    }
    /// Returns the complete ordered replacements.
    pub fn updates(&self) -> &[DomainStateUpdate] {
        &self.updates
    }
    /// Returns the content identity independent of control sequence and timestamp.
    pub fn request_sha256(&self) -> &str {
        &self.request_sha256
    }
}

impl<'de> Deserialize<'de> for DomainStateCommit {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            request_id: Option<crate::DomainRequestId>,
            source: DomainMutationSource,
            updates: Vec<DomainStateUpdate>,
            fact_span: Option<DomainFactSpan>,
            request_sha256: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        let mut commit = Self::new(wire.request_id, wire.source, wire.updates)
            .map_err(serde::de::Error::custom)?;
        if wire.fact_span.is_some() && !matches!(commit.source, DomainMutationSource::Turn { .. }) {
            return Err(serde::de::Error::custom(
                "only a Turn domain request binds Facts",
            ));
        }
        commit.fact_span = wire.fact_span;
        commit.request_sha256 =
            request_digest(&commit.source, &commit.updates, commit.fact_span.as_ref())
                .map_err(serde::de::Error::custom)?;
        if commit.request_sha256 != wire.request_sha256 {
            return Err(serde::de::Error::custom(
                "domain request digest does not match complete provenance and replacements",
            ));
        }
        Ok(commit)
    }
}

fn request_digest(
    source: &DomainMutationSource,
    updates: &[DomainStateUpdate],
    fact_span: Option<&DomainFactSpan>,
) -> Result<String> {
    #[derive(Serialize)]
    struct Payload<'a> {
        source: &'a DomainMutationSource,
        updates: &'a [DomainStateUpdate],
        facts_sha256: Option<&'a str>,
    }
    payload_digest(
        b"rsi-agent-domain-request-v1\0",
        &Payload {
            source,
            updates,
            facts_sha256: fact_span.map(DomainFactSpan::bodies_sha256),
        },
    )
}

#[derive(Debug)]
struct DigestWriter(Sha256);
impl std::io::Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn payload_digest(domain: &[u8], payload: &impl Serialize) -> Result<String> {
    let mut writer = DigestWriter(Sha256::new());
    writer.0.update(domain);
    serde_json::to_writer(&mut writer, payload)
        .map_err(|error| SessionError::Encoding(error.to_string()))?;
    Ok(hex::encode(writer.0.finalize()))
}
