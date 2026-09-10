//! Live approval answerer waterfall plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_approval_protocol::{
    Approval, ApprovalAnswerer, ApprovalAnswerers, ApprovalAnswerersContract, ApprovalContract,
    ApprovalDecision, ApprovalError, ApprovalLease, ApprovalOutcome, ApprovalRequest,
    MAXIMUM_APPROVAL_ANSWERERS, Result,
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
    runtime: rsi_meta::RuntimeIdentity,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    next_registration: u64,
    answerers: BTreeMap<u64, Arc<AnswererEntry>>,
    snapshot: Option<(
        rsi_meta::RegistrationOrderSnapshot,
        Arc<[Arc<AnswererEntry>]>,
    )>,
}

#[derive(Debug)]
struct AnswererEntry {
    answerer: Arc<dyn ApprovalAnswerer>,
    position: rsi_meta::RegistrationPosition,
}

impl Inner {
    fn snapshot(&mut self) -> Result<Arc<[Arc<AnswererEntry>]>> {
        if let Some((order, snapshot)) = &self.snapshot
            && order.is_current()
            && snapshot.iter().all(|entry| entry.position.is_admitting())
        {
            return Ok(snapshot.clone());
        }
        let entries: Vec<_> = self
            .answerers
            .values()
            .filter(|entry| entry.position.is_admitting())
            .cloned()
            .collect();
        let positions: Vec<_> = entries.iter().map(|entry| entry.position.clone()).collect();
        let order = rsi_meta::RegistrationOrderSnapshot::capture(&positions)
            .map_err(|error| ApprovalError::Answerer(error.to_string()))?;
        let mut ranked: Vec<_> = entries.into_iter().zip(order.ranks()).collect();
        ranked.sort_by(|left, right| left.1.cmp(right.1));
        let entries: Arc<[_]> = ranked.into_iter().map(|(entry, _)| entry).collect();
        let entries = self
            .snapshot
            .as_ref()
            .filter(|(_, previous)| {
                previous.len() == entries.len()
                    && previous
                        .iter()
                        .zip(entries.iter())
                        .all(|(left, right)| Arc::ptr_eq(left, right))
            })
            .map_or(entries.clone(), |(_, previous)| previous.clone());
        self.snapshot = Some((order, entries.clone()));
        Ok(entries)
    }
}

impl ApprovalAnswerers for Service {
    fn register(
        &self,
        context: &rsi_meta::RegistrationContext,
        answerer: Arc<dyn ApprovalAnswerer>,
    ) -> Result<ApprovalLease> {
        if context.runtime_identity() != self.state.runtime {
            return Err(ApprovalError::Answerer(
                "approval registration belongs to another Runtime".into(),
            ));
        }
        let registration = {
            let mut inner = self
                .state
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.next_registration = inner
                .next_registration
                .checked_add(1)
                .ok_or_else(|| ApprovalError::Answerer("registration identity exhausted".into()))?;
            inner.next_registration
        };
        let state = Arc::downgrade(&self.state);
        let ((), lease) = context
            .register(
                "withdraw approval answerer",
                move || {
                    remove(&state, registration);
                    Ok(())
                },
                |position| {
                    let mut inner = self
                        .state
                        .inner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if inner.answerers.len() >= MAXIMUM_APPROVAL_ANSWERERS {
                        return Err(MetaError::CapacityExhausted {
                            resource: "approval answerers",
                        });
                    }
                    inner.snapshot = None;
                    inner
                        .answerers
                        .insert(registration, Arc::new(AnswererEntry { answerer, position }));
                    Ok(())
                },
            )
            .map_err(|error| ApprovalError::Answerer(error.to_string()))?;
        Ok(ApprovalLease::from_registration(lease))
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
            .snapshot()?;
        for entry in answerers.iter() {
            if cancellation.is_cancelled() {
                return Err(ApprovalError::Cancelled);
            }
            if let Some(outcome) = entry
                .answerer
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
        let removed = {
            let mut inner = state
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (inner.answerers.remove(&registration), inner.snapshot.take())
        };
        drop(removed);
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
                runtime: plan.context().runtime_identity(),
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
