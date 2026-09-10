//! Source-attributed repeat advice over bounded, cursor-bound settled Tool batches.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContributionContext, ContributionError, ContributionInput, ContributionKind,
    ContributionOutput, ContributionRegistrarContract, ContributionRegistration,
    ContributionResult, DomainDefinition, DomainHandle, DomainRegistrarContract,
    PostToolContributor,
};
use rsi_agent_session_protocol::{
    ContributionId, DomainIdentity, EffectId, InputMessageSource, SessionFact, SessionFactBody,
    SessionId, TurnId,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::ToolResultIdentity;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

mod signature;
#[cfg(test)]
mod tests;
const DOMAIN: &str = "rsi.repeat-tool-reminder";
const MAXIMUM_COUNT: u32 = 1_000_001;

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    thresholds: Vec<u32>,
    include: Option<Vec<String>>,
    exclude: Vec<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            thresholds: vec![3, 5, 8],
            include: None,
            exclude: Vec::new(),
        }
    }
}
impl Config {
    fn parse(value: &ConfigValue) -> rsi_meta::Result<Self> {
        let mut config: Self = if value.is_null() {
            Self::default()
        } else {
            serde_json::from_value(value.clone())
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?
        };
        config.thresholds.sort_unstable();
        if config.thresholds.is_empty()
            || config.thresholds.len() > 32
            || config
                .thresholds
                .iter()
                .any(|count| !(2..MAXIMUM_COUNT).contains(count))
            || config.thresholds.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(MetaError::InvalidInput(
                "repeat thresholds require 1..=32 unique counts in 2..=1000000".into(),
            ));
        }
        for names in config
            .include
            .iter_mut()
            .chain(std::iter::once(&mut config.exclude))
        {
            if names.len() > 64 {
                return Err(MetaError::InvalidInput(
                    "repeat Tool filter exceeds 64 names".into(),
                ));
            }
            names.sort();
            if names.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(MetaError::InvalidInput(
                    "repeat Tool filter has duplicate names".into(),
                ));
            }
            for name in names {
                validate_name(name).map_err(MetaError::InvalidInput)?;
            }
        }
        Ok(config)
    }
    fn tracks(&self, name: &str) -> bool {
        !self.exclude.iter().any(|entry| entry == name)
            && self
                .include
                .as_ref()
                .is_none_or(|names| names.iter().any(|entry| entry == name))
    }
}
fn validate_name(name: &str) -> Result<(), String> {
    rsi_tools_protocol::ToolCall::validate_fields("repeat-config", name, &ConfigValue::Null)
        .map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    session: Option<SessionId>,
    turn: Option<TurnId>,
    through_fact_seq: u64,
    name: Option<String>,
    signature: Option<String>,
    count: u32,
}
impl State {
    fn validate(&self) -> Result<(), String> {
        if self.session.is_some() != self.turn.is_some()
            || (self.session.is_none() && self.through_fact_seq != 0)
            || self.name.is_some() != self.signature.is_some()
            || self.name.is_some() != (self.count > 0)
            || self.count > MAXIMUM_COUNT
            || (self.session.is_none() && self.name.is_some())
            || (self.count > 0 && self.through_fact_seq == 0)
        {
            return Err("repeat state has inconsistent identity, cursor or count".into());
        }
        if let Some(name) = &self.name {
            validate_name(name)?;
        }
        if self.signature.as_ref().is_some_and(|hash| {
            hash.len() != 64
                || !hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        }) {
            return Err("repeat signature must be lowercase SHA-256".into());
        }
        Ok(())
    }
    fn clear(&mut self) {
        self.name = None;
        self.signature = None;
        self.count = 0;
    }
}

#[derive(Debug)]
struct Reminder {
    state: DomainHandle<State>,
    config: Config,
}
#[derive(Debug)]
struct Call {
    name: String,
    signature: String,
}
type Key = (EffectId, ToolResultIdentity);

fn key(fact: &SessionFact) -> ContributionResult<Key> {
    match fact.body() {
        SessionFactBody::ToolResult {
            effect_id,
            identity,
            ..
        }
        | SessionFactBody::ToolRejected {
            effect_id,
            identity,
            ..
        } => Ok((effect_id.clone(), identity.clone())),
        _ => Err(invalid("repeat callback requires settled Tool facts")),
    }
}
fn invalid(error: impl std::fmt::Display) -> ContributionError {
    ContributionError::Invalid(error.to_string())
}

