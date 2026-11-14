use super::{PluginsFeature, Result};
use rsi_configuration_api::leaf::{
    self as wire, Catalog, CatalogRequest, Change, ChangeResult, Grant, Grants, Outcome, Preview,
    PreviewRequest, Target,
};
use serde::{Deserialize, Serialize};

/// Closed human commands shared by terminal and graphical clients.
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeafCommand {
    /// Reads current source choices without retaining server resources.
    Read {
        /// Profile selection and lexical continuation.
        query: CatalogRequest,
    },
    /// Prepares one literal enabled-state change.
    Enabled {
        /// Exact source choice.
        target: Target,
        /// Requested own enabled state.
        enabled: bool,
    },
    /// Parses exact JSON in Rust before preparing a complete replacement.
    Configuration {
        /// Exact source choice.
        target: Target,
        /// Exact user input; retained privately only until review is consumed.
        document: String,
    },
    /// Accepts exactly the displayed prepared proposal.
    Commit {
        /// Displayed review digest.
        digest: String,
    },
    /// Clears the local review without making an API call.
    Discard,
    /// Refreshes the Local grant snapshot without granting anything.
    Grants,
    /// Explicit Local expected-revision grant mutation.
    Grant {
        /// Displayed grant document revision.
        revision: String,
        /// Exact principal, source and operation.
        scope: Grant,
        /// Desired authority state.
        granted: bool,
    },
}
impl std::fmt::Debug for LeafCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LeafCommand(<redacted>)")
    }
}
/// Redacted state retained by the one shared workbench owner.
#[derive(Clone, Debug, Default, Serialize)]
pub struct LeafView {
    /// Required closed API was negotiated.
    pub available: bool,
    /// This connection negotiated Local-only grant administration.
    pub can_grant: bool,
    /// Current explicit source/page selection.
    pub query: CatalogRequest,
    /// Current bounded page, cleared when reading fails.
    pub catalog: Option<Catalog>,
    /// Explicit Local grant observation.
    pub grants: Option<Grants>,
    /// One redacted, client-local review.
    pub preview: Option<Preview>,
    /// Complete change is never serialized to the document or persisted.
    #[serde(skip)]
    pending: Option<PreviewRequest>,
    /// Most recent direct result; not recovery evidence after reconnect.
    pub result: Option<ChangeResult>,
    /// Closed safe outcome or rejection guidance.
    pub notice: Option<String>,
}
impl PluginsFeature {
    pub(super) async fn leaf_command(&self, command: LeafCommand) -> Result<()> {
        let client = self
            .leaves
            .as_ref()
            .ok_or("Host Profile management is unavailable")?;
        let result = self.leaf_operation(client, command).await;
        if let Err(message) = &result {
            self.state.lock().expect("Profile workbench").leaves.notice = Some(message.clone());
        }
        result
    }
    async fn leaf_operation(&self, client: &wire::Client, command: LeafCommand) -> Result<()> {
        match command {
            LeafCommand::Read { query } => {
                {
                    let mut state = self.state.lock().expect("Profile source selection");
                    state.leaves.query = query.clone();
                    state.leaves.catalog = None;
                }
                let catalog = response(client.catalog(query).await)?;
                let mut state = self.state.lock().expect("Profile source page");
                state.leaves.catalog = Some(catalog);
                state.leaves.preview = None;
                state.leaves.pending = None;
                state.leaves.notice = None;
            }
            LeafCommand::Enabled { target, enabled } => {
                self.leaf_prepare(
                    client,
                    PreviewRequest {
                        target,
                        change: Change::Enabled { enabled },
                    },
                )
                .await?;
            }
            LeafCommand::Configuration { target, document } => {
                if document.len() > 64 * 1024 {
                    return Err("Configuration exceeds 64 KiB".into());
                }
                let value = serde_json::from_str(&document)
                    .map_err(|_| "Enter a valid complete JSON configuration".to_owned())?;
                wire::validate_configuration(&value)
                    .map_err(|_| "Configuration exceeds its size or nesting bounds".to_owned())?;
                self.leaf_prepare(
                    client,
                    PreviewRequest {
                        target,
                        change: Change::Configuration { value },
                    },
                )
                .await?;
            }
            LeafCommand::Commit { digest } => {
                self.leaf_commit(client, &digest).await?;
            }
            LeafCommand::Discard => {
                let mut state = self.state.lock().expect("Profile review discard");
                state.leaves.preview = None;
                state.leaves.pending = None;
                state.leaves.notice = None;
            }
            LeafCommand::Grants => {
                let grants = response(client.grants().await)?;
                self.state.lock().expect("Profile grants").leaves.grants = Some(grants);
            }
            LeafCommand::Grant {
                revision,
                scope,
                granted,
            } => {
                let grants = response(
                    client
                        .set_grant(wire::SetGrant {
                            expected: revision,
                            scope,
                            granted,
                        })
                        .await,
                )?;
                let query = {
                    let mut state = self.state.lock().expect("Profile grant receipt");
                    state.leaves.grants = Some(grants);
                    state.leaves.notice = Some("Explicit Profile grant saved".into());
                    state.leaves.query.clone()
                };
                let catalog = response(client.catalog(query).await).ok();
                self.state
                    .lock()
                    .expect("Profile grant observation")
                    .leaves
                    .catalog = catalog;
            }
        }
        Ok(())
    }
    async fn leaf_prepare(&self, client: &wire::Client, request: PreviewRequest) -> Result<()> {
        let preview = response(client.preview(request.clone()).await)?;
        let mut state = self.state.lock().expect("Profile review");
        state.leaves.preview = Some(preview);
        state.leaves.pending = Some(request);
        state.leaves.result = None;
        state.leaves.notice = Some("Review this change before saving.".into());
        Ok(())
    }
    async fn leaf_commit(&self, client: &wire::Client, digest: &str) -> Result<()> {
        let (preview, request) = {
            let mut state = self.state.lock().expect("Profile review acceptance");
            if state
                .leaves
                .preview
                .as_ref()
                .is_none_or(|value| value.digest != digest)
            {
                return Err("Review changed; prepare the change again".into());
            }
            let preview = state.leaves.preview.take().expect("checked review");
            let request = state.leaves.pending.take().expect("review owns its change");
            state.leaves.result = Some(ChangeResult {
                preview: preview.clone(),
                outcome: Outcome::Pending,
            });
            (preview, request)
        };
        let result = match client.commit(&preview, request).await {
            Ok(Ok(result)) => result,
            Ok(Err(failure)) => ChangeResult {
                preview,
                outcome: Outcome::Failed { failure },
            },
            Err(rsi_api_protocol::ApiError::OutcomeUnknown) => ChangeResult {
                preview,
                outcome: Outcome::Unknown,
            },
            Err(error) => {
                self.state
                    .lock()
                    .expect("Profile refused call")
                    .leaves
                    .result = None;
                return response::<()>(Err(error));
            }
        };
        let mut state = self.state.lock().expect("Profile result");
        state.leaves.notice = Some(match &result.outcome {
            Outcome::Pending => "Applying the change.".into(),
            Outcome::Unknown => "Write outcome unknown. Refresh current source and runtime state before further work; it does not prove the original result.".into(),
            Outcome::Failed { failure } => failure_message(failure),
            Outcome::Saved { .. } => "Source saved. Runtime application is reported separately.".into(),
        });
        state.leaves.result = Some(result);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
fn response<T>(value: wire::Reply<T>) -> Result<T> {
    match value {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(failure)) => Err(failure_message(&failure)),
        Err(rsi_api_protocol::ApiError::Unauthorized) => {
            Err("Profile management requires explicit operator authority".into())
        }
        Err(rsi_api_protocol::ApiError::Capacity) => {
            Err("Profile management is busy; refresh after the current operation completes".into())
        }
        Err(rsi_api_protocol::ApiError::OutcomeUnknown) => Err(
            "Operation outcome unknown. Read current state before further work; do not blindly replay it."
                .into(),
        ),
        Err(_) => Err("Host Profile observation unavailable; reconnect and refresh".into()),
    }
}
fn failure_message(failure: &wire::Failure) -> String {
    match failure {
        wire::Failure::Conflict => "Profile sources changed. Prepare a new review.".into(),
        wire::Failure::NotLeaf => "Select one existing plugin leaf".into(),
        wire::Failure::DisabledAncestor(parent) => {
            format!("Enable is blocked by disabled group {parent}")
        }
        wire::Failure::Preparation => "Plugin configuration preparation was rejected".into(),
        wire::Failure::Source => "Profile source is unavailable or cannot be safely edited".into(),
        wire::Failure::Busy => "Profile management capacity is occupied".into(),
        wire::Failure::Unauthorized => {
            "This exact Profile change has no current grant for this principal".into()
        }
    }
}
