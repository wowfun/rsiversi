//! Typed bounded navigation over durable Session truth.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
use rsi_agent_session_protocol::SessionId;
pub use rsi_agent_store_protocol::StoreActivityCursor as ActivityCursor;
use rsi_api_protocol::{
    ApiClient, ApiError, HostEpoch, OperationAccess, OperationClass, OperationEffect, OperationId,
    OperationSpec, RequestEncoding, Result, call_json,
};
use rsi_workspace_protocol::WorkspaceId;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
/// Current-owner attention and explicit reading-position operations.
pub mod attention;
mod order;
mod validation;
pub use order::{OrderMember, OrderMembership, OrderScope, OrderSeed, SummaryPage, SummaryRequest};

/// Exact title and archive state; neither field mutates Session execution.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMetadata {
    /// Optional user title; absent uses the immutable Session identity.
    pub title: Option<String>,
    /// Whether normal navigation hides this Session.
    pub archived: bool,
    /// Pinned independently of recent-page depth; absent in legacy durable records.
    #[serde(default)]
    pub pinned: bool,
}
impl SessionMetadata {
    /// Validates the complete external or durable metadata record.
    pub fn validate(&self) -> Result<()> {
        if self.archived && self.pinned {
            return Err(ApiError::Invalid(
                "archived sessions cannot be pinned".into(),
            ));
        }
        if self.title.as_ref().is_some_and(|title| {
            title.is_empty() || title.len() > 256 || title.chars().any(char::is_control)
        }) {
            return Err(ApiError::Invalid(
                "Session title must be 1..=256 bytes without control characters".into(),
            ));
        }
        Ok(())
    }
}
/// Explicit workspace partition for navigation queries.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceFilter {
    /// Every Header regardless of registration.
    #[default]
    All,
    /// One registered canonical workspace.
    Registered {
        /// Exact registration identity.
        id: WorkspaceId,
    },
    /// Headers whose canonical directory has no registration.
    Unregistered,
}
impl WorkspaceFilter {
    /// Matches the registration derived by the Host without changing it.
    pub fn matches(&self, workspace: Option<&WorkspaceId>) -> bool {
        match self {
            Self::All => true,
            Self::Registered { id } => workspace == Some(id),
            Self::Unregistered => workspace.is_none(),
        }
    }
}
/// Exact query identity retained by continuation cursors.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NavigationFilter {
    /// Case-insensitive title, path or `SessionId` substring, at most 128 bytes.
    pub query: String,
    /// Whether to show archived or active navigation records.
    pub archived: bool,
    /// Exact registered, unregistered or all-workspace partition.
    pub workspace: WorkspaceFilter,
}
impl NavigationFilter {
    /// Validates bounded query input before any scan.
    pub fn validate(&self) -> Result<()> {
        if self.query.len() > 128 || self.query.chars().any(char::is_control) {
            return Err(ApiError::Invalid(
                "navigation query exceeds 128 bytes or contains controls".into(),
            ));
        }
        Ok(())
    }
}
/// Exclusive continuation after the last scanned row, independent of matches.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NavigationCursor {
    /// Exact filter that produced this continuation.
    pub filter: NavigationFilter,
    /// Current Host generation.
    pub host_epoch: HostEpoch,
    /// Exact metadata revision.
    pub metadata_revision: String,
    /// Last scanned durable Store position.
    pub after: ActivityCursor,
}
/// One navigation result derived from durable Header and optional metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NavigationEntry {
    /// Durable Session identity.
    pub session: SessionId,
    /// Exact creation timestamp encoded as decimal text.
    pub created_at_ms: String,
    /// Exact activity timestamp encoded as decimal text.
    pub last_activity_ms: String,
    /// Immutable execution machine; never permission to use it.
    pub location: rsi_agent_session_protocol::ExecutionLocation,
    /// Immutable canonical target path.
    pub path: String,
    /// Exact registered workspace, or absent for unregistered historical roots.
    pub workspace: Option<WorkspaceId>,
    /// Host-owned navigation metadata.
    pub metadata: SessionMetadata,
}
/// Bounded query result, including empty continuations.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NavigationPage {
    /// Metadata revision used by this page and later explicit edits.
    pub metadata_revision: String,
    /// Newest activity key in the selected coordinate scope at this read.
    pub newest: Option<ActivityCursor>,
    /// At most 64 matches in descending activity order.
    pub entries: Vec<NavigationEntry>,
    /// Actual scanned row count, at most 256.
    pub scanned: u16,
    /// Exclusive continuation when more rows remain.
    pub next: Option<NavigationCursor>,
}
/// A pinned record remains removable when its durable Header has disappeared.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum PinnedEntry {
    /// Indexed durable identity and its metadata.
    Available {
        /// Complete navigation row.
        entry: NavigationEntry,
    },
    /// Stale pin, shown only by all-workspace queries because its path is unknown.
    Missing {
        /// Exact retained identity.
        session: SessionId,
        /// Complete metadata for explicit unpinning.
        metadata: SessionMetadata,
    },
}
/// All matching pins, independently bounded to 64 records.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedPage {
    /// Revision shared with ordinary navigation and edits.
    pub metadata_revision: String,
    /// Available rows descending by activity/identity, then missing IDs descending.
    pub entries: Vec<PinnedEntry>,
}
/// Exact metadata mutation receipt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataReceipt {
    /// New global metadata revision.
    pub revision: String,
    /// Session whose navigation was edited.
    pub session: SessionId,
    /// Complete resulting metadata.
    pub metadata: SessionMetadata,
}
/// Canonical exact revision parser shared by the endpoint owner and client.
pub fn revision(text: &str) -> Result<u64> {
    text.parse::<u64>()
        .ok()
        .filter(|value| text.len() <= 20 && value.to_string() == text)
        .ok_or_else(|| ApiError::Invalid("invalid navigation revision".into()))
}
/// Closed navigation wire operations.
#[derive(Clone, Copy, Debug)]
pub enum NavigationOperation {
    /// Read one bounded filtered page.
    Query,
    /// Read every matching pin without attaching a Session.
    Pinned,
    /// Read complete bounded manual-order membership.
    OrderSeed,
    /// Read exact device-ordered summary identities.
    Summaries,
    /// Replace one title/archive record against the global revision.
    Replace,
}
impl NavigationOperation {
    /// Exact authenticated operation descriptor.
    ///
    /// # Panics
    /// Panics if the static operation identities are invalid.
    pub fn spec(self) -> OperationSpec {
        OperationSpec {
            id: OperationId::new(
                "navigation",
                match self {
                    Self::Query => "query",
                    Self::Pinned => "pinned",
                    Self::OrderSeed => "order_seed",
                    Self::Summaries => "summaries",
                    Self::Replace => "replace",
                },
                3,
            )
            .expect("static navigation operation"),
            access: OperationAccess::Authenticated,
            class: OperationClass::Data,
            effect: match self {
                Self::Query | Self::Pinned | Self::OrderSeed | Self::Summaries => {
                    OperationEffect::Read
                }
                Self::Replace => OperationEffect::Mutation,
            },
            encoding: RequestEncoding::Json,
            maximum_request_bytes: match self {
                Self::Query | Self::Pinned | Self::Replace => 4096,
                Self::Summaries => 32 * 1024,
                Self::OrderSeed => 128 * 1024,
            },
            maximum_response_bytes: 2 * 1024 * 1024,
        }
    }
}
#[derive(Serialize, Deserialize)]
enum Never {}
/// Client that preserves query continuation and never replays uncertain edits.
#[derive(Clone, Debug)]
pub struct NavigationClient {
    api: Arc<dyn ApiClient>,
}
impl NavigationClient {
    /// Requires exact navigation operations.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if [
            NavigationOperation::Query,
            NavigationOperation::Pinned,
            NavigationOperation::Replace,
            NavigationOperation::OrderSeed,
            NavigationOperation::Summaries,
        ]
        .into_iter()
        .any(|operation| !api.operations().contains(&operation.spec()))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    /// Reads one page, validating remote bounds and query/Host continuation identity.
    pub async fn query(
        &self,
        filter: NavigationFilter,
        after: Option<NavigationCursor>,
    ) -> Result<NavigationPage> {
        #[derive(Serialize)]
        struct Query<'a> {
            filter: &'a NavigationFilter,
            after: &'a Option<NavigationCursor>,
        }
        filter.validate()?;
        let page = match call_json::<_, NavigationPage, Never>(
            self.api.as_ref(),
            &NavigationOperation::Query.spec(),
            &Query {
                filter: &filter,
                after: &after,
            },
        )
        .await?
        {
            Ok(value) => value,
            Err(never) => match never {},
        };
        validation::page(
            &page,
            &filter,
            after.as_ref(),
            &self.api.description().host_epoch,
        )?;
        Ok(page)
    }
    /// Reads all matching pins, rejecting duplicate, misfiltered or malformed rows.
    pub async fn pinned(&self, filter: NavigationFilter) -> Result<PinnedPage> {
        filter.validate()?;
        let page = match call_json::<_, PinnedPage, Never>(
            self.api.as_ref(),
            &NavigationOperation::Pinned.spec(),
            &filter,
        )
        .await?
        {
            Ok(value) => value,
            Err(never) => match never {},
        };
        validation::pins(&page, &filter)?;
        Ok(page)
    }
    /// Edits title/archive metadata once against the observed global revision.
    pub async fn replace(
        &self,
        session: SessionId,
        expected_revision: &str,
        mut metadata: SessionMetadata,
    ) -> Result<MetadataReceipt> {
        #[derive(Serialize)]
        struct Replace<'a> {
            session: &'a SessionId,
            expected_revision: &'a str,
            metadata: &'a SessionMetadata,
        }
        if metadata.archived {
            metadata.pinned = false;
        }
        metadata.validate()?;
        let next = revision(expected_revision)?
            .checked_add(1)
            .ok_or_else(|| ApiError::Invalid("navigation revision exhausted".into()))?;
        let receipt = match call_json::<_, MetadataReceipt, Never>(
            self.api.as_ref(),
            &NavigationOperation::Replace.spec(),
            &Replace {
                session: &session,
                expected_revision,
                metadata: &metadata,
            },
        )
        .await?
        {
            Ok(value) => value,
            Err(never) => match never {},
        };
        if receipt.revision != next.to_string()
            || receipt.session != session
            || receipt.metadata != metadata
        {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(receipt)
    }
}
/// Checks strict descending activity/identity progress through durable rows.
pub fn cursor_advances(previous: &ActivityCursor, next: &ActivityCursor) -> bool {
    (next.last_activity_ms, &next.session_id) < (previous.last_activity_ms, &previous.session_id)
}

/// Shared title/path/identity search semantics at both sides of the wire.
pub fn matches_query(
    query: &str,
    id: &SessionId,
    metadata: &SessionMetadata,
    path: Option<&str>,
) -> bool {
    query.is_empty()
        || id.as_str().to_lowercase().contains(query)
        || metadata
            .title
            .as_ref()
            .is_some_and(|title| title.to_lowercase().contains(query))
        || path.is_some_and(|path| path.to_lowercase().contains(query))
}

#[cfg(test)]
mod validation_tests;