#[async_trait]
impl PostToolContributor for Reminder {
    async fn contribute(
        &self,
        context: &ContributionContext,
        settled: &[Arc<SessionFact>],
        cancellation: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        if settled.len() > rsi_ai_protocol::MAX_CONTENT_BLOCKS {
            return Err(ContributionError::Capacity);
        }
        let view = context
            .domains
            .iter()
            .find(|view| view.snapshot.identity().id() == DOMAIN)
            .ok_or_else(|| invalid("repeat state is missing"))?;
        let previous = self.state.decode(&view.snapshot).map_err(invalid)?;
        let mut state = if previous.session.as_ref() == Some(context.header.session_id())
            && previous.turn.as_ref() == Some(&context.turn_id)
        {
            previous.clone()
        } else {
            State {
                session: Some(context.header.session_id().clone()),
                turn: Some(context.turn_id.clone()),
                through_fact_seq: context.accepted_fact_seq.saturating_sub(1),
                ..State::default()
            }
        };
        if state.through_fact_seq > context.horizon.fact_seq {
            return Err(invalid("repeat cursor exceeds its captured Turn horizon"));
        }
        let after = state.through_fact_seq;
        let (calls, human) = captured_calls(context, settled, after, &cancellation).await?;
        if human.is_some() {
            state.clear();
        }
        let mut inputs = Vec::new();
        for fact in settled {
            if fact.seq() <= after || human.is_some_and(|seq| fact.seq() < seq) {
                continue;
            }
            let call = calls
                .get(&key(fact)?)
                .ok_or_else(|| invalid("settled Tool lacks its exact captured intent"))?;
            if !self.config.tracks(&call.name) {
                continue;
            }
            state.count = if state.signature.as_ref() == Some(&call.signature) {
                state.count.saturating_add(1).min(MAXIMUM_COUNT)
            } else {
                1
            };
            state.name = Some(call.name.clone());
            state.signature = Some(call.signature.clone());
            if self.config.thresholds.binary_search(&state.count).is_ok() {
                inputs.push(ContributionInput::context(format!("Tool {} has been called {} consecutive times with identical arguments. Analyze the previous result before repeating it; use a different approach or finish when the task is complete.", call.name, state.count)));
            }
        }
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        state.through_fact_seq = context.horizon.fact_seq;
        let domains = if state == previous {
            Vec::new()
        } else {
            vec![self.state.propose(view.revision, &state).map_err(invalid)?]
        };
        Ok(ContributionOutput { inputs, domains })
    }
}

async fn captured_calls(
    context: &ContributionContext,
    settled: &[Arc<SessionFact>],
    after: u64,
    cancellation: &CancellationToken,
) -> ContributionResult<(BTreeMap<Key, Call>, Option<u64>)> {
    let mut wanted = BTreeSet::new();
    let mut calls = BTreeMap::new();
    for fact in settled {
        if fact.body().turn_id() != &context.turn_id || fact.seq() > context.horizon.fact_seq {
            return Err(invalid("settled Tool is outside the captured Turn"));
        }
        let key = key(fact)?;
        if !wanted.insert(key.clone()) {
            return Err(invalid("duplicate settled Tool identity"));
        }
        if let SessionFactBody::ToolRejected {
            name, arguments, ..
        } = fact.body()
        {
            calls.insert(
                key,
                Call {
                    name: name.clone(),
                    signature: signature::signature(name, arguments)?,
                },
            );
        }
    }
    let mut cursor = after;
    let mut human = None;
    while cursor < context.horizon.fact_seq {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let page = context.facts.read(cursor, 1).await?;
        if page.through_seq <= cursor || page.through_seq > context.horizon.fact_seq {
            return Err(invalid(
                "repeat Fact reader did not advance within its capture",
            ));
        }
        for fact in &page.facts {
            if fact.body().turn_id() != &context.turn_id {
                continue;
            }
            match fact.body() {
                SessionFactBody::InputMessageEntered {
                    source: InputMessageSource::Human { .. },
                    ..
                } => human = Some(fact.seq()),
                SessionFactBody::ToolIntent {
                    effect_id,
                    identity,
                    name,
                    arguments,
                    ..
                } if wanted.contains(&(effect_id.clone(), identity.clone())) => {
                    let key = (effect_id.clone(), identity.clone());
                    if calls
                        .insert(
                            key,
                            Call {
                                name: name.clone(),
                                signature: signature::signature(name, arguments)?,
                            },
                        )
                        .is_some()
                    {
                        return Err(invalid("duplicate or conflicting captured Tool intent"));
                    }
                }
                _ => {}
            }
        }
        cursor = page.through_seq;
    }
    Ok((calls, human))
}

/// Agent-only repeat reminder with typed cursor/state and ordinary `PostTool` registration.
#[derive(Clone, Copy, Debug, Default)]
pub struct RepeatToolReminderFactory;
#[async_trait]
impl PluginFactory for RepeatToolReminderFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config = Config::parse(desired)?;
        let normalized = serde_json::to_value(&config)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let bytes = serde_json::to_vec(&normalized)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?
            .len();
        Ok(PreparedActivation::with_state(normalized, config, bytes)
            .requiring_local::<DomainRegistrarContract>()
            .requiring_local::<ContributionRegistrarContract>())
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config: Config = plan.take_state()?;
        let definition = DomainDefinition::new(
            DomainIdentity::new(DOMAIN, 1).expect("static domain"),
            &State::default(),
            State::validate,
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        let context = plan.context().registration_context()?;
        let (state, domain_lease) = definition
            .register(plan.local::<DomainRegistrarContract>()?.as_ref(), &context)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let lease = plan
            .local::<ContributionRegistrarContract>()?
            .register(
                &context,
                ContributionRegistration::new(
                    ContributionId::new(DOMAIN).expect("static contribution"),
                    30,
                    ContributionKind::PostTool(Arc::new(Reminder { state, config })),
                ),
            )
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "withdraw repeat reminder",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    drop(domain_lease);
                    Ok(())
                })
            }),
        )
    }
}
