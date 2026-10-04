//! Product-owned SSH candidates, Local trust and live delegated execution admission.
mod api;
mod artifact;
mod identity;
mod plugin;
use futures_util::future::BoxFuture;
pub(crate) use plugin::register;
use rsi_api_protocol::{ApiError, CallOrigin, HostEpoch};
use rsi_configuration_api::{
    leaf::{GrantScope, Principal},
    ssh as wire,
};
use rsi_execution::{
    ExecutionAdmission, ExecutionLease, ExecutionOperation, ExecutionProvider, ExecutionTargetId,
};
use rsi_meta::{Context, LocalContract};
use rsi_ssh_client::ConnectedSsh;
use rsi_ssh_protocol::{
    SshHostKey,
    initialization::{Initialization, ProgramPolicy},
};
use rsi_storage_domain::Domain;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
use tokio_util::task::TaskTracker;

type Reply<T> = wire::Reply<T>;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Trust {
    host_key: SshHostKey,
    identity_path: String,
    identity_sha256: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    candidate: wire::Candidate,
    revision: u64,
    creator: Principal,
    trust: Option<Trust>,
}
impl Record {
    fn validate(&self) -> rsi_api_protocol::Result<()> {
        self.candidate.validate()?;
        if self.revision == 0 || matches!(self.creator, Principal::Agent(_)) {
            return Err(ApiError::Unavailable);
        }
        if let Some(trust) = &self.trust
            && (!trust.identity_path.starts_with('/')
                || trust.identity_path.len() > 16 * 1024
                || trust.identity_path.chars().any(char::is_control)
                || trust.identity_sha256.len() != 64
                || !trust
                    .identity_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(())
    }
}
struct Live {
    connection: Arc<ConnectedSsh>,
    provider: ExecutionProvider,
}
struct State {
    closed: bool,
    records: BTreeMap<ExecutionTargetId, Record>,
    connections: BTreeMap<ExecutionTargetId, Live>,
    next_epoch: u64,
}
#[derive(Debug)]
pub(crate) struct Contract;
impl LocalContract for Contract {
    const KEY: &'static str = "rsi.ssh-targets";
    type Service = Manager;
}
pub(crate) struct Manager {
    context: Context,
    domain: Arc<dyn Domain>,
    grants: Arc<crate::profile_management::Manager>,
    configuration: Arc<rsi_configuration_access::ConfigurationAccess>,
    epoch: HostEpoch,
    service: String,
    artifact: tokio::sync::OnceCell<rsi_ssh_client::HelperArtifact>,
    state: Mutex<State>,
    writer: Arc<Semaphore>,
    target_writers: TargetWriters,
    slots: Arc<Semaphore>,
    effects: Arc<Semaphore>,
    operations: Arc<Semaphore>,
    tasks: TaskTracker,
    stop: tokio_util::sync::CancellationToken,
}
impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshTargetManager")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}
fn principal(origin: &CallOrigin) -> rsi_api_protocol::Result<Principal> {
    match origin {
        CallOrigin::Local => Ok(Principal::Local),
        CallOrigin::Device(device) if !device.revoked.is_cancelled() => {
            Ok(Principal::Device(device.id.clone()))
        }
        CallOrigin::Device(_) => Err(ApiError::Unauthorized),
    }
}
#[derive(Default)]
struct TargetWriters(Mutex<std::collections::BTreeSet<ExecutionTargetId>>);
struct TargetWriter<'a> {
    owner: &'a TargetWriters,
    target: ExecutionTargetId,
}
impl TargetWriters {
    fn admit(&self, target: &ExecutionTargetId) -> Option<TargetWriter<'_>> {
        self.0
            .lock()
            .expect("target writers")
            .insert(target.clone())
            .then(|| TargetWriter {
                owner: self,
                target: target.clone(),
            })
    }
}
impl Drop for TargetWriter<'_> {
    fn drop(&mut self) {
        self.owner
            .0
            .lock()
            .expect("target writers")
            .remove(&self.target);
    }
}
impl Manager {
    fn available(&self) -> rsi_api_protocol::Result<()> {
        self.domain
            .ensure_available()
            .map_err(rsi_storage_domain::storage_error)?;
        if self.state.lock().expect("SSH state").closed {
            return Err(ApiError::ShuttingDown);
        }
        Ok(())
    }
    fn run<T: Send + 'static>(
        self: &Arc<Self>,
        origin: CallOrigin,
        work: impl FnOnce(Arc<Self>, CallOrigin) -> BoxFuture<'static, Reply<T>>,
    ) -> rsi_api_protocol::Result<BoxFuture<'static, Reply<T>>> {
        self.domain
            .ensure_available()
            .map_err(rsi_storage_domain::storage_error)?;
        principal(&origin)?;
        let state = self.state.lock().expect("SSH admission");
        if state.closed {
            return Err(ApiError::ShuttingDown);
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let future = work(self.clone(), origin);
        let task = self
            .context
            .runtime()
            .execution()
            .spawn(self.tasks.track_future(async move {
                let _permit = permit;
                future.await
            }));
        drop(state);
        Ok(Box::pin(async move {
            task.await.map_err(|_| ApiError::OutcomeUnknown)?
        }))
    }
    fn permissions(
        &self,
        origin: &CallOrigin,
        target: &ExecutionTargetId,
    ) -> rsi_api_protocol::Result<wire::Permissions> {
        let permitted = |scope| match self.grants.admit_execution_scope(origin, scope) {
            Ok(_) => Ok(true),
            Err(ApiError::Unauthorized) => Ok(false),
            Err(error) => Err(error),
        };
        Ok(wire::Permissions {
            use_target: permitted(GrantScope::SshUse {
                target: target.clone(),
            })?,
            manage: permitted(GrantScope::SshManage {
                target: target.clone(),
            })?,
        })
    }
    fn project(
        &self,
        origin: &CallOrigin,
        record: &Record,
        connection: Option<&ConnectedSsh>,
    ) -> rsi_api_protocol::Result<wire::Target> {
        Ok(wire::Target {
            candidate: record.candidate.clone(),
            revision: record.revision.to_string(),
            fingerprint: record
                .trust
                .as_ref()
                .map(|trust| trust.host_key.fingerprint()),
            connection_epoch: connection.map(|live| live.client().epoch().to_string()),
            connected: connection.is_some_and(|live| !live.client().is_closed()),
            unavailable_programs: connection.map_or_else(Vec::new, |live| {
                let mut values = live.unavailable().to_vec();
                values.sort();
                values
            }),
            permissions: self.permissions(origin, &record.candidate.target)?,
        })
    }
    fn catalog(&self, origin: &CallOrigin) -> Reply<wire::Catalog> {
        self.available()?;
        let principal = principal(origin)?;
        let snapshot = {
            let state = self.state.lock().expect("SSH catalog");
            state
                .records
                .values()
                .map(|record| {
                    (
                        record.clone(),
                        state
                            .connections
                            .get(&record.candidate.target)
                            .map(|live| live.connection.clone()),
                    )
                })
                .collect::<Vec<_>>()
        };
        let mut targets = vec![];
        for (record, connection) in snapshot {
            let target = self.project(origin, &record, connection.as_deref())?;
            if principal == Principal::Local
                || record.creator == principal
                || target.permissions.use_target
                || target.permissions.manage
            {
                targets.push(target);
            }
        }
        Ok(Ok(wire::Catalog {
            host_epoch: self.epoch.clone(),
            targets,
        }))
    }
    fn selected(&self, selection: &wire::Selection) -> Reply<Record> {
        selection.validate(&self.epoch)?;
        let state = self.state.lock().expect("SSH selection");
        Ok(state
            .records
            .get(&selection.target)
            .filter(|record| record.revision.to_string() == selection.revision)
            .cloned()
            .ok_or(wire::Failure::Conflict {}))
    }
    async fn publish(&self, origin: &CallOrigin, record: Record) -> Reply<wire::Target> {
        record.validate()?;
        let value = serde_json::to_value(&record).map_err(|_| ApiError::Unavailable)?;
        self.domain
            .put(record.candidate.target.as_str(), value)
            .await
            .map_err(rsi_storage_domain::storage_error)?;
        let retired = {
            let mut state = self.state.lock().expect("SSH publication");
            let retired = state.connections.remove(&record.candidate.target);
            state
                .records
                .insert(record.candidate.target.clone(), record.clone());
            retired
        };
        // Previously admitted leases retain the old provider; this owner issues no more of them.
        drop(retired);
        Ok(Ok(self.project(origin, &record, None)?))
    }
    async fn put(&self, origin: CallOrigin, input: wire::PutCandidate) -> Reply<wire::Target> {
        input.validate(&self.epoch)?;
        self.available()?;
        let creator = principal(&origin)?;
        let expected = input
            .expected
            .parse::<u64>()
            .map_err(|_| ApiError::Unavailable)?;
        let (_configuration, _management) = if expected == 0 {
            (Some(self.configuration.admit(&origin)?), None)
        } else {
            (
                None,
                Some(self.grants.admit_execution_scope(
                    &origin,
                    GrantScope::SshManage {
                        target: input.candidate.target.clone(),
                    },
                )?),
            )
        };
        let Some(_target_writer) = self.target_writers.admit(&input.candidate.target) else {
            return Ok(Err(wire::Failure::Busy {}));
        };
        let Ok(_writer) = self.writer.try_acquire() else {
            return Ok(Err(wire::Failure::Busy {}));
        };
        let creator = if expected == 0 {
            let state = self.state.lock().expect("SSH candidate capacity");
            if state.records.contains_key(&input.candidate.target) {
                return Ok(Err(wire::Failure::Conflict {}));
            }
            if state.records.len() >= 64
                || state
                    .records
                    .values()
                    .filter(|record| record.creator == creator && record.trust.is_none())
                    .count()
                    >= 4
            {
                return Err(ApiError::Capacity);
            }
            creator
        } else {
            let state = self.state.lock().expect("SSH candidate CAS");
            let Some(record) = state
                .records
                .get(&input.candidate.target)
                .filter(|record| record.revision == expected)
            else {
                return Ok(Err(wire::Failure::Conflict {}));
            };
            record.creator.clone()
        };
        self.publish(
            &origin,
            Record {
                candidate: input.candidate,
                revision: expected.checked_add(1).ok_or(ApiError::Capacity)?,
                creator,
                trust: None,
            },
        )
        .await
    }
    async fn trust(&self, origin: CallOrigin, input: wire::ConfirmTrust) -> Reply<wire::Target> {
        if !matches!(origin, CallOrigin::Local) {
            return Err(ApiError::Unauthorized);
        }
        input.validate(&self.epoch)?;
        self.available()?;
        let Some(_target_writer) = self.target_writers.admit(&input.selection.target) else {
            return Ok(Err(wire::Failure::Busy {}));
        };
        let mut record = match self.selected(&input.selection)? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        let Ok(identity) = identity::read(input.identity_path).await else {
            return Ok(Err(wire::Failure::IdentityUnavailable {}));
        };
        record.revision = record.revision.checked_add(1).ok_or(ApiError::Capacity)?;
        record.trust = Some(Trust {
            host_key: input.host_key,
            identity_path: identity.path,
            identity_sha256: identity.digest,
        });
        let Ok(_writer) = self.writer.try_acquire() else {
            return Ok(Err(wire::Failure::Busy {}));
        };
        self.publish(&origin, record).await
    }
    fn connection_matches(&self, request: &wire::ConnectionRequest) -> bool {
        self.state
            .lock()
            .expect("SSH epoch CAS")
            .connections
            .get(&request.selection.target)
            .map(|live| live.connection.client().epoch().to_string())
            == request.expected_connection_epoch
    }
    async fn connect(
        &self,
        origin: CallOrigin,
        input: wire::ConnectionRequest,
    ) -> Reply<wire::Target> {
        input.validate(&self.epoch)?;
        self.available()?;
        let _admission = self.grants.admit_execution_scope(
            &origin,
            GrantScope::SshUse {
                target: input.selection.target.clone(),
            },
        )?;
        let Some(_target_writer) = self.target_writers.admit(&input.selection.target) else {
            return Ok(Err(wire::Failure::Busy {}));
        };
        let record = match self.selected(&input.selection)? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        if !self.connection_matches(&input) {
            return Ok(Err(wire::Failure::Conflict {}));
        }
        let Some(trust) = &record.trust else {
            return Ok(Err(wire::Failure::TrustRequired {}));
        };
        let identity = match identity::read(trust.identity_path.clone()).await {
            Ok(value) if value.digest == trust.identity_sha256 => value,
            _ => return Ok(Err(wire::Failure::IdentityUnavailable {})),
        };
        let prepared = rsi_ssh_client::PreparedSsh::with_identity(
            &record.candidate.endpoint,
            &trust.host_key,
            &identity.bytes,
        )
        .map_err(|_| ApiError::Unavailable)?;
        drop(identity);
        let Ok(artifact) = self.artifact.get_or_try_init(artifact::installed).await else {
            return Ok(Err(wire::Failure::HelperUnavailable {}));
        };
        let (epoch, previous) = {
            let mut state = self.state.lock().expect("SSH connection reservation");
            let epoch = state.next_epoch;
            state.next_epoch = epoch.checked_add(1).ok_or(ApiError::Capacity)?;
            (epoch, state.connections.remove(&record.candidate.target))
        };
        if let Some(previous) = previous {
            previous
                .connection
                .shutdown()
                .await
                .map_err(|_| ApiError::OutcomeUnknown)?;
        }
        let connected = match rsi_ssh_client::connect(
            prepared,
            std::path::Path::new("/usr/bin/ssh"),
            artifact.clone(),
            &self.service,
            epoch,
            target_programs(),
        )
        .await
        {
            Ok(value) => Arc::new(value),
            Err(rsi_ssh_client::SshClientError::CacheContentionTimeout) => {
                return Ok(Err(wire::Failure::CacheContentionTimeout {}));
            }
            Err(_) => return Ok(Err(wire::Failure::ConnectionFailed {})),
        };
        let provider = rsi_ssh_client::execution_provider(
            self.epoch.clone(),
            connected.client(),
            record.candidate.target.clone(),
            record.revision,
        )
        .map_err(|_| ApiError::Unavailable)?;
        self.domain
            .ensure_available()
            .map_err(rsi_storage_domain::storage_error)?;
        let mut state = self.state.lock().expect("SSH connection publication");
        state.connections.insert(
            record.candidate.target.clone(),
            Live {
                connection: connected.clone(),
                provider,
            },
        );
        drop(state);
        Ok(Ok(self.project(&origin, &record, Some(&connected))?))
    }
    async fn disconnect(
        &self,
        origin: CallOrigin,
        input: wire::ConnectionRequest,
    ) -> Reply<wire::Target> {
        input.validate(&self.epoch)?;
        self.available()?;
        let _admission = self.grants.admit_execution_scope(
            &origin,
            GrantScope::SshManage {
                target: input.selection.target.clone(),
            },
        )?;
        let Some(_target_writer) = self.target_writers.admit(&input.selection.target) else {
            return Ok(Err(wire::Failure::Busy {}));
        };
        let record = match self.selected(&input.selection)? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        if !self.connection_matches(&input) {
            return Ok(Err(wire::Failure::Conflict {}));
        }
        let previous = self
            .state
            .lock()
            .expect("SSH disconnect")
            .connections
            .remove(&record.candidate.target);
        if let Some(previous) = previous {
            previous
                .connection
                .shutdown()
                .await
                .map_err(|_| ApiError::OutcomeUnknown)?;
        }
        Ok(Ok(self.project(&origin, &record, None)?))
    }
    async fn resolve_directory(
        &self,
        origin: CallOrigin,
        input: wire::ResolveDirectory,
    ) -> Reply<rsi_execution::ExecutionCoordinates> {
        input.validate(&self.epoch)?;
        self.available()?;
        let _admission = self.grants.admit_execution_scope(
            &origin,
            GrantScope::SshUse {
                target: input.connection.selection.target.clone(),
            },
        )?;
        let lease = {
            let Some(_target_writer) = self
                .target_writers
                .admit(&input.connection.selection.target)
            else {
                return Ok(Err(wire::Failure::Busy {}));
            };
            if let Err(error) = self.selected(&input.connection.selection)? {
                return Ok(Err(error));
            }
            if !self.connection_matches(&input.connection) {
                return Ok(Err(wire::Failure::Conflict {}));
            }
            self.lease(origin, &input.connection.selection.target)?
        };
        Ok(Ok(lease
            .canonicalize(&input.path)
            .await
            .map_err(execution_error)?))
    }
    pub(crate) fn admit(
        &self,
        origin: &CallOrigin,
        target: &ExecutionTargetId,
    ) -> rsi_api_protocol::Result<ExecutionOperation> {
        self.available()?;
        UseAdmission {
            origin: origin.clone(),
            target: target.clone(),
            grants: self.grants.clone(),
            domain: self.domain.clone(),
            effects: self.effects.clone(),
            operations: self.operations.clone(),
            stop: self.stop.clone(),
        }
        .admit(rsi_execution::ExecutionAdmissionKind::Scope)
        .map_err(execution_error)
    }
    pub(crate) fn visibility(
        &self,
        origin: &CallOrigin,
    ) -> rsi_api_protocol::Result<rsi_execution::ExecutionVisibility> {
        self.available()?;
        let permit = self
            .effects
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let visibility = self.grants.execution_visibility(origin)?;
        Ok(rsi_execution::ExecutionVisibility::new(
            visibility.locations().clone(),
            ExecutionOperation::new((permit, visibility)),
        ))
    }
    pub(crate) fn lease(
        &self,
        origin: CallOrigin,
        target: &ExecutionTargetId,
    ) -> rsi_api_protocol::Result<ExecutionLease> {
        self.available()?;
        let admission = Arc::new(UseAdmission {
            origin,
            target: target.clone(),
            grants: self.grants.clone(),
            domain: self.domain.clone(),
            effects: self.effects.clone(),
            operations: self.operations.clone(),
            stop: self.stop.clone(),
        });
        let _admitted = admission
            .admit(rsi_execution::ExecutionAdmissionKind::Publication)
            .map_err(execution_error)?;
        let state = self.state.lock().expect("SSH provider selection");
        state
            .connections
            .get(target)
            .ok_or(ApiError::Unavailable)?
            .provider
            .lease(admission)
            .map_err(execution_error)
    }
    async fn close(&self) {
        {
            let mut state = self.state.lock().expect("SSH retirement");
            state.closed = true;
            self.stop.cancel();
            self.tasks.close();
        }
        self.tasks.wait().await;
        let connections = std::mem::take(
            &mut self
                .state
                .lock()
                .expect("SSH connection retirement")
                .connections,
        );
        for (_, live) in connections {
            let _ = live.connection.shutdown().await;
        }
    }
}
#[derive(Debug)]
struct UseAdmission {
    stop: tokio_util::sync::CancellationToken,
    origin: CallOrigin,
    target: ExecutionTargetId,
    grants: Arc<crate::profile_management::Manager>,
    domain: Arc<dyn Domain>,
    effects: Arc<Semaphore>,
    operations: Arc<Semaphore>,
}
impl ExecutionAdmission for UseAdmission {
    fn admit(
        &self,
        kind: rsi_execution::ExecutionAdmissionKind,
    ) -> rsi_process::Result<ExecutionOperation> {
        if self.stop.is_cancelled() {
            return Err(rsi_process::ProcessError::ShuttingDown);
        }
        self.domain.ensure_available().map_err(|error| {
            rsi_process::ProcessError::Api(rsi_storage_domain::storage_error(error))
        })?;
        let grant = self
            .grants
            .admit_execution_scope(
                &self.origin,
                GrantScope::SshUse {
                    target: self.target.clone(),
                },
            )
            .map_err(rsi_process::ProcessError::Api)?;
        let slots = match kind {
            rsi_execution::ExecutionAdmissionKind::Scope => &self.effects,
            rsi_execution::ExecutionAdmissionKind::Operation => &self.operations,
            rsi_execution::ExecutionAdmissionKind::Publication => {
                return Ok(ExecutionOperation::new(grant));
            }
        };
        let permit = slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| rsi_process::ProcessError::Capacity)?;
        Ok(ExecutionOperation::new((permit, grant)))
    }
}

