use super::{ApiError, Failure, Gate, Grant, Manager, Principal, Reply, wire};
use rsi_api_protocol::CallOrigin;

impl Manager {
    pub(super) fn grants(&self, origin: &CallOrigin) -> Reply<wire::Grants> {
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
        if !matches!(origin, CallOrigin::Local) {
            return Err(ApiError::Unauthorized);
        }
        change.validate()?;
        let writer = self.writer.try_acquire().map_err(|_| ApiError::Capacity)?;
        let mut document = self.state.lock().expect("Profile grants").document.clone();
        if document.revision != change.expected {
            return Ok(Err(Failure::Conflict));
        }
        let mut drain = None;
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
            drain = self
                .state
                .lock()
                .expect("Profile grant revocation")
                .gates
                .get_mut(&change.scope)
                .map(Gate::close);
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
        if self.domain.put("grants", value).await.is_err() {
            // A failed durable write has unknown publication. Never keep granting from a stale view.
            let mut state = self.state.lock().expect("Profile grant uncertainty");
            state.uncertain = true;
            for gate in state.gates.values_mut() {
                gate.close();
            }
            return Err(ApiError::OutcomeUnknown);
        }
        {
            let mut state = self.state.lock().expect("Profile grant publication");
            state.document = document.clone();
            if change.granted && !state.closed {
                state.gates.entry(change.scope).or_insert_with(Gate::open);
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
    ) -> Vec<wire::ChangeKind> {
        let state = self.state.lock().expect("Profile grant observation");
        if state.closed || state.uncertain {
            return vec![];
        }
        [
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
                    target: target.clone(),
                    operation: *operation,
                })
                .is_some_and(|gate| gate.open)
        })
        .collect()
    }
}
