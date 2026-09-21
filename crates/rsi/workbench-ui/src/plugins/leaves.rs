use super::{PluginsFeature, Result};
use rsi_configuration_api::leaf::{
    self as wire, Catalog, CatalogRequest, Change, Grant, Grants, Outcome, Preview, PreviewRequest,
    Receipt, Target,
};
use serde::{Deserialize, Serialize};

/// Closed human commands shared by terminal and graphical clients.
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeafCommand {
    /// Reads an exact bounded page and recovers owned proposal/receipt identities.
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
        /// Ephemeral user input; never retained by a view.
        document: String,
    },
    /// Accepts exactly the displayed prepared proposal.
    Commit {
        /// Existing prepared ticket.
        ticket: String,
        /// Displayed review digest.
        digest: String,
    },
    /// Reads an original receipt, including after an unknown mutation reply.
    Receipt {
        /// Exact owned ticket.
        ticket: String,
    },
    /// Frees an unused prepared proposal.
    Discard {
        /// Exact owned ticket.
        ticket: String,
    },
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
    /// At most four recoverable, owned prepared proposals.
    pub previews: Vec<Preview>,
    /// At most 256 exact owned receipt identities, recoverable after reconnect.
    pub receipts: Vec<String>,
    /// One selected source result, separate from application status.
    pub receipt: Option<Receipt>,
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
                let previews = response(client.previews().await)?;
                let receipts = response(client.receipts().await)?;
                let mut state = self.state.lock().expect("Profile source page");
                state.leaves.catalog = Some(catalog);
                state.leaves.previews = previews;
                state.leaves.receipts = receipts;
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
            LeafCommand::Commit { ticket, digest } => {
                self.leaf_commit(client, &ticket, &digest).await?;
            }
            LeafCommand::Receipt { ticket } => {
                let receipt = response(client.receipt(&ticket).await)?;
                self.leaf_receipt(receipt);
            }
            LeafCommand::Discard { ticket } => {
                response(client.discard(&ticket).await)?;
                let mut state = self.state.lock().expect("Profile proposal discard");
                state
                    .leaves
                    .previews
                    .retain(|preview| preview.ticket != ticket);
                state.leaves.notice = Some("Unused proposal discarded".into());
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
        let preview = response(client.preview(request).await)?;
        let mut state = self.state.lock().expect("Profile prepared proposal");
        if state.leaves.previews.len() == 4 {
            state.leaves.previews.remove(0);
        }
        state.leaves.previews.push(preview);
        state.leaves.notice = Some("Prepared. Review this exact proposal before saving.".into());
        Ok(())
    }
    async fn leaf_commit(&self, client: &wire::Client, ticket: &str, digest: &str) -> Result<()> {
        let preview = {
            let mut state = self.state.lock().expect("Profile review acceptance");
            if state.leaves.receipt.as_ref().is_some_and(|receipt| {
                receipt.preview.ticket == ticket
                    && matches!(receipt.outcome, Outcome::Pending | Outcome::Unknown)
            }) {
                return Err("Query the original receipt before any further write".into());
            }
            let preview = state
                .leaves
                .previews
                .iter()
                .find(|preview| preview.ticket == ticket && preview.digest == digest)
                .cloned()
                .ok_or("Prepared proposal changed; refresh Host Profiles")?;
            state.leaves.receipt = Some(Receipt {
                preview: preview.clone(),
                outcome: Outcome::Pending,
            });
            preview
        };
        let result = client.commit(&preview).await;
        match result {
            Ok(Ok(receipt)) => {
                self.state
                    .lock()
                    .expect("Committed Profile proposal")
                    .leaves
                    .previews
                    .retain(|item| item.ticket != ticket);
                self.leaf_receipt(receipt);
                Ok(())
            }
            Ok(Err(failure)) => {
                // The owner proved rejection before write admission; the proposal remains reusable.
                self.state
                    .lock()
                    .expect("Rejected Profile proposal")
                    .leaves
                    .receipt = None;
                Err(failure_message(&failure))
            }
            Err(_) => {
                self.leaf_receipt(Receipt {
                    preview,
                    outcome: Outcome::Unknown,
                });
                Err("Source write outcome unknown. Query this original receipt; do not submit a new write.".into())
            }
        }
    }
    fn leaf_receipt(&self, receipt: Receipt) {
        let mut state = self.state.lock().expect("Profile receipt");
        if matches!(
            receipt.outcome,
            Outcome::Saved { .. } | Outcome::Failed { .. }
        ) {
            state
                .leaves
                .previews
                .retain(|preview| preview.ticket != receipt.preview.ticket);
        }
        if !state.leaves.receipts.contains(&receipt.preview.ticket)
            && state.leaves.receipts.len() < 256
        {
            state.leaves.receipts.push(receipt.preview.ticket.clone());
        }
        state.leaves.notice = Some(match &receipt.outcome {
            Outcome::Pending => "The original source operation is still running".into(),
            Outcome::Unknown => "Source outcome unknown. Query the original receipt.".into(),
            Outcome::Failed { failure } => failure_message(failure),
            Outcome::Saved { .. } => {
                "Source saved. Runtime application is reported separately.".into()
            }
        });
        state.leaves.receipt = Some(receipt);
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
            "Operation outcome unknown. Refresh its original grant or receipt; do not replay it."
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
