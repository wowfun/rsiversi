use super::{
    ApiClient, ApiError, Arc, Deserialize, HostEpoch, Never, OperationAccess, OperationClass,
    OperationEffect, OperationId, OperationSpec, RequestEncoding, Result, Serialize, call_json,
    revision,
};
pub use rsi_conversation::ConversationIdentity;
use rsi_session_protocol::ActivityRequest;

/// Exact visible source cut, independent of execution authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    /// Native or external source owner.
    pub conversation: ConversationIdentity,
    /// Zero for append-only native history, otherwise the external replay epoch.
    pub epoch: String,
    /// Last observed durable/locally journaled record.
    pub sequence: String,
}
impl Position {
    /// Rejects noncanonical and backend-incompatible coordinates.
    pub fn validate(&self) -> Result<()> {
        let epoch = revision(&self.epoch)?;
        let sequence = revision(&self.sequence)?;
        if matches!(self.conversation, ConversationIdentity::Native(_)) != (epoch == 0)
            || matches!(self.conversation, ConversationIdentity::External(_))
                && (epoch > i64::MAX as u64 || sequence > i64::MAX as u64)
        {
            return Err(ApiError::Invalid("invalid attention position".into()));
        }
        Ok(())
    }
}
/// Target identity revalidated by the owning broker when opened or answered.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    /// Exact native approval or question request.
    Native {
        /// Native broker tuple within the row's Session.
        request: ActivityRequest,
    },
    /// Exact ACP permission in one connection generation.
    External {
        /// Connection generation, encoded without precision loss.
        generation: String,
        /// Opaque peer request identity.
        request: String,
    },
}
/// Presentation priority; no member implies external-effect settlement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// An exact human interaction is pending.
    Waiting,
    /// Current owner confirms nonterminal work.
    Running,
    /// Current ownership cannot establish a running or settled state.
    Unknown,
    /// Idle history has advanced beyond this principal's reading position.
    Unread,
}
/// One attention candidate without transcript text, configuration or credentials.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Exact source and observed watermark.
    pub position: Position,
    /// Current presentation priority.
    pub status: Status,
    /// At most 32 pending interaction identities.
    pub targets: Vec<Target>,
}
/// Complete bounded attention read for the authenticated caller.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    /// Current owner identity; writes must retain this fence.
    pub host_epoch: HostEpoch,
    /// At most 136 distinct candidates, in priority and identity order.
    pub entries: Vec<Entry>,
    /// Some candidates or targets were omitted due to their explicit bounds.
    pub truncated: bool,
}
impl Page {
    /// Validates remote/durable identities and bounded presentation metadata.
    pub fn validate(&self) -> Result<()> {
        if self.entries.len() > 136 {
            return Err(ApiError::Invalid("attention row bound".into()));
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.entries {
            entry.position.validate()?;
            if !seen.insert(&entry.position.conversation)
                || entry.targets.len() > 32
                || (entry.status == Status::Waiting) == entry.targets.is_empty()
            {
                return Err(ApiError::Invalid("invalid attention targets".into()));
            }
            for target in &entry.targets {
                let request = match (target, &entry.position.conversation) {
                    (Target::Native { request }, ConversationIdentity::Native(_)) => {
                        match request {
                            ActivityRequest::Approval { request, .. }
                            | ActivityRequest::Question { request, .. } => request,
                        }
                    }
                    (
                        Target::External {
                            generation,
                            request,
                        },
                        ConversationIdentity::External(_),
                    ) if revision(generation)? > 0 => request,
                    _ => return Err(ApiError::Invalid("attention backend mismatch".into())),
                };
                if request.is_empty()
                    || request.len() > 256
                    || request.chars().any(char::is_control)
                {
                    return Err(ApiError::Invalid(
                        "invalid attention request identity".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}
/// Closed authenticated attention API.
#[derive(Clone, Copy, Debug)]
pub enum Operation {
    /// Read current-owner metadata.
    Read,
    /// Acknowledge one explicitly displayed source cut.
    MarkRead,
}
impl Operation {
    /// Returns the exact bounded authenticated operation descriptor.
    ///
    /// # Panics
    /// Static API names are valid identifiers.
    pub fn spec(self) -> OperationSpec {
        OperationSpec {
            id: OperationId::new(
                "attention",
                match self {
                    Self::Read => "read",
                    Self::MarkRead => "mark_read",
                },
                1,
            )
            .expect("static attention name"),
            access: OperationAccess::Authenticated,
            class: OperationClass::Data,
            effect: match self {
                Self::Read => OperationEffect::Read,
                Self::MarkRead => OperationEffect::Mutation,
            },
            encoding: RequestEncoding::Json,
            maximum_request_bytes: 4096,
            maximum_response_bytes: 256 * 1024,
        }
    }
}
/// One explicit acknowledgment, fenced to the observed Host generation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkRead {
    /// Exact observed Host.
    pub host_epoch: HostEpoch,
    /// Exact source cut displayed to this caller.
    pub position: Position,
}
/// Transport-independent client; an uncertain acknowledgment is never replayed.
#[derive(Clone, Debug)]
pub struct Client {
    api: Arc<dyn ApiClient>,
}
impl Client {
    /// Checks exact advertised operations.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if [Operation::Read, Operation::MarkRead]
            .into_iter()
            .any(|op| !api.operations().contains(&op.spec()))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    /// Reads a bounded current-owner view.
    pub async fn read(&self) -> Result<Page> {
        let page = match call_json::<_, Page, Never>(
            self.api.as_ref(),
            &Operation::Read.spec(),
            &serde_json::json!({}),
        )
        .await?
        {
            Ok(page) => page,
            Err(never) => match never {},
        };
        page.validate()?;
        if page.host_epoch != self.api.description().host_epoch {
            return Err(ApiError::Unavailable);
        }
        Ok(page)
    }
    /// Marks only the displayed source position read for this authenticated caller.
    pub async fn mark_read(&self, position: Position) -> Result<Position> {
        position.validate()?;
        let request = MarkRead {
            host_epoch: self.api.description().host_epoch.clone(),
            position: position.clone(),
        };
        let result = match call_json::<_, Position, Never>(
            self.api.as_ref(),
            &Operation::MarkRead.spec(),
            &request,
        )
        .await?
        {
            Ok(value) => value,
            Err(never) => match never {},
        };
        result.validate()?;
        if result.conversation != position.conversation
            || result.epoch != position.epoch
            || revision(&result.sequence)? < revision(&position.sequence)?
        {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(result)
    }
}
