//! Live approval answerer waterfall plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_approval_protocol::{
    Approval, ApprovalAnswerer, ApprovalAnswerers, ApprovalAnswerersContract, ApprovalContract,
    ApprovalDecision, ApprovalError, ApprovalLease, ApprovalOutcome, ApprovalRequest, Result,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Service {
    state: Arc<State>,
}

#[derive(Debug)]
struct State {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    next_registration: u64,
    answerers: BTreeMap<u64, Arc<dyn ApprovalAnswerer>>,
}

impl ApprovalAnswerers for Service {
    fn register(&self, answerer: Arc<dyn ApprovalAnswerer>) -> Result<ApprovalLease> {
        let mut inner = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.next_registration = inner
            .next_registration
            .checked_add(1)
            .ok_or_else(|| ApprovalError::Answerer("registration identity exhausted".into()))?;
        let registration = inner.next_registration;
        inner.answerers.insert(registration, answerer);
        let state = Arc::downgrade(&self.state);
        Ok(ApprovalLease::new(move || {
            remove(&state, registration);
        }))
    }
}

#[async_trait]
impl Approval for Service {
    async fn ask(
        &self,
        request: ApprovalRequest,
        cancellation: CancellationToken,
    ) -> Result<ApprovalOutcome> {
        request.validate()?;
        let answerers = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .answerers
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for answerer in answerers {
            if cancellation.is_cancelled() {
                return Err(ApprovalError::Cancelled);
            }
            if let Some(outcome) = answerer
                .answer(request.clone(), cancellation.clone())
                .await?
            {
                outcome.validate()?;
                return Ok(outcome);
            }
        }
        Ok(ApprovalOutcome {
            decision: ApprovalDecision::Deny,
            answerer: "rsi.approval.default-deny".into(),
            reason: Some("no approval answerer allowed the request".into()),
        })
    }
}

fn remove(state: &Weak<State>, registration: u64) {
    if let Some(state) = state.upgrade() {
        state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .answerers
            .remove(&registration);
    }
}

/// Ordinary factory for one Approval service generation.
#[derive(Clone, Debug, Default)]
pub struct ApprovalFactory;

#[async_trait]
impl PluginFactory for ApprovalFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(MetaError::InvalidInput(
                "Approval configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let service = Arc::new(Service {
            state: Arc::new(State {
                inner: Mutex::new(Inner::default()),
            }),
        });
        let approval: Arc<dyn Approval> = service.clone();
        let answerers: Arc<dyn ApprovalAnswerers> = service;
        let approval_supply = plan.context().provide_local::<ApprovalContract>(approval)?;
        let answerer_supply = plan
            .context()
            .provide_local::<ApprovalAnswerersContract>(answerers)?;
        plan.defer(
            "withdraw Approval services",
            Box::new(move || {
                Box::pin(async move {
                    drop(answerer_supply);
                    drop(approval_supply);
                    Ok(())
                })
            }),
        )
    }
}
