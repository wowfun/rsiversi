//! Bounded interval evidence with durable summaries and runtime-only diffs.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
use rsi_agent_session_protocol::TurnId;
use rsi_api_protocol::{
    ApiClient, ApiError, OperationAccess, OperationClass, OperationEffect, OperationId,
    OperationSpec, RequestEncoding, Result, call_json,
};
pub use rsi_conversation::ConversationIdentity;
use rsi_workspace_protocol::WorkspaceId;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Current source authorization, independent of an interval's opaque ID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    /// Registered workspace.
    pub workspace: WorkspaceId,
    /// Exact conversation.
    pub conversation: ConversationIdentity,
}
/// Evidence state; terminal Turn state alone is not Complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Baseline, controlled work or final capture is still pending.
    Pending,
    /// Both captures completed for the declared text-file scope and work settled.
    Complete,
    /// At least one capture or settlement could not be confirmed.
    Partial,
}
/// Stable omission taxonomy without unbounded native error output.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmissionKind {
    /// Git metadata cannot identify this workspace.
    NonGit,
    /// Runtime has no original baseline for already started execution.
    RecoveredWithoutBaseline,
    /// A stage did not complete within its deadline.
    Deadline,
    /// Provider or scratch admission was exhausted.
    Capacity,
    /// File/path count or original-byte ceiling was reached.
    Limit,
    /// A path was missing, changed or rejected by no-follow reads.
    Unreadable,
    /// Binary or invalid UTF-8 original excluded from textual comparison.
    Binary,
    /// A Git command or bounded output could not be completed.
    Git,
    /// Controlled Jobs/Tools/finalization have no complete settlement proof.
    Unsettled,
    /// Begin did not finish before effects were allowed to proceed.
    MissingBaseline,
}
/// Counted bounded omission, never a silently complete result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Omission {
    /// Stable cause.
    pub kind: OmissionKind,
    /// Number of known excluded paths or stages, at least one.
    pub count: u32,
}
/// Durable summary; diff files are never embedded here.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    /// Opaque lowercase-hex interval ID.
    pub id: String,
    /// Opaque owning runtime epoch.
    pub epoch: String,
    /// Independently authorized source.
    pub scope: Scope,
    /// Native Turn, or None for a standalone observed prompt.
    pub turn: Option<TurnId>,
    /// Native claim or observed prompt coordinate, canonical decimal.
    pub execution: String,
    /// Source acceptance cursor, canonical decimal.
    pub accepted_seq: String,
    /// Source live cursor before capture, canonical decimal.
    pub live_seq: String,
    /// Wall-clock start in milliseconds, canonical decimal.
    pub started_ms: String,
    /// End capture time, absent while pending.
    pub finished_ms: Option<String>,
    /// Distinct pending/complete/partial states.
    pub phase: Phase,
    /// Number of changed text files, including renames.
    pub changed_files: u32,
    /// Added lines in the captured interval.
    pub added_lines: u64,
    /// Removed lines in the captured interval.
    pub removed_lines: u64,
    /// Stable counted limitations.
    pub omissions: Vec<Omission>,
}
impl Summary {
    /// Validate a durable or wire record before publication.
    pub fn validate(&self) -> Result<()> {
        identity(&self.id)?;
        identity(&self.epoch)?;
        for value in [
            &self.execution,
            &self.accepted_seq,
            &self.live_seq,
            &self.started_ms,
        ] {
            decimal(value)?;
        }
        if let Some(value) = &self.finished_ms {
            decimal(value)?;
        }
        if matches!(&self.scope.conversation, ConversationIdentity::Native(_))
            != self.turn.is_some()
            || self.execution == "0"
            || self.changed_files > 20_000
            || self.omissions.len() > 10
            || self.omissions.iter().any(|o| o.count == 0)
            || self
                .omissions
                .windows(2)
                .any(|pair| pair[0].kind >= pair[1].kind)
            || (self.phase == Phase::Complete && !self.omissions.is_empty())
            || (self.phase == Phase::Pending) != self.finished_ms.is_none()
            || serde_json::to_vec(self).map_err(invalid)?.len() > 256 * 1024
        {
            return Err(invalid("invalid interval summary"));
        }
        Ok(())
    }
}
/// A current-runtime changed file; names are untrusted display data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileChange {
    /// Relative destination path, or removed path.
    pub path: String,
    /// Relative source when Git detected a rename.
    pub previous_path: Option<String>,
    /// Added lines.
    pub added: u64,
    /// Removed lines.
    pub removed: u64,
}
impl FileChange {
    /// Bounds relative names before Git pathspec construction.
    pub fn validate(&self) -> Result<()> {
        path(&self.path)?;
        if let Some(value) = &self.previous_path {
            path(value)?;
        }
        Ok(())
    }
}
/// Explicit finite read request.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Page durable summaries by stable ID.
    List {
        /// Current source.
        scope: Scope,
        /// Exclusive interval cursor.
        after: Option<String>,
    },
    /// Page changed files from current scratch.
    Files {
        /// Current source.
        scope: Scope,
        /// Exact interval.
        id: String,
        /// First entry.
        offset: usize,
    },
    /// Page one textual comparison.
    Diff {
        /// Current source.
        scope: Scope,
        /// Exact interval.
        id: String,
        /// One path from the interval file list.
        path: String,
        /// UTF-8 byte cursor.
        offset: usize,
    },
}
impl Request {
    /// Borrows the exact authorization scope.
    pub fn scope(&self) -> &Scope {
        match self {
            Self::List { scope, .. } | Self::Files { scope, .. } | Self::Diff { scope, .. } => {
                scope
            }
        }
    }
    /// Checks all externally supplied bounds before work.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::List { after, .. } => {
                if let Some(id) = after {
                    identity(id)?;
                }
            }
            Self::Files { id, offset, .. } => {
                identity(id)?;
                if *offset > 20_000 {
                    return Err(invalid("file cursor exceeds interval bound"));
                }
            }
            Self::Diff {
                id,
                path: value,
                offset,
                ..
            } => {
                identity(id)?;
                path(value)?;
                if *offset > 4 * 1024 * 1024 {
                    return Err(invalid("diff cursor exceeds bound"));
                }
            }
        }
        Ok(())
    }
    /// Exact advertised operation.
    pub fn spec(&self) -> OperationSpec {
        spec(match self {
            Self::List { .. } => "list",
            Self::Files { .. } => "files",
            Self::Diff { .. } => "diff",
        })
    }
}
/// Bounded response with explicit expiration instead of an empty diff.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    /// Durable summaries independent of scratch availability.
    Summaries {
        /// Current owner epoch for immediate expired/interrupted presentation.
        epoch: String,
        /// Bounded records.
        items: Vec<Summary>,
        /// Last returned ID when more exist.
        next: Option<String>,
    },
    /// Current-runtime file page.
    Files {
        /// Exact interval.
        id: String,
        /// First entry.
        offset: usize,
        /// At most 64 files.
        files: Vec<FileChange>,
        /// More files remain.
        has_more: bool,
    },
    /// Verified current-runtime diff text page.
    Diff {
        /// Exact interval.
        id: String,
        /// Exact chosen path.
        path: String,
        /// Scalar-aligned first byte.
        offset: usize,
        /// Exclusive next cursor.
        next_offset: usize,
        /// More captured text exists.
        has_more: bool,
        /// At most 64 KiB of text.
        text: String,
    },
    /// Summary remains durable, but comparison content is unavailable.
    Expired {
        /// Exact interval.
        id: String,
    },
}
fn invalid(e: impl std::fmt::Display) -> ApiError {
    ApiError::Invalid(e.to_string())
}
/// Validate canonical decimal coordinates without lossy JS numeric decoding.
pub fn decimal(value: &str) -> Result<u64> {
    let n = value.parse::<u64>().map_err(invalid)?;
    if n.to_string() != value {
        return Err(invalid("noncanonical review coordinate"));
    }
    Ok(n)
}
/// Validate fixed opaque identities.
pub fn identity(value: &str) -> Result<()> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(invalid("invalid review identity"));
    }
    Ok(())
}
/// Validate a textual relative path before exact literal-path Git use.
pub fn path(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 4096
        || value.as_bytes().contains(&0)
        || value
            .split('/')
            .any(|c| c.is_empty() || matches!(c, "." | ".."))
        || value.contains('\\')
    {
        return Err(invalid("invalid review path"));
    }
    Ok(())
}
fn spec(name: &str) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("workspace_review", name, 1).expect("static review operation"),
        access: OperationAccess::Authenticated,
        class: OperationClass::Data,
        effect: OperationEffect::Read,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 16384,
        maximum_response_bytes: 1024 * 1024,
    }
}
/// Exact operations negotiated by product clients.
pub fn operations() -> Vec<OperationSpec> {
    ["list", "files", "diff"].map(spec).into()
}
#[derive(Serialize, Deserialize)]
enum Never {}
/// Read-only review client; validates response correlation and finite pages.
#[derive(Clone, Debug)]
pub struct Client {
    api: Arc<dyn ApiClient>,
}
impl Client {
    /// Negotiate the complete typed read operation set.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if operations().iter().any(|s| !api.operations().contains(s)) {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    /// Perform one finite read.
    pub async fn call(&self, request: Request) -> Result<Reply> {
        request.validate()?;
        let reply =
            match call_json::<_, Reply, Never>(&*self.api, &request.spec(), &request).await? {
                Ok(value) => value,
                Err(never) => match never {},
            };
        validate_reply(&request, &reply)?;
        Ok(reply)
    }
}
/// Validates owner or remote response before it enters a UI.
pub fn validate_reply(request: &Request, reply: &Reply) -> Result<()> {
    match (request, reply) {
        (Request::List { scope, after }, Reply::Summaries { epoch, items, next }) => {
            identity(epoch)?;
            if items.len() > 32 {
                return Err(invalid("too many interval summaries"));
            }
            let mut previous = after.as_deref();
            for item in items {
                item.validate()?;
                if &item.scope != scope || previous.is_some_and(|p| p >= item.id.as_str()) {
                    return Err(invalid("interval scope or order mismatch"));
                }
                previous = Some(&item.id);
            }
            if next
                .as_deref()
                .is_some_and(|n| Some(n) != items.last().map(|i| i.id.as_str()))
            {
                return Err(invalid("interval continuation mismatch"));
            }
        }
        (
            Request::Files { id, offset, .. },
            Reply::Files {
                id: actual,
                offset: first,
                files,
                has_more,
            },
        ) => {
            if id != actual
                || offset != first
                || files.len() > 64
                || offset
                    .checked_add(files.len())
                    .is_none_or(|end| end > 20_000)
                || files.windows(2).any(|pair| pair[0].path >= pair[1].path)
                || (*has_more && files.is_empty())
                || serde_json::to_vec(reply).map_err(invalid)?.len() > 256 * 1024
            {
                return Err(invalid("invalid changed-file page"));
            }
            for file in files {
                file.validate()?;
            }
        }
        (
            Request::Diff {
                id, path, offset, ..
            },
            Reply::Diff {
                id: actual,
                path: actual_path,
                offset: first,
                next_offset,
                has_more,
                text,
            },
        ) => {
            if id != actual
                || path != actual_path
                || first > offset
                || offset - first > 3
                || next_offset < first
                || next_offset - first != text.len()
                || text.len() > 65536
                || *next_offset > 4 * 1024 * 1024
                || (*has_more && text.is_empty())
            {
                return Err(invalid("invalid diff page"));
            }
        }
        (Request::Files { id, .. } | Request::Diff { id, .. }, Reply::Expired { id: actual })
            if id == actual => {}
        _ => return Err(invalid("review reply variant mismatch")),
    }
    if serde_json::to_vec(reply).map_err(invalid)?.len() > 1024 * 1024 {
        return Err(invalid("review reply exceeds limit"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> Scope {
        Scope {
            workspace: WorkspaceId::parse("a".repeat(64)).unwrap(),
            conversation: ConversationIdentity::Native(
                rsi_agent_session_protocol::SessionId::new("source").unwrap(),
            ),
        }
    }
    #[test]
    fn response_pages_reject_aliased_files_backward_diff_and_forged_scopes() {
        let id = "b".repeat(32);
        let request = Request::Files {
            scope: scope(),
            id: id.clone(),
            offset: 0,
        };
        let file = FileChange {
            path: "file.rs".into(),
            previous_path: None,
            added: 1,
            removed: 0,
        };
        assert!(
            validate_reply(
                &request,
                &Reply::Files {
                    id: id.clone(),
                    offset: 0,
                    files: vec![file.clone(), file],
                    has_more: false
                }
            )
            .is_err()
        );
        let request = Request::Diff {
            scope: scope(),
            id: id.clone(),
            path: "file.rs".into(),
            offset: 8,
        };
        assert!(
            validate_reply(
                &request,
                &Reply::Diff {
                    id: id.clone(),
                    path: "file.rs".into(),
                    offset: 9,
                    next_offset: 9,
                    has_more: false,
                    text: String::new()
                }
            )
            .is_err()
        );
        assert!(validate_reply(&request, &Reply::Expired { id: "c".repeat(32) }).is_err());
        assert!(
            Request::Diff {
                scope: scope(),
                id,
                path: "../secret".into(),
                offset: 0
            }
            .validate()
            .is_err()
        );
        assert!(decimal("01").is_err());
        assert!(identity(&"B".repeat(32)).is_err());
    }
}
