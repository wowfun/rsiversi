//! Exact-name explicit command registry plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_commands_protocol::{
    CommandDefinition, CommandDescriptor, CommandError, CommandLease, CommandRequest,
    CommandResult, CommandRuntime, CommandRuntimeContract, Result,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Registry {
    state: Arc<State>,
}

#[derive(Debug)]
struct State {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    next_registration: u64,
    definitions: BTreeMap<String, Entry>,
}

#[derive(Debug)]
struct Entry {
    registration: u64,
    definition: CommandDefinition,
}

#[async_trait]
impl CommandRuntime for Registry {
    fn register(&self, definition: CommandDefinition) -> Result<CommandLease> {
        definition.validate()?;
        let name = definition.name.clone();
        let mut inner = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner.definitions.contains_key(&name) {
            return Err(CommandError::Duplicate(name));
        }
        inner.next_registration = inner
            .next_registration
            .checked_add(1)
            .ok_or_else(|| CommandError::Execution("registration identity exhausted".into()))?;
        let registration = inner.next_registration;
        inner.definitions.insert(
            name.clone(),
            Entry {
                registration,
                definition,
            },
        );
        let state = Arc::downgrade(&self.state);
        Ok(CommandLease::new(move || {
            remove_if_current(&state, &name, registration);
        }))
    }

    fn descriptors(&self) -> Vec<CommandDescriptor> {
        self.state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .definitions
            .values()
            .map(|entry| CommandDescriptor {
                name: entry.definition.name.clone(),
                description: entry.definition.description.clone(),
            })
            .collect()
    }

    async fn execute(
        &self,
        request: CommandRequest,
        cancellation: CancellationToken,
    ) -> Result<CommandResult> {
        request.validate()?;
        if cancellation.is_cancelled() {
            return Err(CommandError::Cancelled);
        }
        let handler = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .definitions
            .get(&request.name)
            .map(|entry| Arc::clone(&entry.definition.handler))
            .ok_or_else(|| CommandError::Unknown(request.name.clone()))?;
        let result = handler.execute(request.text, cancellation).await?;
        result.validate()?;
        Ok(result)
    }
}

fn remove_if_current(state: &Weak<State>, name: &str, registration: u64) {
    let Some(state) = state.upgrade() else {
        return;
    };
    let mut inner = state
        .inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if inner
        .definitions
        .get(name)
        .is_some_and(|entry| entry.registration == registration)
    {
        inner.definitions.remove(name);
    }
}

/// Ordinary factory for one Commands registry generation.
#[derive(Clone, Debug, Default)]
pub struct CommandsFactory;

#[async_trait]
impl PluginFactory for CommandsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(MetaError::InvalidInput(
                "Commands configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let commands: Arc<dyn CommandRuntime> = Arc::new(Registry {
            state: Arc::new(State {
                inner: Mutex::new(Inner::default()),
            }),
        });
        let supply = plan
            .context()
            .provide_local::<CommandRuntimeContract>(commands)?;
        plan.defer(
            "withdraw Commands registry",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
