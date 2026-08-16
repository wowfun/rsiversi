use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use rsi_meta::{
    ApplyRequest, ApplyResult, CompositionHost, CompositionProject, CompositionWorkspace,
    InstallRequest, InstallResult, LockResult, OpenOptions, OperationId,
};

use crate::auth::ensure_private_directory;
use crate::cli::{HostOpenRequest, HostOpener, OpenedHost};
use crate::host::{BoxHostServiceStream, HostApi, HostEventStream};
use crate::protocol::{
    Command, CommandEnvelope, CommandOutcome, CommandOutcomeEnvelope, EventEnvelope, GraphRevision,
    ServiceOpenRequest,
};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CompositionHostOpener;

#[derive(Clone)]
struct CompositionHostAdapter {
    host: CompositionHost,
}

impl fmt::Debug for CompositionHostAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompositionHostAdapter")
            .field("graph_revision", &self.host.snapshot().graph.revision)
            .finish_non_exhaustive()
    }
}

pub(crate) fn workspace(state_dir: &std::path::Path) -> CompositionWorkspace {
    CompositionWorkspace {
        database_path: state_dir.join("state.sqlite3"),
        cache_root: state_dir.join("cache"),
        manifest_path: state_dir.join("composition.toml"),
        lock_path: state_dir.join("rsi-meta.lock"),
    }
}

#[async_trait]
impl HostOpener for CompositionHostOpener {
    async fn open(&self, request: HostOpenRequest) -> Result<OpenedHost> {
        ensure_private_directory(&request.state_dir)?;
        let host = CompositionHost::open(OpenOptions::new(workspace(&request.state_dir)))
            .await
            .context("open composition host")?;
        Ok(OpenedHost {
            host: Arc::new(CompositionHostAdapter { host }),
        })
    }

    async fn validate(&self, project: CompositionProject) -> Result<rsi_meta::ValidationReport> {
        project.validate().context("validate composition project")
    }

    async fn lock(&self, project: CompositionProject) -> Result<LockResult> {
        project.lock().context("lock composition project")
    }

    async fn install(&self, request: InstallRequest) -> Result<InstallResult> {
        if let Some(state_dir) = request.workspace.database_path.parent() {
            ensure_private_directory(state_dir)?;
        }
        CompositionHost::install_offline(request)
            .await
            .context("install composition project")
    }
}

#[async_trait]
impl HostApi for CompositionHostAdapter {
    async fn submit(&self, command: CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
        let command_id = command.command_id.clone();
        let result = match command.payload {
            Command::ApplyManifestPath {
                manifest_path,
                lock_path,
            } => match self
                .host
                .apply(ApplyRequest {
                    operation_id: OperationId(command_id.clone()),
                    project: CompositionProject {
                        manifest_path,
                        lock_path: Some(lock_path),
                    },
                    expected_revision: command.expected_graph_revision,
                })
                .await?
            {
                ApplyResult::Applied { snapshot } => CommandOutcome::Applied {
                    graph: snapshot.graph,
                },
                ApplyResult::Unchanged { snapshot } => CommandOutcome::NoChange {
                    graph: snapshot.graph,
                },
                ApplyResult::RestartRequired {
                    current,
                    candidate,
                    packages,
                } => CommandOutcome::RestartRequired {
                    current,
                    candidate,
                    packages,
                },
            },
            Command::QueryGraph => {
                let snapshot = self.host.snapshot();
                CommandOutcome::Graph {
                    graph: snapshot.graph,
                    cursor: snapshot.cursor,
                }
            }
            Command::QueryEvents {
                after_cursor,
                limit,
            } => CommandOutcome::Events {
                events: self
                    .host
                    .events_after(after_cursor, limit)
                    .await?
                    .events
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            },
            Command::InspectPlugin { instance_id } => CommandOutcome::Plugin {
                instance: self.host.inspect_plugin(instance_id).await?,
            },
            Command::RotateToken => CommandOutcome::TokenRotated {
                generation: self
                    .host
                    .rotate_token(OperationId(command_id.clone()))
                    .await?
                    .generation,
            },
            Command::Shutdown => {
                self.host
                    .request_shutdown(OperationId(command_id.clone()))
                    .await?;
                CommandOutcome::ShuttingDown
            }
            Command::Unknown { command_type, .. } => CommandOutcome::Rejected {
                code: "unsupported_command".to_owned(),
                message: format!("command type {command_type:?} is not supported"),
                details: std::collections::BTreeMap::new(),
            },
        };
        Ok(CommandOutcomeEnvelope::new(
            command_id,
            self.host.snapshot().graph.revision,
            result,
        ))
    }

    async fn subscribe(&self, after_cursor: u64) -> Result<HostEventStream> {
        let stream = self
            .host
            .subscribe(after_cursor)
            .await
            .context("subscribe to composition events")?
            .map(|event| event.map(EventEnvelope::from).map_err(anyhow::Error::new));
        Ok(Box::pin(stream))
    }

    fn graph_revision(&self) -> GraphRevision {
        self.host.snapshot().graph.revision
    }

    fn token_generation(&self) -> u64 {
        self.host.snapshot().token_generation
    }

    fn open_service(&self, request: ServiceOpenRequest) -> Result<BoxHostServiceStream> {
        self.host
            .open_service(request)
            .map(|stream| Box::new(stream) as BoxHostServiceStream)
            .context("open routed service stream")
    }

    async fn shutdown(&self) -> Result<()> {
        self.host
            .request_shutdown(OperationId(uuid::Uuid::now_v7().to_string()))
            .await
            .context("request composition host shutdown")?;
        self.host
            .wait_terminated(Instant::now() + SHUTDOWN_TIMEOUT)
            .await
            .context("wait for composition host shutdown")
    }

    async fn wait_terminated(&self) -> Result<()> {
        self.host
            .wait_terminated(Instant::now() + SHUTDOWN_TIMEOUT)
            .await
            .context("wait for composition host shutdown")
    }

    async fn monitor_terminated(&self) -> Result<()> {
        loop {
            match self
                .host
                .wait_terminated(Instant::now() + SHUTDOWN_TIMEOUT)
                .await
            {
                Err(rsi_meta::HostError::ShutdownDeadline) => {}
                result => return result.context("observe composition host termination"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[tokio::test]
    async fn validate_and_lock_do_not_open_a_host() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("rsi-meta.toml");
        let lock = directory.path().join("rsi-meta.lock");
        fs::write(
            &manifest,
            "format_version = 0\nscopes = []\ninstances = []\n\n[composition]\nid = \"offline-test\"\n",
        )
        .unwrap();
        let project = CompositionProject {
            manifest_path: manifest,
            lock_path: Some(lock.clone()),
        };
        assert!(matches!(
            project.lock().unwrap(),
            LockResult::Created { .. }
        ));
        assert!(project.validate().unwrap().is_valid());
        assert!(matches!(
            project.lock().unwrap(),
            LockResult::Unchanged { .. }
        ));
    }
}
