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
#[derive(schemars::JsonSchema)]
pub struct Position {
    /// Native or external source owner.
    pub conversation: ConversationIdentity,
    /// Zero for append-only native history, otherwise the external replay epoch.
    pub epoch: String,
    /// Latest observed live native terminal or external journal record.
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
    /// A live completion or an explicit reminder awaits reading.
    Unread,
}
/// Host-shared reminder state for one authorized conversation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reading {
    /// A completion beyond the latest acknowledged cut.
    pub unseen: bool,
    /// An explicit reminder that survives automatic acknowledgments.
    pub manual: bool,
    /// The most recent completion ended in error.
    pub failed: bool,
}

impl Reading {
    /// Whether the conversation should show an unread reminder.
    pub fn unread(self) -> bool {
        self.manual || self.unseen
    }
}

/// One attention candidate without transcript text, configuration or credentials.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Exact source and observed watermark.
    pub position: Position,
    /// Current presentation priority.
    pub status: Status,
    /// Reading state, independent of the current running/waiting priority.
    pub reading: Reading,
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
    Mark,
}
impl Operation {
    /// Returns the exact bounded authenticated operation descriptor.
    ///
    /// # Panics
    /// Static API names are valid identifiers.
    pub fn spec(self) -> OperationSpec {
        OperationSpec {
            protocol: rsi_api_protocol::wire_contract!(),
            id: OperationId::new(
                "attention",
                match self {
                    Self::Read => "read",
                    Self::Mark => "mark",
                },
                1,
            )
            .expect("static attention name"),
            access: OperationAccess::Authenticated,
            class: OperationClass::Data,
            effect: match self {
                Self::Read => OperationEffect::Read,
                Self::Mark => OperationEffect::Mutation,
            },
            encoding: RequestEncoding::Json,
            maximum_request_bytes: 4096,
            maximum_response_bytes: 256 * 1024,
        }
    }
}
/// User or visible-transcript reading action.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadAction {
    /// Clear the manual reminder and advance through the observed completion.
    Read,
    /// Set a reminder without moving the shared read position.
    Unread,
    /// Advance the shared position and preserve the manual reminder.
    Acknowledge,
}

/// A reading action fenced to the observed Host generation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mark {
    /// Intent from the authenticated caller.
    pub action: ReadAction,
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
    /// Creates a typed proxy; availability is resolved when each operation is called.
    pub fn new(api: Arc<dyn ApiClient>) -> Self {
        Self { api }
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
    /// Clears the shared reminder through the displayed position.
    pub async fn mark_read(&self, position: Position) -> Result<Position> {
        self.mark(Mark {
            host_epoch: self.api.description().host_epoch.clone(),
            position,
            action: ReadAction::Read,
        })
        .await
    }

    /// Changes the shared reminder through an observed cut.
    pub async fn mark(&self, request: Mark) -> Result<Position> {
        let position = &request.position;
        position.validate()?;
        if request.host_epoch != self.api.description().host_epoch {
            return Err(ApiError::Unavailable);
        }
        let result = match call_json::<_, Position, Never>(
            self.api.as_ref(),
            &Operation::Mark.spec(),
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
