//! Durable SSH stdio inputs and exact grant-held management operations.
use rsi_api_protocol::{ApiError, CallOrigin, Result, json_handler};
use rsi_configuration_api::mcp_ssh as wire;
use rsi_mcp::{McpConfig, McpOwner, ServerConfig, TransportConfig};
use rsi_storage_domain::{Domain, DomainSpec};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::{Semaphore, oneshot};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
type Reply = Result<std::result::Result<wire::State, wire::Failure>>;
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    revision: u64,
    config: McpConfig,
}
struct State {
    closed: bool,
    document: Document,
}
pub(crate) struct Owner {
    domain: Arc<dyn Domain>,
    state: Mutex<State>,
    mcp: Arc<McpOwner>,
    grants: Arc<crate::profile_management::Manager>,
    resolver: Arc<dyn rsi_execution::ExecutionResolver>,
    epoch: rsi_api_protocol::HostEpoch,
    tasks: TaskTracker,
    slots: Arc<Semaphore>,
    writer: Semaphore,
}
impl Owner {
    fn snapshot(&self) -> Result<Document> {
        self.domain
            .ensure_available()
            .map_err(rsi_storage_domain::storage_error)?;
        let state = self.state.lock().expect("MCP SSH state");
        if state.closed {
            return Err(ApiError::ShuttingDown);
        }
        Ok(state.document.clone())
    }
    fn authorize(
        &self,
        origin: &CallOrigin,
        target: &wire::Target,
        config: Option<&ServerConfig>,
    ) -> Result<tokio_util::task::task_tracker::TaskTrackerToken> {
        target.validate(&self.epoch)?;
        if config.is_some_and(|config| !matches!(&config.transport, TransportConfig::SshStdio { target: actual, .. } if actual == &target.target)) { return Err(ApiError::Unauthorized); }
        self.grants.admit_ssh_stdio(
            origin,
            &target.target,
            &target.server,
            &config.map_or_else(Default::default, ServerConfig::ssh_credentials),
        )
    }
    fn get(&self, origin: &CallOrigin, target: wire::Target) -> Reply {
        let _access = self.authorize(origin, &target, None)?;
        let document = self.snapshot()?;
        let config = document
            .config
            .servers
            .into_iter()
            .find(|config| config.id == target.server);
        let _references = self.authorize(origin, &target, config.as_ref())?;
        Ok(Ok(wire::State {
            target,
            revision: document.revision.to_string(),
            config,
            apply_error: None,
        }))
    }
    async fn change(
        &self,
        origin: CallOrigin,
        operation: wire::Operation,
        input: wire::Change,
    ) -> Reply {
        input.validate(&self.epoch, operation == wire::Operation::Put)?;
        let _access = self.authorize(&origin, &input.target, input.config.as_ref())?;
        let _writer = self.writer.try_acquire().map_err(|_| ApiError::Capacity)?;
        let mut document = self.snapshot()?;
        if input.expected != document.revision.to_string() {
            return Ok(Err(wire::Failure::Conflict));
        }
        let previous = document
            .config
            .servers
            .iter()
            .find(|config| config.id == input.target.server)
            .cloned();
        let _previous = self.authorize(&origin, &input.target, previous.as_ref())?;
        if operation != wire::Operation::Put && previous.is_none() {
            return Ok(Err(wire::Failure::NotFound));
        }
        let apply_error = if operation == wire::Operation::Refresh {
            let lease = self.resolver.lease(
                origin,
                &rsi_execution::ExecutionLocation::Ssh {
                    target: input.target.target.clone(),
                },
            )?;
            match self.mcp.set_remote_stdio(document.config.clone()).await {
                Err(error) => Some(error),
                Ok(()) => self
                    .mcp
                    .refresh_ssh(&input.target.server, lease, CancellationToken::new())
                    .await
                    .err(),
            }
        } else {
            document
                .config
                .servers
                .retain(|config| config.id != input.target.server);
            if let Some(config) = input.config {
                document.config.servers.push(config);
            }
            document.config.servers.sort_by(|a, b| a.id.cmp(&b.id));
            if self.mcp.validate_remote_stdio(&document.config).is_err() {
                return Ok(Err(wire::Failure::Configuration));
            }
            document.revision = document.revision.checked_add(1).ok_or(ApiError::Capacity)?;
            let encoded = serde_json::to_value(&document).map_err(|_| ApiError::Capacity)?;
            self.domain
                .put("configuration", encoded)
                .await
                .map_err(rsi_storage_domain::storage_error)?;
            self.state.lock().expect("MCP SSH publication").document = document.clone();
            self.mcp
                .set_remote_stdio(document.config.clone())
                .await
                .err()
        };
        let config = document
            .config
            .servers
            .into_iter()
            .find(|config| config.id == input.target.server);
        Ok(Ok(wire::State {
            target: input.target,
            revision: document.revision.to_string(),
            config,
            apply_error,
        }))
    }
    async fn run<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(Arc<Self>) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>,
    ) -> Result<T> {
        self.domain
            .ensure_available()
            .map_err(rsi_storage_domain::storage_error)?;
        let (send, receive) = oneshot::channel();
        {
            let state = self.state.lock().expect("MCP SSH admission");
            if state.closed {
                return Err(ApiError::ShuttingDown);
            }
            let permit = self
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| ApiError::Capacity)?;
            let future = work(self.clone());
            self.tasks.spawn(async move {
                let _permit = permit;
                let _ = send.send(future.await);
            });
        }
        receive.await.map_err(|_| ApiError::OutcomeUnknown)
    }
    pub(crate) async fn close(&self) {
        {
            let mut state = self.state.lock().expect("MCP SSH retirement");
            state.closed = true;
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
}
pub(crate) async fn register(
    mcp: Arc<McpOwner>,
    facility: Arc<dyn rsi_storage_domain::DomainFacility>,
    grants: Arc<crate::profile_management::Manager>,
    resolver: Arc<dyn rsi_execution::ExecutionResolver>,
    epoch: rsi_api_protocol::HostEpoch,
    registrar: Arc<dyn rsi_api_protocol::ApiRegistrar>,
) -> rsi_meta::Result<(Arc<Owner>, Vec<rsi_api_protocol::ApiRegistration>)> {
    let domain = facility
        .open(DomainSpec {
            id: "rsi.mcp-ssh".into(),
            backend: "base".into(),
            version: 1,
            maximum_records: 1,
            maximum_bytes: 256 * 1024 + 128,
        })
        .await
        .map_err(meta)?;
    let mut snapshot = domain.snapshot().await.map_err(meta)?;
    let document: Document = snapshot
        .remove("configuration")
        .map_or(Ok(Document::default()), serde_json::from_value)
        .map_err(meta)?;
    if !snapshot.is_empty() {
        return Err(meta("Invalid MCP SSH storage record"));
    }
    mcp.validate_remote_stdio(&document.config).map_err(meta)?;
    mcp.set_remote_stdio(document.config.clone())
        .await
        .map_err(meta)?;
    let owner = Arc::new(Owner {
        domain,
        state: Mutex::new(State {
            closed: false,
            document,
        }),
        mcp,
        grants,
        resolver,
        epoch,
        tasks: TaskTracker::new(),
        slots: Arc::new(Semaphore::new(4)),
        writer: Semaphore::new(1),
    });
    let selected = owner.clone();
    let mut registrations = vec![
        registrar
            .register(
                wire::Operation::Get.spec(),
                json_handler(move |context, target: wire::Target| {
                    let owner = selected.clone();
                    async move {
                        owner
                            .run(move |owner| {
                                Box::pin(async move { owner.get(&context.origin, target) })
                            })
                            .await?
                    }
                }),
            )
            .map_err(meta)?,
    ];
    for operation in [
        wire::Operation::Put,
        wire::Operation::Remove,
        wire::Operation::Refresh,
    ] {
        let selected = owner.clone();
        registrations.push(
            registrar
                .register(
                    operation.spec(),
                    json_handler(move |context, input: wire::Change| {
                        let owner = selected.clone();
                        async move {
                            owner
                                .run(move |owner| {
                                    Box::pin(async move {
                                        owner.change(context.origin, operation, input).await
                                    })
                                })
                                .await?
                        }
                    }),
                )
                .map_err(meta)?,
        );
    }
    Ok((owner, registrations))
}
fn meta(error: impl std::fmt::Display) -> rsi_meta::MetaError {
    rsi_meta::MetaError::Activation(error.to_string())
}
