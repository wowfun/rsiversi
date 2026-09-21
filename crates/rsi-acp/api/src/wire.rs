use rsi_acp_protocol::{
    observation::{ConversationId, Page},
    service::{Error, Result, Setup},
};
use rsi_api_protocol::{
    OperationAccess, OperationClass, OperationEffect, OperationId, OperationSpec, RequestEncoding,
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy)]
pub(super) enum Operation {
    Endpoints,
    Residents,
    List,
    View,
    Start,
    Reconnect,
    Submit,
    Cancel,
    Close,
    Answer,
    Page,
    Window,
}
impl Operation {
    pub const ALL: [Self; 12] = [
        Self::Endpoints,
        Self::Residents,
        Self::List,
        Self::View,
        Self::Start,
        Self::Reconnect,
        Self::Submit,
        Self::Cancel,
        Self::Close,
        Self::Answer,
        Self::Page,
        Self::Window,
    ];
    pub fn spec(self) -> OperationSpec {
        use OperationClass::{Control, Data};
        use OperationEffect::{Mutation, Read};
        let (name, class, effect, input, output) = match self {
            Self::Endpoints => ("endpoints", Data, Read, 64, 32 * 1024),
            Self::Residents => ("residents", Data, Read, 64, 2 * 1024 * 1024),
            Self::List => ("list", Data, Read, 1024, 2 * 1024 * 1024),
            Self::View => ("view", Data, Read, 1024, 4 * 1024 * 1024),
            Self::Start => ("start", Control, Mutation, 2048, 32 * 1024),
            Self::Reconnect => ("reconnect", Control, Mutation, 2048, 32 * 1024),
            Self::Submit => ("submit", Data, Mutation, 4 * 1024 * 1024, 32 * 1024),
            Self::Cancel => ("cancel", Control, Mutation, 1024, 32 * 1024),
            Self::Close => ("close", Control, Mutation, 1024, 32 * 1024),
            Self::Answer => ("answer", Control, Mutation, 8192, 64),
            Self::Page => ("page", Data, Read, 2048, 320 * 1024),
            Self::Window => ("window", Data, Read, 2048, 132 * 1024),
        };
        OperationSpec {
            id: OperationId::new("external-conversation", name, 1).expect("static operation"),
            class,
            effect,
            access: OperationAccess::Authenticated,
            encoding: RequestEncoding::Json,
            maximum_request_bytes: input,
            maximum_response_bytes: output,
        }
    }
}
pub(super) fn number(text: &str) -> Result<u64> {
    let number = text.parse::<u64>().map_err(|_| Error::Input)?;
    if number.to_string() != text || number > i64::MAX as u64 {
        return Err(Error::Input);
    }
    Ok(number)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Empty {}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Target {
    pub id: ConversationId,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Start {
    pub id: ConversationId,
    pub endpoint: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reconnect {
    pub id: ConversationId,
    pub setup: Setup,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Submit {
    pub id: ConversationId,
    pub text: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct List {
    pub after: Option<ConversationId>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Answer {
    pub id: ConversationId,
    pub generation: String,
    pub permission: String,
    pub option: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PageRequest {
    pub id: ConversationId,
    pub epoch: String,
    pub after: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PageReply {
    pub source: PageRequest,
    pub page: Page,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WindowRequest {
    pub id: ConversationId,
    pub epoch: String,
    pub sequence: String,
    pub start: usize,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WindowReply {
    pub source: WindowRequest,
    pub hex: String,
}
