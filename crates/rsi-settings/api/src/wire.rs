use rsi_api_protocol::{
    ApiError, OperationClass, OperationEffect, OperationId, OperationSpec, RequestEncoding,
};
use rsi_settings_protocol::{SettingsError, SettingsVersion};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy)]
pub(crate) enum Operation {
    List,
    Describe,
    Read,
    Replace,
    Clear,
}
impl Operation {
    pub const ALL: [Self; 5] = [
        Self::List,
        Self::Describe,
        Self::Read,
        Self::Replace,
        Self::Clear,
    ];
    pub fn spec(self) -> OperationSpec {
        let (name, effect) = match self {
            Self::List => ("list", OperationEffect::Read),
            Self::Describe => ("describe", OperationEffect::Read),
            Self::Read => ("read", OperationEffect::Read),
            Self::Replace => ("replace", OperationEffect::Mutation),
            Self::Clear => ("clear", OperationEffect::Mutation),
        };
        OperationSpec {
            id: OperationId::new("settings", name, 1).expect("constant operation"),
            class: OperationClass::Data,
            effect,
            access: rsi_api_protocol::OperationAccess::Authenticated,
            encoding: RequestEncoding::Json,
            maximum_request_bytes: if matches!(self, Self::Replace) {
                8 * 1024 * 1024
            } else {
                1024
            },
            maximum_response_bytes: 8 * 1024 * 1024,
        }
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct List {
    pub after: Option<String>,
    pub limit: usize,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Read {
    pub namespace: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Replace {
    pub namespace: String,
    pub expected: SettingsVersion,
    pub value: Value,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Clear {
    pub namespace: String,
    pub expected: SettingsVersion,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Failure {
    UnknownNamespace,
    InvalidInput,
    StaleRegistration,
    Conflict { expected: u64, actual: u64 },
    ConcurrentDocumentChange,
    ReadOnly,
    Corrupt,
}
pub(crate) fn result<T>(
    result: rsi_settings_protocol::Result<T>,
) -> rsi_api_protocol::Result<std::result::Result<T, Failure>> {
    Ok(match result {
        Ok(snapshot) => Ok(snapshot),
        Err(error) => Err(match error {
            SettingsError::Api(error) => return Err(error),
            SettingsError::UnknownNamespace(_) => Failure::UnknownNamespace,
            SettingsError::InvalidInput(_) => Failure::InvalidInput,
            SettingsError::StaleRegistration(_) => Failure::StaleRegistration,
            SettingsError::Conflict { expected, actual } => Failure::Conflict { expected, actual },
            SettingsError::ConcurrentDocumentChange => Failure::ConcurrentDocumentChange,
            SettingsError::ReadOnly => Failure::ReadOnly,
            SettingsError::Corrupt(_) => Failure::Corrupt,
            SettingsError::Io(_) | SettingsError::DuplicateNamespace(_) => {
                return Err(ApiError::Backend(
                    "settings provider or commit task failed".into(),
                ));
            }
        }),
    })
}
impl Failure {
    pub fn into_error(self, namespace: &str, version: Option<&SettingsVersion>) -> SettingsError {
        match self {
            Self::UnknownNamespace => SettingsError::UnknownNamespace(namespace.into()),
            Self::InvalidInput => {
                SettingsError::InvalidInput("remote namespace rejected input".into())
            }
            Self::StaleRegistration => SettingsError::StaleRegistration(namespace.into()),
            Self::Conflict { expected, actual }
                if version
                    .is_some_and(|version| expected == version.revision && expected != actual) =>
            {
                SettingsError::Conflict { expected, actual }
            }
            Self::Conflict { .. } => SettingsError::Api(if version.is_some() {
                ApiError::OutcomeUnknown
            } else {
                ApiError::Invalid("invalid Settings conflict response".into())
            }),
            Self::ConcurrentDocumentChange => SettingsError::ConcurrentDocumentChange,
            Self::ReadOnly => SettingsError::ReadOnly,
            Self::Corrupt => SettingsError::Corrupt("remote settings document is invalid".into()),
        }
    }
}