fn execution_error(error: rsi_process::ProcessError) -> ApiError {
    match error {
        rsi_process::ProcessError::Api(error) => error,
        rsi_process::ProcessError::Capacity => ApiError::Capacity,
        rsi_process::ProcessError::ShuttingDown => ApiError::ShuttingDown,
        rsi_process::ProcessError::OutcomeUnknown => ApiError::OutcomeUnknown,
        _ => ApiError::Unavailable,
    }
}

fn target_programs() -> Initialization {
    let mut programs: BTreeMap<_, _> = ["bash", "git", "node"]
        .into_iter()
        .map(|selector| {
            (
                selector.into(),
                ProgramPolicy {
                    command: selector.into(),
                    environment: vec![],
                },
            )
        })
        .collect();
    programs.insert(
        "review_git".into(),
        ProgramPolicy {
            command: "git".into(),
            environment: rsi_workspace_review::SOURCE_GIT_ENVIRONMENT
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
                .collect(),
        },
    );
    programs.insert(
        "terminal".into(),
        ProgramPolicy {
            command: "bash".into(),
            environment: vec![
                ("TERM".into(), "xterm-256color".into()),
                ("LANG".into(), "C.UTF-8".into()),
                ("HISTFILE".into(), "/dev/null".into()),
            ],
        },
    );
    Initialization {
        programs,
        apply_patch: true,
        workspace_context: true,
        directory_picker: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn target_serialization_retains_one_target_without_blocking_another() {
        let owner = Arc::new(TargetWriters::default());
        let a = ExecutionTargetId::parse("a".repeat(32)).unwrap();
        let b = ExecutionTargetId::parse("b".repeat(32)).unwrap();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, barrier) = tokio::sync::oneshot::channel::<()>();
        let retained = owner.clone();
        let target = a.clone();
        let task = tokio::spawn(async move {
            let _permit = retained.admit(&target).unwrap();
            entered.send(()).unwrap();
            let _ = barrier.await;
        });
        ready.await.unwrap();
        assert!(owner.admit(&a).is_none());
        assert!(owner.admit(&b).is_some());
        drop(task); // Losing the caller cannot release the admitted target.
        assert!(owner.admit(&a).is_none());
        release.send(()).unwrap();
        for _ in 0..100 {
            if owner.admit(&a).is_some() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("completed target operation did not release its gate");
    }
}
