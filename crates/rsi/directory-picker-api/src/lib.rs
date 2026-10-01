//! Bounded standard-product directory selection over an authenticated API.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
use async_trait::async_trait;
use rsi_api_protocol::{
    ApiClient, ApiClientContract, ApiError, OperationAccess, OperationClass, OperationEffect,
    OperationId, OperationSpec, RequestEncoding, call_json,
};
pub use rsi_execution_protocol::ExecutionLocation;
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use serde::{Deserialize, Serialize};
use std::{fmt, sync::Arc};
/// Maximum encoded directory reply bytes.
pub const MAXIMUM_REPLY: usize = 2 * 1024 * 1024;
/// Maximum selectable entries in one sorted window.
pub const MAXIMUM_ENTRIES: usize = 1000;
/// Maximum UTF-8 path bytes.
pub const MAXIMUM_PATH: usize = 16 * 1024;
/// Known domain failures, distinct from transport uncertainty.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Failure {
    /// No native implementation on this Host.
    Unsupported,
    /// Composition did not supply a default home.
    HomeUnavailable,
    /// Input failed the picker contract.
    Invalid,
    /// Filesystem access failed before a mutation succeeded.
    Io {
        /// Bounded human-readable I/O category.
        message: String,
    },
    /// The caller or plugin retired before further work.
    Cancelled,
    /// Read deadline elapsed while actual work may still drain.
    TimedOut,
    /// Creation may have succeeded; read back explicitly before another action.
    OutcomeUnknown,
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported=>f.write_str("Directory browsing is unsupported on this Host; enter a path manually"),
            Self::HomeUnavailable=>f.write_str("Host home is unavailable; enter an absolute directory path"),
            Self::Invalid=>f.write_str("Invalid directory path or folder name"),
            Self::Io{message}=>write!(f,"Directory access failed: {message}"),
            Self::Cancelled=>f.write_str("Directory request cancelled"),
            Self::TimedOut=>f.write_str("Directory read timed out"),
            Self::OutcomeUnknown=>f.write_str("Folder creation outcome is unknown. Read the parent directory to check before creating again"),
        }
    }
}
/// Domain result inside the authenticated transport result.
pub type Result<T> = std::result::Result<T, Failure>;
/// Explicit location selection; the contained data does not confer authority.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AtLocation<T> {
    /// The machine whose directory namespace is selected.
    pub location: ExecutionLocation,
    /// Closed operation-specific input.
    pub request: T,
}
/// Platform and current grant availability; contains no paths.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    /// Platform implementation exists.
    pub supported: bool,
    /// The authenticated origin currently has a configuration grant.
    pub allowed: bool,
}
/// One explicit list request; no cursor is accepted.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListRequest {
    /// None selects composition's captured home.
    pub path: Option<String>,
}
/// One directory name and its physical target.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Exact UTF-8 name in the requested parent.
    pub name: String,
    /// Resolved physical directory.
    pub path: String,
    /// Name starts with a dot.
    pub hidden: bool,
    /// The selected entry is a symbolic link.
    pub symlink: bool,
}
/// Bounded physical directory window with explicit omissions.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listing {
    /// Echo of the original path selection.
    pub requested: Option<String>,
    /// Physical opened parent.
    pub path: String,
    /// Captured home, resolved to its physical directory if available.
    pub home: Option<String>,
    /// Path components after the filesystem root.
    pub breadcrumbs: Vec<String>,
    /// Sorted selectable directories.
    pub entries: Vec<Entry>,
    /// Entry or encoded-byte bound omitted further entries.
    pub truncated: bool,
    /// Non-UTF-8 directory names or targets were skipped.
    pub unrepresentable: bool,
}
/// Explicit single-level creation request.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    /// Absolute parent, resolved before acquiring its handle.
    pub parent: String,
    /// One valid name fragment.
    pub name: String,
}
/// Confirmed created path bound to the original request.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Created {
    /// Original parent selection.
    pub requested_parent: String,
    /// Physical parent used by the operation.
    pub parent: String,
    /// Original name.
    pub name: String,
    /// Physical result.
    pub path: String,
}
/// Validates UTF-8 absolute Unix paths without performing filesystem I/O.
pub fn validate_path(path: &str) -> Result<()> {
    if !path.starts_with('/') || path.len() > MAXIMUM_PATH || path.contains('\0') {
        Err(Failure::Invalid)
    } else {
        Ok(())
    }
}
/// Validates exactly one creation name before mutation.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 255
        || matches!(name, "." | "..")
        || name.contains(['/', '\\', '\0'])
    {
        Err(Failure::Invalid)
    } else {
        Ok(())
    }
}
fn validate_physical(path: &str) -> Result<()> {
    validate_path(path)?;
    if path != "/"
        && path[1..]
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(Failure::Invalid);
    }
    Ok(())
}
impl Listing {
    /// Validates a remote window before it reaches presentation code.
    pub fn validate(&self, request: &ListRequest) -> Result<()> {
        validate_physical(&self.path)?;
        if let Some(home) = &self.home {
            validate_physical(home)?;
        }
        if self.requested != request.path
            || self.entries.len() > MAXIMUM_ENTRIES
            || self.breadcrumbs
                != self
                    .path
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
        {
            return Err(Failure::Invalid);
        }
        let mut previous: Option<&str> = None;
        for entry in &self.entries {
            // Unix filenames may include backslashes; only creation forbids them.
            if entry.name.is_empty()
                || entry.name.len() > 255
                || entry.name.contains(['/', '\0'])
                || matches!(entry.name.as_str(), "." | "..")
                || entry.hidden != entry.name.starts_with('.')
                || previous.is_some_and(|previous| previous >= entry.name.as_str())
            {
                return Err(Failure::Invalid);
            }
            validate_physical(&entry.path)?;
            if !entry.symlink
                && entry.path != format!("{}/{}", self.path.trim_end_matches('/'), entry.name)
            {
                return Err(Failure::Invalid);
            }
            previous = Some(&entry.name);
        }
        if serde_json::to_vec(self)
            .map_err(|_| Failure::Invalid)?
            .len()
            > MAXIMUM_REPLY
        {
            return Err(Failure::Invalid);
        }
        Ok(())
    }
}
/// Versioned picker operations.
#[derive(Clone, Copy, Debug)]
pub enum Operation {
    /// Platform status.
    Status,
    /// Physical directory window.
    List,
    /// Single-level creation.
    Create,
}
impl Operation {
    /// Static authenticated operation descriptor.
    ///
    /// # Panics
    /// Panics only if the static operation identifiers violate their contract.
    pub fn spec(self) -> OperationSpec {
        OperationSpec {
            id: OperationId::new(
                "directory-picker",
                match self {
                    Self::Status => "status",
                    Self::List => "list",
                    Self::Create => "create",
                },
                2,
            )
            .expect("static operation"),
            access: OperationAccess::Authenticated,
            class: OperationClass::Data,
            effect: match self {
                Self::Create => OperationEffect::Mutation,
                _ => OperationEffect::Read,
            },
            encoding: RequestEncoding::Json,
            maximum_request_bytes: 128 * 1024,
            maximum_response_bytes: MAXIMUM_REPLY,
        }
    }
}
/// One transport-independent client used by native and Worker applications.
#[derive(Clone, Debug)]
pub struct Client {
    api: Arc<dyn ApiClient>,
}
impl Client {
    /// Requires the exact complete operation catalog.
    pub fn new(api: Arc<dyn ApiClient>) -> rsi_api_protocol::Result<Self> {
        if [Operation::Status, Operation::List, Operation::Create]
            .into_iter()
            .any(|operation| !api.operations().contains(&operation.spec()))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    async fn call<I: Serialize + Sync, O: serde::de::DeserializeOwned>(
        &self,
        operation: Operation,
        input: &I,
    ) -> rsi_api_protocol::Result<Result<O>> {
        let result =
            call_json::<_, O, Failure>(self.api.as_ref(), &operation.spec(), input).await?;
        if matches!(&result,Err(Failure::Io{message}) if message.len()>512) {
            return Err(ApiError::Invalid("Oversized directory diagnostic".into()));
        }
        Ok(result)
    }
    /// Reads platform/grant status without filesystem access.
    pub async fn status(
        &self,
        location: ExecutionLocation,
    ) -> rsi_api_protocol::Result<Result<Status>> {
        self.call(
            Operation::Status,
            &AtLocation {
                location,
                request: (),
            },
        )
        .await
    }
    /// Reads a bounded physical window without retrying failures.
    pub async fn list(
        &self,
        location: ExecutionLocation,
        request: ListRequest,
    ) -> rsi_api_protocol::Result<Result<Listing>> {
        if let Some(path) = &request.path
            && let Err(error) = validate_path(path)
        {
            return Ok(Err(error));
        }
        let result: Result<Listing> = self
            .call(
                Operation::List,
                &AtLocation {
                    location,
                    request: &request,
                },
            )
            .await?;
        if let Ok(listing) = &result {
            listing
                .validate(&request)
                .map_err(|_| ApiError::Invalid("Invalid directory listing".into()))?;
        }
        Ok(result)
    }
    /// Creates once; transport uncertainty never authorizes an automatic retry.
    pub async fn create(
        &self,
        location: ExecutionLocation,
        request: CreateRequest,
    ) -> rsi_api_protocol::Result<Result<Created>> {
        if let Err(error) =
            validate_path(&request.parent).and_then(|()| validate_name(&request.name))
        {
            return Ok(Err(error));
        }
        let result: Result<Created> = self
            .call(
                Operation::Create,
                &AtLocation {
                    location,
                    request: &request,
                },
            )
            .await?;
        if let Ok(created) = &result
            && created.validate(&request).is_err()
        {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(result)
    }
}
impl Created {
    /// Checks mutation acknowledgement against the exact submitted identity.
    pub fn validate(&self, request: &CreateRequest) -> Result<()> {
        if self.requested_parent != request.parent
            || self.name != request.name
            || validate_physical(&self.parent).is_err()
            || validate_physical(&self.path).is_err()
            || self.path != format!("{}/{}", self.parent.trim_end_matches('/'), self.name)
        {
            Err(Failure::OutcomeUnknown)
        } else {
            Ok(())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn listing() -> Listing {
        Listing {
            requested: Some("/alias".into()),
            path: "/physical".into(),
            home: Some("/".into()),
            breadcrumbs: vec!["physical".into()],
            entries: vec![
                Entry {
                    name: ".hidden".into(),
                    path: "/physical/.hidden".into(),
                    hidden: true,
                    symlink: false,
                },
                Entry {
                    name: "link".into(),
                    path: "/elsewhere".into(),
                    hidden: false,
                    symlink: true,
                },
            ],
            truncated: false,
            unrepresentable: false,
        }
    }
    #[test]
    fn remote_windows_reject_false_identity_order_and_physical_paths() {
        let request = ListRequest {
            path: Some("/alias".into()),
        };
        let valid = listing();
        assert_eq!(valid.validate(&request), Ok(()));
        for damage in 0..8 {
            let mut value = valid.clone();
            match damage {
                0 => value.requested = None,
                1 => value.entries.reverse(),
                2 => value.entries[0].hidden = false,
                3 => value.entries[0].path = "/elsewhere".into(),
                4 => value.entries[1].path = "/elsewhere/../secret".into(),
                5 => value.breadcrumbs.clear(),
                6 => value.entries.push(value.entries[1].clone()),
                _ => value.home = Some("//physical".into()),
            }
            assert_eq!(
                value.validate(&request),
                Err(Failure::Invalid),
                "damage {damage}"
            );
        }
    }
    #[test]
    fn remote_windows_enforce_count_and_encoded_bytes_independently() {
        let request = ListRequest {
            path: Some("/alias".into()),
        };
        let mut value = listing();
        value.entries = (0..=MAXIMUM_ENTRIES)
            .map(|index| Entry {
                name: format!("{index:04}"),
                path: "/target".into(),
                hidden: false,
                symlink: true,
            })
            .collect();
        assert_eq!(value.validate(&request), Err(Failure::Invalid));
        value.entries.pop();
        assert_eq!(value.validate(&request), Ok(()));
        for entry in &mut value.entries {
            entry.path = format!("/{}", "a".repeat(2200));
        }
        assert_eq!(value.validate(&request), Err(Failure::Invalid));
    }
    #[test]
    fn creation_names_never_allow_multiple_components_or_empty_targets() {
        for name in ["", ".", "..", "a/b", "a\\b", "a\0b"] {
            assert_eq!(validate_name(name), Err(Failure::Invalid));
        }
        assert_eq!(validate_name(&"界".repeat(86)), Err(Failure::Invalid));
        assert_eq!(validate_name("My folder"), Ok(()));
        assert_eq!(validate_name(".hidden"), Ok(()));
    }
}
/// Nominal client capability.
#[derive(Debug)]
pub struct ClientContract;
impl LocalContract for ClientContract {
    const KEY: &'static str = "rsi.directory-picker.client";
    type Service = Client;
}
/// Ordinary shared API client plugin.
#[derive(Clone, Debug, Default)]
pub struct ClientFactory;
#[async_trait]
impl PluginFactory for ClientFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "Directory client config must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<ApiClientContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let client = Client::new(plan.local::<ApiClientContract>()?)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let supply = plan
            .context()
            .provide_local::<ClientContract>(Arc::new(client))?;
        plan.defer(
            "withdraw directory client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
