use super::{ApiError, Failure, Gate, Grant, Manager, Principal, Reply, wire};
use rsi_api_protocol::CallOrigin;

impl Manager {
    pub(crate) fn admit_ssh_stdio(
        &self,
        origin: &CallOrigin,
        target: &rsi_execution::ExecutionTargetId,
        server: &str,
        references: &std::collections::BTreeSet<rsi_credentials_protocol::CredentialRef>,
    ) -> rsi_api_protocol::Result<tokio_util::task::task_tracker::TaskTrackerToken> {
        self.domain
            .ensure_available()
            .map_err(rsi_storage_domain::storage_error)?;
        let state = self.state.lock().expect("MCP stdio grants");
        if state.closed {
            return Err(ApiError::ShuttingDown);
        }
        let CallOrigin::Device(device) = origin else {
            return Ok(self.tasks.token());
        };
        if device.revoked.is_cancelled() {
            return Err(ApiError::Unauthorized);
        }
        let principal = Principal::Device(device.id.clone());
        state
            .gates
            .iter()
            .find_map(|(grant, gate)| {
                if grant.principal != principal || !gate.open {
                    return None;
                }
                let wire::GrantScope::SshStdio {
                    target: allowed,
                    server: name,
                    credentials,
                } = &grant.scope
                else {
                    return None;
                };
                (allowed == target
                    && name == server
                    && references
                        .iter()
                        .all(|reference| credentials.binary_search(reference).is_ok()))
                .then(|| gate.tasks.token())
            })
            .ok_or(ApiError::Unauthorized)
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn execution_visibility(
        &self,
        origin: &CallOrigin,
    ) -> rsi_api_protocol::Result<rsi_execution::ExecutionVisibility> {
        use rsi_execution::{
            ExecutionLocation, ExecutionLocations, ExecutionOperation, ExecutionVisibility,
        };
        self.domain
            .ensure_available()
            .map_err(rsi_storage_domain::storage_error)?;
        let state = self.state.lock().expect("execution visibility grants");
        if state.closed {
            return Err(ApiError::ShuttingDown);
        }
        let CallOrigin::Device(device) = origin else {
            return Ok(ExecutionVisibility::new(
                ExecutionLocations::all(),
                ExecutionOperation::new(self.tasks.token()),
            ));
        };
        if device.revoked.is_cancelled() {
            return Err(ApiError::Unauthorized);
        }
        let principal = Principal::Device(device.id.clone());
        let mut locations = std::collections::BTreeSet::from([ExecutionLocation::Local]);
        let mut permits = Vec::new();
        for (grant, gate) in &state.gates {
            if grant.principal == principal
                && gate.open
                && let wire::GrantScope::SshUse { target } = &grant.scope
            {
                locations.insert(ExecutionLocation::Ssh {
                    target: target.clone(),
                });
                permits.push(gate.tasks.token());
            }
        }
        Ok(ExecutionVisibility::new(
            ExecutionLocations::only(locations).map_err(|_| ApiError::Capacity)?,
            ExecutionOperation::new(permits),
        ))
    }
    pub(super) fn grants(&self, origin: &CallOrigin) -> Reply<wire::Grants> {
        self.domain
            .ensure_available()
            .map_err(rsi_storage_domain::storage_error)?;
        if !matches!(origin, CallOrigin::Local) {
            return Err(ApiError::Unauthorized);
        }
        Ok(Ok(self
            .state
            .lock()
            .expect("Profile grants")
            .document
            .clone()))
    }
    pub(super) async fn set_grant(
        &self,
        origin: CallOrigin,
        change: wire::SetGrant,
    ) -> Reply<wire::Grants> {
        self.domain
            .ensure_available()
            .map_err(rsi_storage_domain::storage_error)?;
        if !matches!(origin, CallOrigin::Local) {
            return Err(ApiError::Unauthorized);
        }
        change.validate()?;
        let writer = self.writer.try_acquire().map_err(|_| ApiError::Capacity)?;
        let mut document = self.state.lock().expect("Profile grants").document.clone();
        if document.revision != change.expected {
            return Ok(Err(Failure::Conflict));
        }
        if change.granted {
            if let Principal::Device(device) = &change.scope.principal
                && !self
                    .administration
                    .list()?
                    .iter()
                    .any(|record| &record.id == device)
            {
                return Err(ApiError::Invalid(
                    "Profile grants require a registered device".into(),
                ));
            }
            if !document.scopes.contains(&change.scope) {
                if document.scopes.len() == 256 {
                    return Ok(Err(Failure::Busy));
                }
                document.scopes.push(change.scope.clone());
                document.scopes.sort();
            }
        } else {
            document.scopes.retain(|scope| scope != &change.scope);
        }
        let revision = document
            .revision
            .parse::<u64>()
            .map_err(|_| ApiError::Unavailable)?;
        document.revision = revision
            .checked_add(1)
            .ok_or(ApiError::Capacity)?
            .to_string();
        if document.validate().is_err() {
            return Ok(Err(Failure::Busy));
        }
        let value = serde_json::to_value(&document).map_err(|_| ApiError::Unavailable)?;
        let drain = if change.granted {
            None
        } else {
            self.state
                .lock()
                .expect("Profile grant revocation")
                .gates
                .get_mut(&change.scope)
                .map(Gate::close)
        };
        self.domain
            .put("grants", value)
            .await
            .map_err(rsi_storage_domain::storage_error)?;
        {
            let mut state = self.state.lock().expect("Profile grant publication");
            state.document = document.clone();
            if change.granted && !state.closed {
                state
                    .gates
                    .entry(change.scope)
                    .and_modify(Gate::reopen)
                    .or_insert_with(Gate::open);
            } else {
                state.gates.remove(&change.scope);
            }
        }
        drop(writer);
        if let Some(drain) = drain {
            drain.wait().await;
        }
        Ok(Ok(document))
    }
    pub(super) fn allowed(
        &self,
        principal: &Principal,
        target: &wire::Target,
    ) -> rsi_api_protocol::Result<Vec<wire::ChangeKind>> {
        self.domain
            .ensure_available()
            .map_err(rsi_storage_domain::storage_error)?;
        let state = self.state.lock().expect("Profile grant observation");
        if state.closed {
            return Ok(vec![]);
        }
        Ok([
            wire::ChangeKind::Enable,
            wire::ChangeKind::Disable,
            wire::ChangeKind::Configuration,
        ]
        .into_iter()
        .filter(|operation| {
            state
                .gates
                .get(&Grant {
                    principal: principal.clone(),
                    scope: wire::GrantScope::Profile {
                        target: target.clone(),
                        operation: *operation,
                    },
                })
                .is_some_and(|gate| gate.open)
        })
        .collect())
    }
}
