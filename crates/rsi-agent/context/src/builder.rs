//! Selected cursor ownership and the builder-independent bounded cache envelope.

use crate::{ContextError, ContextLimits, MAXIMUM_CONTEXT_CHECKPOINT_BYTES, Result};
use rsi_agent_session_protocol::{SessionFact, SessionHeader};
use rsi_ai_protocol::LanguageRequest;
use rsi_meta::LocalContract;
use rsi_tools_protocol::ToolDefinition;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt, sync::Arc};

const MAGIC: &[u8] = b"rsi-agent-model-context-v6\0";
const MAXIMUM_METADATA_BYTES: usize = 2048;

/// Frozen semantic and configuration identity of one context builder.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextBuilderIdentity {
    id: String,
    semantic_version: String,
    config_sha256: String,
}

impl<'de> Deserialize<'de> for ContextBuilderIdentity {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            id: String,
            semantic_version: String,
            config_sha256: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.id, wire.semantic_version, wire.config_sha256)
            .map_err(serde::de::Error::custom)
    }
}

impl ContextBuilderIdentity {
    /// Creates a bounded ASCII identity with a lowercase SHA-256 configuration digest.
    pub fn new(
        id: impl Into<String>,
        semantic_version: impl Into<String>,
        config_sha256: impl Into<String>,
    ) -> Result<Self> {
        let identity = Self {
            id: id.into(),
            semantic_version: semantic_version.into(),
            config_sha256: config_sha256.into(),
        };
        identity.validate()?;
        Ok(identity)
    }

    fn validate(&self) -> Result<()> {
        for (value, maximum) in [(&self.id, 256), (&self.semantic_version, 64)] {
            if value.is_empty()
                || value.len() > maximum
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-+".contains(&byte))
            {
                return Err(invalid(
                    "context builder identity is empty, oversized, or malformed",
                ));
            }
        }
        crate::decode_sha256("builder configuration digest", &self.config_sha256)?;
        Ok(())
    }

    /// Returns the implementation identity.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Returns the semantic version controlling context and payload compatibility.
    pub fn semantic_version(&self) -> &str {
        &self.semantic_version
    }
    /// Returns the digest of the builder's normalized configuration.
    pub fn config_sha256(&self) -> &str {
        &self.config_sha256
    }
}

/// Exact child Fact position; inherited fork Facts do not advance this position.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPosition {
    /// Highest scanned child Fact sequence, including claim-hidden Facts.
    pub through_seq: u64,
    /// Rolling digest of the applied child Facts; only exact prefixes may checkpoint.
    pub fact_prefix_digest: [u8; 32],
}

impl ContextPosition {
    /// Returns the canonical lowercase hexadecimal prefix digest for Store metadata.
    pub fn fact_prefix_sha256(&self) -> String {
        hex::encode(self.fact_prefix_digest)
    }
}

/// Immutable opening inputs. Checkpoint bytes are borrowed only during `open`.
#[derive(Debug)]
pub struct ContextInit<'a> {
    /// Validated immutable Session Header.
    pub header: SessionHeader,
    /// Exact retention and model-request limits.
    pub limits: ContextLimits,
    /// Bounded provider payload from a matching generic envelope, if restoring.
    pub checkpoint: Option<&'a [u8]>,
}

