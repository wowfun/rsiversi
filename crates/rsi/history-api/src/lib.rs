//! Bounded lexical history and source-owned selected-reference operations.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
use rsi_agent_session_protocol::{FrozenReference, ReferenceSelection, ReferenceSource, SessionId};
use rsi_api_protocol::{
    ApiClient, ApiError, OperationAccess, OperationClass, OperationEffect, OperationId,
    OperationSpec, RequestEncoding, Result, call_json,
};
pub use rsi_conversation::ConversationIdentity;
use rsi_workspace_protocol::WorkspaceId;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
#[cfg(test)]
mod tests;

/// Exact query scope; conversation identity does not grant workspace authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    /// Independently registered workspace.
    pub workspace: WorkspaceId,
    /// Explicit native or external conversation.
    pub conversation: ConversationIdentity,
}
/// Candidate source coordinate and full original digest; never authoritative text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hit {
    /// Exact Header or observed epoch.
    pub source: ReferenceSource,
    /// Full original range before the user selects a fragment.
    pub original: ReferenceSelection,
    /// Untrusted index preview, at most 2048 UTF-8 bytes.
    pub preview: String,
}
impl Hit {
    /// Validates finite wire input, without authorizing or verifying indexed content.
    pub fn validate(&self) -> Result<()> {
        self.source.validate().map_err(invalid)?;
        self.original.validate().map_err(invalid)?;
        if self.original.start != 0
            || self.preview.len() > 2048
            || self.preview.len() > self.original.end
        {
            return Err(invalid("invalid history preview"));
        }
        Ok(())
    }
}
/// Explicit per-source indexing coverage, including stable omissions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    /// Exact native Header or observed epoch currently indexed.
    pub source: ReferenceSource,
    /// Last examined original sequence, in canonical decimal.
    pub indexed_through: String,
    /// Last observed source horizon, in canonical decimal.
    pub observed_through: String,
    /// Count of originals or fields excluded by finite limits.
    pub omissions: String,
    /// Whether another batch is required as of this observation.
    pub has_more: bool,
}
impl Coverage {
    /// Validates durable or remote progress without equating observations with Facts.
    pub fn validate(&self) -> Result<()> {
        self.source.validate().map_err(invalid)?;
        if decimal(&self.indexed_through)? > decimal(&self.observed_through)?
            || (!self.has_more
                && decimal(&self.indexed_through)? < decimal(&self.observed_through)?)
        {
            return Err(invalid("invalid history progress"));
        }
        decimal(&self.omissions)?;
        Ok(())
    }
}
/// Exclusive result continuation, fenced against query changes and cache rebuilds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    /// Private cache generation, a 32-character lowercase hex identity.
    pub generation: String,
    /// Exact previous query.
    pub query: String,
    /// Exact previous scope.
    pub scope: Scope,
    /// Last result row, canonical decimal.
    pub after: String,
}
/// Closed history request; each operation has its own advertised API identity.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Advance one finite batch.
    Advance {
        /// Exact source scope.
        scope: Scope,
    },
    /// Search indexed text within the exact source.
    Search {
        /// Exact source scope.
        scope: Scope,
        /// Literal lexical query; no FTS syntax is accepted.
        query: String,
        /// Exclusive continuation.
        after: Option<Cursor>,
    },
    /// Reread an original and return one scalar-aligned window.
    Read {
        /// Exact source scope.
        scope: Scope,
        /// Untrusted candidate to verify.
        hit: Hit,
        /// UTF-8 byte cursor, at most 1 MiB.
        offset: usize,
    },
    /// Freeze a user selection for the actual target.
    Freeze {
        /// Exact source scope.
        scope: Scope,
        /// Original to reread.
        hit: Hit,
        /// Actual receiving Session.
        target: SessionId,
        /// Inclusive UTF-8 byte start.
        start: usize,
        /// Exclusive UTF-8 byte end.
        end: usize,
    },
    /// Discard this source's derived index and reset coverage.
    Rebuild {
        /// Exact source scope.
        scope: Scope,
    },
}
impl Request {
    /// Returns the exact authorized scope for every operation.
    pub fn scope(&self) -> &Scope {
        match self {
            Self::Advance { scope }
            | Self::Search { scope, .. }
            | Self::Read { scope, .. }
            | Self::Freeze { scope, .. }
            | Self::Rebuild { scope } => scope,
        }
    }
    /// Validates external request bounds before any source work.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Search {
                scope,
                query,
                after,
            } => {
                if query.trim().is_empty()
                    || query.len() > 256
                    || query.chars().any(char::is_control)
                {
                    return Err(invalid("history query must contain 1..=256 bytes"));
                }
                if let Some(cursor) = after
                    && (cursor.scope != *scope
                        || cursor.query != *query
                        || cursor.generation.len() != 32
                        || !cursor
                            .generation
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        || decimal(&cursor.after)? == 0)
                {
                    return Err(invalid("history cursor does not match query"));
                }
            }
            Self::Read { hit, offset, .. } => {
                hit.validate()?;
                if *offset > hit.original.end {
                    return Err(invalid("original cursor exceeds text"));
                }
            }
            Self::Freeze {
                hit, start, end, ..
            } => {
                hit.validate()?;
                if start >= end || *end > hit.original.end {
                    return Err(invalid("selected range exceeds original"));
                }
            }
            Self::Advance { .. } | Self::Rebuild { .. } => {}
        }
        Ok(())
    }
    /// Advertised wire operation including its effect classification.
    pub fn spec(&self) -> OperationSpec {
        operation_spec(match self {
            Self::Advance { .. } => "advance",
            Self::Search { .. } => "search",
            Self::Read { .. } => "read",
            Self::Freeze { .. } => "freeze",
            Self::Rebuild { .. } => "rebuild",
        })
    }
}
/// Exact operation specifications for API registration and negotiation.
pub fn operations() -> Vec<OperationSpec> {
    ["advance", "search", "read", "freeze", "rebuild"]
        .map(operation_spec)
        .into()
}
fn operation_spec(name: &str) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("history", name, 1).expect("static history operation"),
        access: OperationAccess::Authenticated,
        class: OperationClass::Data,
        effect: if matches!(name, "search" | "read") {
            OperationEffect::Read
        } else {
            OperationEffect::Mutation
        },
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 16384,
        maximum_response_bytes: 1024 * 1024,
    }
}
/// Bounded response without execution or source-control authority.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    /// Index progress after one step or explicit rebuild.
    Coverage {
        /// Current progress.
        coverage: Coverage,
    },
    /// At most 64 candidates, bounded to 256 KiB encoded.
    Hits {
        /// Progress used for this search.
        coverage: Coverage,
        /// Untrusted candidate previews.
        hits: Vec<Hit>,
        /// More results, if present.
        next: Option<Cursor>,
    },
    /// Exact original source text after owner reread and digest validation.
    Original {
        /// Verified candidate coordinates.
        hit: Hit,
        /// Actual scalar-aligned start.
        offset: usize,
        /// Exclusive next cursor.
        next_offset: usize,
        /// Remaining original bytes exist.
        has_more: bool,
        /// At most 64 KiB of original text.
        text: String,
    },
    /// Immutable target-bound CAS descriptor.
    Frozen {
        /// Source-owned captured reference.
        reference: FrozenReference,
    },
}
/// Canonical source coordinate parser; no JS numeric rounding is accepted.
pub fn decimal(text: &str) -> Result<u64> {
    let n = text.parse::<u64>().map_err(invalid)?;
    if n.to_string() != text {
        return Err(invalid("noncanonical history coordinate"));
    }
    Ok(n)
}
fn invalid(e: impl std::fmt::Display) -> ApiError {
    ApiError::Invalid(e.to_string())
}
#[derive(Serialize, Deserialize)]
enum Never {}
/// API client used by terminal, GUI and opt-in model tools.
#[derive(Clone, Debug)]
pub struct Client {
    api: Arc<dyn ApiClient>,
}
impl Client {
    /// Negotiates the exact history operation set.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if operations()
            .iter()
            .any(|spec| !api.operations().contains(spec))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    /// Executes one bounded request, never retrying an uncertain mutation.
    pub async fn call(&self, request: Request) -> Result<Reply> {
        request.validate()?;
        let reply =
            match call_json::<_, Reply, Never>(&*self.api, &request.spec(), &request).await? {
                Ok(reply) => reply,
                Err(never) => match never {},
            };
        validate_reply(&request, &reply)?;
        Ok(reply)
    }
}
/// Validates response bounds and its correlation with the actual request.
pub fn validate_reply(request: &Request, reply: &Reply) -> Result<()> {
    match (request, reply) {
        (Request::Advance { .. } | Request::Rebuild { .. }, Reply::Coverage { coverage }) => {
            coverage.validate()?;
        }
        (
            Request::Search { scope, query, .. },
            Reply::Hits {
                coverage,
                hits,
                next,
            },
        ) => {
            coverage.validate()?;
            if hits.len() > 64 || serde_json::to_vec(reply).map_err(invalid)?.len() > 256 * 1024 {
                return Err(invalid("history result exceeds limit"));
            }
            let mut seen = std::collections::BTreeSet::new();
            for hit in hits {
                hit.validate()?;
                if !seen.insert(serde_json::to_string(&hit.original.record).map_err(invalid)?) {
                    return Err(invalid("duplicate history record"));
                }
                if hit.source != coverage.source {
                    return Err(invalid("history source mismatch"));
                }
            }
            if let Some(next) = next {
                if hits.is_empty() {
                    return Err(invalid("empty history continuation"));
                }
                if let Request::Search {
                    after: Some(previous),
                    ..
                } = request
                    && decimal(&next.after)? <= decimal(&previous.after)?
                {
                    return Err(invalid("history cursor did not advance"));
                }
                Request::Search {
                    scope: scope.clone(),
                    query: query.clone(),
                    after: Some(next.clone()),
                }
                .validate()?;
            }
        }
        (
            Request::Read { hit, offset, .. },
            Reply::Original {
                hit: actual,
                offset: start,
                next_offset,
                has_more,
                text,
            },
        ) => {
            actual.validate()?;
            if hit != actual
                || start > offset
                || offset - start > 3
                || next_offset < start
                || next_offset - start != text.len()
                || text.len() > 65536
                || *next_offset > hit.original.end
                || *has_more != (*next_offset < hit.original.end)
            {
                return Err(invalid("original response mismatch"));
            }
        }
        (
            Request::Freeze {
                hit,
                target,
                start,
                end,
                ..
            },
            Reply::Frozen { reference },
        ) => {
            reference.validate().map_err(invalid)?;
            let mut selection = hit.original.clone();
            selection.start = *start;
            selection.end = *end;
            if reference.metadata.source != hit.source
                || reference.metadata.target.session_id != *target
                || reference.metadata.capture
                    != (rsi_agent_session_protocol::ReferenceCapture::Selected { selection })
            {
                return Err(invalid("frozen selection mismatch"));
            }
        }
        _ => return Err(invalid("history reply variant mismatch")),
    }
    validate_source(request, reply)
}
fn validate_source(request: &Request, reply: &Reply) -> Result<()> {
    let source = match reply {
        Reply::Coverage { coverage } | Reply::Hits { coverage, .. } => &coverage.source,
        Reply::Original { hit, .. } => &hit.source,
        Reply::Frozen { reference } => &reference.metadata.source,
    };
    let matches = match (&request.scope().conversation, source) {
        (ConversationIdentity::Native(id), ReferenceSource::Native { binding }) => {
            *id == binding.session_id
        }
        (
            ConversationIdentity::External(id),
            ReferenceSource::Observed {
                owner,
                id: original,
                ..
            },
        ) => owner == "acp" && id.as_str() == original,
        _ => false,
    };
    if !matches {
        return Err(invalid("history conversation mismatch"));
    }
    Ok(())
}
