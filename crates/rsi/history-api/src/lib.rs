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
/// Query range; identities select data but never supply permission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueryScope {
    /// Exact source.
    Conversation {
        /// Exact registered source.
        source: Scope,
    },
    /// All locally saved conversations at registered coordinates.
    Workspace {
        /// Registered workspace identity.
        workspace: WorkspaceId,
    },
    /// All currently authorized registered sources on this Host.
    AccessibleHost,
}
impl QueryScope {
    /// Whether an exact source belongs to the requested range, independent of authority.
    pub fn contains(&self, source: &Scope) -> bool {
        match self {
            Self::Conversation { source: exact } => exact == source,
            Self::Workspace { workspace } => workspace == &source.workspace,
            Self::AccessibleHost => true,
        }
    }
}
/// Owner-retained discovery state over validated metadata; ingress carries only opaque tokens.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent scan completion, fairness and failure observations can coexist."
)]
pub struct DiscoveryCursor {
    /// Exclusive round-robin index position.
    pub index_after: Option<String>,
    /// Bounded failures skipped until an explicit fresh pass.
    pub unavailable: Vec<String>,
    /// Actual caller correlation, never an admission token.
    pub caller: String,
    /// Exact query range correlation.
    pub scope_key: String,
    /// Last consumed registry insertion position.
    pub workspace_after: Option<u64>,
    /// Last consumed native metadata identity.
    pub native_after: Option<SessionId>,
    /// Last consumed external metadata identity.
    pub external_after: Option<rsi_acp_protocol::observation::ConversationId>,
    /// Returned workspace items consumed by this pass.
    pub workspaces: usize,
    /// Source items consumed by this pass.
    pub sources: usize,
    /// Registry scan reached its end.
    pub workspace_done: bool,
    /// Native scan reached its end.
    pub native_done: bool,
    /// External scan reached its end.
    pub external_done: bool,
    /// Alternate source family on the next step.
    pub external_next: bool,
    /// A hard count, byte or cache limit ended this pass.
    pub capacity_limited: bool,
    /// A metadata family or source Header failed; a fresh pass retries it.
    pub metadata_unavailable: bool,
}
impl DiscoveryCursor {
    /// End-of-pass is a live scan observation, not a Host snapshot.
    pub fn complete(&self) -> bool {
        self.workspace_done
            && self.native_done
            && self.external_done
            && !self.capacity_limited
            && !self.metadata_unavailable
    }
}
/// Authorized summary; omitted unauthorized sources have no observable count.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryProgress {
    /// Discovery reached the end of this finite pass.
    pub discovery_complete: bool,
    /// A discovery/cache capacity boundary stopped progress.
    pub capacity_limited: bool,
    /// A metadata family or source Header failed; a fresh pass retries it.
    pub metadata_unavailable: bool,
    /// Currently authorized cached sources in this range.
    pub visible_sources: usize,
    /// Sources whose observed horizon still needs indexing.
    pub pending_sources: usize,
    /// Last retained discovery continuation, if any.
    pub continuation: Option<String>,
}
/// Source-owned indexing status.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCoverage {
    /// Bounded owner-derived workspace display text.
    pub label: String,
    /// This pass could not read the admitted source; refresh retries it.
    pub unavailable: bool,
    /// Registered exact source.
    pub scope: Scope,
    /// Derived coverage, never source truth.
    pub coverage: Coverage,
    /// Whether current human policy permits reference capture.
    pub reference_allowed: bool,
}
/// A cross-conversation candidate retaining the source required for Read/Freeze.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    /// Bounded owner-derived workspace display text.
    pub label: String,
    /// Exact source coordinates selected by the owner.
    pub scope: Scope,
    /// Candidate, whose original must be reread.
    pub hit: Hit,
    /// Protected history can be read but never captured.
    pub reference_allowed: bool,
}
/// Stable read continuation, invalidated by content or authority-set changes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryCursor {
    /// Cache reconstruction identity.
    pub generation: String,
    /// Canonical monotonic content revision.
    pub revision: String,
    /// Exact lexical query.
    pub query: String,
    /// Exact range.
    pub scope: QueryScope,
    /// Actual caller correlation.
    pub caller: String,
    /// Current admitted-source-set fingerprint.
    pub visibility: String,
    /// Exclusive result row.
    pub after: String,
}
fn hex_identity(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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
    /// Discover finite saved metadata and advance one admitted source.
    Discover {
        /// Requested discovery range.
        scope: QueryScope,
        /// Opaque retained pass continuation.
        after: Option<String>,
    },
    /// Search already indexed text across conversations.
    Query {
        /// Requested search range.
        scope: QueryScope,
        /// Literal lexical query.
        query: String,
        /// Previous stable read continuation.
        after: Option<QueryCursor>,
    },
    /// Inspect authorized coverage in pages of at most 64 sources.
    Progress {
        /// Requested coverage range.
        scope: QueryScope,
        /// Exclusive source key.
        after: Option<String>,
    },
    /// Reset at most 64 currently admitted sources in one Workspace.
    Reset {
        /// Exact Workspace range; global resets are rejected.
        scope: QueryScope,
        /// Last reset source key, for interruptible continuation.
        after: Option<String>,
    },
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
    pub fn scope(&self) -> Option<&Scope> {
        match self {
            Self::Advance { scope }
            | Self::Search { scope, .. }
            | Self::Read { scope, .. }
            | Self::Freeze { scope, .. }
            | Self::Rebuild { scope } => Some(scope),
            Self::Discover { scope, .. }
            | Self::Query { scope, .. }
            | Self::Progress { scope, .. }
            | Self::Reset { scope, .. } => {
                if let QueryScope::Conversation { source } = scope {
                    Some(source)
                } else {
                    None
                }
            }
        }
    }
    /// Validates external request bounds before any source work.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Discover { after, .. } => {
                if after.as_ref().is_some_and(|s| !hex_identity(s, 32)) {
                    return Err(invalid("invalid discovery continuation"));
                }
            }
            Self::Reset { scope, after } => {
                if !matches!(scope, QueryScope::Workspace { .. })
                    || after.as_ref().is_some_and(|s| !hex_identity(s, 64))
                {
                    return Err(invalid(
                        "reset requires a Workspace and valid source cursor",
                    ));
                }
            }
            Self::Progress { after, .. } => {
                if after.as_ref().is_some_and(|s| !hex_identity(s, 64)) {
                    return Err(invalid("invalid progress cursor"));
                }
            }
            Self::Query {
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
                if let Some(cursor) = after {
                    if cursor.scope != *scope
                        || cursor.query != *query
                        || !hex_identity(&cursor.generation, 32)
                        || !hex_identity(&cursor.caller, 64)
                        || !hex_identity(&cursor.visibility, 64)
                        || decimal(&cursor.after)? == 0
                    {
                        return Err(invalid("invalid query cursor"));
                    }
                    decimal(&cursor.revision)?;
                }
            }
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
            Self::Discover { .. } => "discover",
            Self::Query { .. } => "query",
            Self::Progress { .. } => "progress",
            Self::Reset { .. } => "reset",
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
    [
        "advance", "search", "read", "freeze", "rebuild", "discover", "query", "progress", "reset",
    ]
    .map(operation_spec)
    .into()
}
fn operation_spec(name: &str) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("history", name, 2).expect("static history operation"),
        access: OperationAccess::Authenticated,
        class: OperationClass::Data,
        effect: if matches!(name, "search" | "read" | "query" | "progress") {
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
    /// A query continuation no longer represents this content or authority view.
    Stale {
        /// Bounded reason requiring a fresh query.
        reason: String,
    },
    /// Cross-conversation candidates in stable row order.
    Matches {
        /// Authorized coverage at this observation.
        progress: QueryProgress,
        /// Bounded source-labelled candidates.
        matches: Vec<Match>,
        /// Stable read continuation.
        next: Option<QueryCursor>,
    },
    /// Authorized discovery and per-source indexing diagnostics.
    Progress {
        /// Authorized aggregate coverage.
        progress: QueryProgress,
        /// Bounded source status page.
        sources: Vec<SourceCoverage>,
        /// Exclusive source key for another coverage page.
        next: Option<String>,
    },
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
#[expect(
    clippy::too_many_lines,
    reason = "One exhaustive request/reply matrix keeps correlation and bounds at the wire boundary."
)]
pub fn validate_reply(request: &Request, reply: &Reply) -> Result<()> {
    match (request, reply) {
        (Request::Query { .. }, Reply::Stale { reason }) if reason.len() <= 256 => return Ok(()),
        (
            Request::Query {
                scope,
                query,
                after,
            },
            Reply::Matches {
                progress,
                matches,
                next,
            },
        ) => {
            validate_progress(progress)?;
            if matches.len() > 64 || serde_json::to_vec(reply).map_err(invalid)?.len() > 256 * 1024
            {
                return Err(invalid("history result exceeds limit"));
            }
            let mut seen = std::collections::BTreeSet::new();
            for item in matches {
                item.hit.validate()?;
                if item.label.len() > 512
                    || !scope.contains(&item.scope)
                    || !source_matches(&item.scope, &item.hit.source)
                    || !seen.insert(
                        serde_json::to_string(&(&item.scope, &item.hit.original.record))
                            .map_err(invalid)?,
                    )
                {
                    return Err(invalid("history candidate mismatch"));
                }
            }
            if let Some(next) = next {
                if matches.is_empty()
                    || after.as_ref().is_some_and(|p| {
                        decimal(&next.after).unwrap_or(0) <= decimal(&p.after).unwrap_or(u64::MAX)
                    })
                {
                    return Err(invalid("history cursor did not advance"));
                }
                Request::Query {
                    scope: scope.clone(),
                    query: query.clone(),
                    after: Some(next.clone()),
                }
                .validate()?;
            }
            return Ok(());
        }
        (
            Request::Discover { scope, .. }
            | Request::Progress { scope, .. }
            | Request::Reset { scope, .. },
            Reply::Progress {
                progress,
                sources,
                next,
            },
        ) => {
            validate_progress(progress)?;
            if sources.len() > 64
                || next.as_ref().is_some_and(|s| !hex_identity(s, 64))
                || serde_json::to_vec(reply).map_err(invalid)?.len() > 256 * 1024
            {
                return Err(invalid("history progress exceeds limit"));
            }
            for source in sources {
                source.coverage.validate()?;
                if source.label.len() > 512
                    || !scope.contains(&source.scope)
                    || !source_matches(&source.scope, &source.coverage.source)
                {
                    return Err(invalid("history progress source mismatch"));
                }
            }
            return Ok(());
        }
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
        Reply::Matches { .. } | Reply::Progress { .. } | Reply::Stale { .. } => {
            return Err(invalid("unexpected aggregate reply"));
        }
    };
    if !source_matches(
        request
            .scope()
            .ok_or_else(|| invalid("missing exact source"))?,
        source,
    ) {
        return Err(invalid("history conversation mismatch"));
    }
    Ok(())
}
fn source_matches(scope: &Scope, source: &ReferenceSource) -> bool {
    match (&scope.conversation, source) {
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
    }
}
fn validate_progress(progress: &QueryProgress) -> Result<()> {
    if progress.visible_sources > 4096
        || progress.pending_sources > progress.visible_sources
        || progress
            .continuation
            .as_ref()
            .is_some_and(|s| !hex_identity(s, 32))
        || (progress.capacity_limited || progress.metadata_unavailable)
            && progress.discovery_complete
    {
        return Err(invalid("invalid aggregate coverage"));
    }
    Ok(())
}