/// Distinct bounded history inputs supplied by execution or maintenance.
#[derive(Clone, Copy, Debug)]
pub enum ContextPage<'a> {
    /// A contiguous canonical child Fact page, including freshly published Facts.
    Canonical(&'a [Arc<SessionFact>]),
    /// Claim-visible Facts and the complete scan horizon, which may include holes.
    ClaimVisible {
        /// Visible validated Facts in source order.
        facts: &'a [Arc<SessionFact>],
        /// Highest scanned child sequence.
        through_seq: u64,
    },
    /// A contiguous page in the Header's exact inherited parent interval.
    ForkSeed(&'a [Arc<SessionFact>]),
    /// Validate that the complete inherited interval has arrived.
    FinishSeed,
}

/// Pure synchronous context construction selected by one Agent generation.
pub trait ModelContextBuilder: fmt::Debug + Send + Sync + 'static {
    /// Returns this instance's immutable semantic/configuration identity.
    fn identity(&self) -> &ContextBuilderIdentity;
    /// Opens a cursor; performs no I/O, clock sampling, or external effects.
    fn open(&self, init: ContextInit<'_>) -> Result<Box<dyn ModelContextCursor>>;
}

/// Exclusive incremental context state for one immutable Session Header.
pub trait ModelContextCursor: fmt::Debug + Send + 'static {
    /// Consumes a framework-supplied bounded page or seed completion marker.
    fn ingest(&mut self, page: ContextPage<'_>) -> Result<()>;
    /// Builds a request using Tool definitions from the cursor's Agent pin.
    fn build(&self, tools: Vec<ToolDefinition>) -> Result<LanguageRequest>;
    /// Encodes a bounded payload only for an exact checkpointable Fact prefix.
    fn checkpoint(&self) -> Result<Arc<[u8]>>;
    /// Returns the current child Fact position.
    fn position(&self) -> ContextPosition;
}

/// Nominal Local contract for the generation's unique context builder.
#[derive(Debug)]
pub struct ModelContextBuilderContract;
impl LocalContract for ModelContextBuilderContract {
    const KEY: &'static str = "rsi.agent.model-context-builder";
    type Service = dyn ModelContextBuilder;
}

/// Selected builder and cursor with one Context-owned cache boundary.
#[derive(Debug)]
pub struct ModelContextState {
    builder: Arc<dyn ModelContextBuilder>,
    identity: ContextBuilderIdentity,
    header: SessionHeader,
    header_fingerprint: String,
    limits: ContextLimits,
    cursor: Box<dyn ModelContextCursor>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    builder: ContextBuilderIdentity,
    header_fingerprint: String,
    limits: ContextLimits,
    position: ContextPosition,
}

impl ModelContextState {
    /// Opens empty state with the selected immutable builder.
    pub fn open(
        builder: Arc<dyn ModelContextBuilder>,
        header: SessionHeader,
        limits: ContextLimits,
    ) -> Result<Self> {
        header
            .validate()
            .map_err(|error| invalid(&error.to_string()))?;
        ContextLimits::new(limits.max_messages, limits.max_bytes)?;
        let identity = builder.identity().clone();
        let header_fingerprint = header
            .fingerprint()
            .map_err(|error| invalid(&error.to_string()))?;
        let cursor = builder.open(ContextInit {
            header: header.clone(),
            limits,
            checkpoint: None,
        })?;
        Ok(Self {
            builder,
            identity,
            header,
            header_fingerprint,
            limits,
            cursor,
        })
    }

    /// Consumes one bounded typed page from the owning execution path.
    pub fn ingest(&mut self, page: ContextPage<'_>) -> Result<()> {
        self.cursor.ingest(page)
    }
    /// Builds the next bounded request using the same pin's Tools.
    pub fn build(&self, tools: Vec<ToolDefinition>) -> Result<LanguageRequest> {
        self.cursor.build(tools)
    }
    /// Returns the selected cursor's exact child position.
    pub fn position(&self) -> ContextPosition {
        self.cursor.position()
    }

    /// Encodes the generic binding metadata and raw provider payload once.
    pub fn checkpoint(&self) -> Result<Arc<[u8]>> {
        let position = self.position();
        if position.through_seq == 0 {
            return Err(invalid("checkpoint requires a nonempty prefix"));
        }
        let payload = self.cursor.checkpoint()?;
        if payload.is_empty() || payload.len() > MAXIMUM_CONTEXT_CHECKPOINT_BYTES {
            return Err(invalid("builder checkpoint payload is empty or oversized"));
        }
        let metadata = serde_json::to_vec(&Metadata {
            builder: self.identity.clone(),
            header_fingerprint: self.header_fingerprint.clone(),
            limits: self.limits,
            position,
        })
        .map_err(|error| invalid(&error.to_string()))?;
        let size = MAGIC.len() + 32 + 4 + metadata.len() + payload.len();
        if metadata.len() > MAXIMUM_METADATA_BYTES || size > MAXIMUM_CONTEXT_CHECKPOINT_BYTES {
            return Err(invalid(
                "context checkpoint envelope exceeds its byte bound",
            ));
        }
        let length = u32::try_from(metadata.len())
            .map_err(|_| invalid("checkpoint metadata length overflow"))?
            .to_le_bytes();
        let mut digest = Sha256::new();
        digest.update(MAGIC);
        digest.update(length);
        digest.update(&metadata);
        digest.update(&payload);
        let mut bytes = Vec::with_capacity(size);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&digest.finalize());
        bytes.extend_from_slice(&length);
        bytes.extend_from_slice(&metadata);
        bytes.extend_from_slice(&payload);
        Ok(bytes.into())
    }

    /// Restores a matching cache; any rejection preserves the current cursor.
    pub fn restore(&mut self, bytes: &[u8]) -> Result<()> {
        *self = self.restored(bytes)?;
        Ok(())
    }

    /// Opens a matching cached state for independent Store-metadata adjudication.
    pub fn restored(&self, bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAXIMUM_CONTEXT_CHECKPOINT_BYTES {
            return Err(invalid("context checkpoint exceeds its byte bound"));
        }
        let body = bytes
            .strip_prefix(MAGIC)
            .ok_or_else(|| invalid("context checkpoint version mismatch"))?;
        let (binding, body) = body
            .split_at_checked(32)
            .ok_or_else(|| invalid("truncated checkpoint digest"))?;
        let (length, body) = body
            .split_at_checked(4)
            .ok_or_else(|| invalid("truncated checkpoint metadata length"))?;
        let metadata_length = usize::try_from(u32::from_le_bytes(
            length
                .try_into()
                .map_err(|_| invalid("invalid checkpoint length framing"))?,
        ))
        .map_err(|_| invalid("checkpoint metadata length overflow"))?;
        if metadata_length > MAXIMUM_METADATA_BYTES {
            return Err(invalid("oversized checkpoint metadata"));
        }
        let (metadata, payload) = body
            .split_at_checked(metadata_length)
            .ok_or_else(|| invalid("truncated checkpoint metadata"))?;
        if payload.is_empty() {
            return Err(invalid("empty checkpoint payload"));
        }
        let mut digest = Sha256::new();
        digest.update(MAGIC);
        digest.update(length);
        digest.update(body);
        if binding != digest.finalize().as_slice() {
            return Err(invalid("context checkpoint binding mismatch"));
        }
        let metadata: Metadata =
            serde_json::from_slice(metadata).map_err(|error| invalid(&error.to_string()))?;
        if metadata.builder != self.identity
            || metadata.header_fingerprint != self.header_fingerprint
            || metadata.limits != self.limits
            || metadata.position.through_seq == 0
        {
            return Err(invalid(
                "context checkpoint builder, header, limits, or position mismatch",
            ));
        }
        let cursor = self.builder.open(ContextInit {
            header: self.header.clone(),
            limits: self.limits,
            checkpoint: Some(payload),
        })?;
        if cursor.position() != metadata.position {
            return Err(invalid("restored cursor differs from checkpoint position"));
        }
        Ok(Self {
            builder: self.builder.clone(),
            identity: self.identity.clone(),
            header: self.header.clone(),
            header_fingerprint: self.header_fingerprint.clone(),
            limits: self.limits,
            cursor,
        })
    }
}

fn invalid(message: &str) -> ContextError {
    ContextError::Invalid(message.into())
}
