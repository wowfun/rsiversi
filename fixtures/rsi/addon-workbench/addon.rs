use async_trait::async_trait;
use rsi::{AddonScope, StandardAddonBuilder, StandardAddonSet};
use rsi_host::{ProfileEntry, ProfileFragment};
use rsi_meta::{
    ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation, UpdateMode,
};
use rsi_settings_protocol::{SettingsContract, SettingsSpec, ValidateWith};
use rsi_tools_protocol::{
    ToolDefinition, ToolExecution, ToolExecutor, ToolRegistrarContract, ToolRegistration,
    ToolResult, ToolTimeoutPolicy,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug, Default)]
pub struct Evidence {
    pub live_tools: AtomicUsize,
    pub tool_calls: AtomicUsize,
    pub entered: tokio::sync::Notify,
    pub release: tokio::sync::Notify,
}
pub fn addons(evidence: Arc<Evidence>) -> StandardAddonSet {
    let mut addon = StandardAddonBuilder::new("fixture.workbench");
    addon
        .register_factory(
            AddonScope::Agent,
            "fixture.workbench.tool",
            "1",
            UpdateMode::Replayable,
            Arc::new(ToolFactory(evidence)),
        )
        .unwrap();
    addon
        .register_factory(
            AddonScope::Agent,
            "fixture.workbench.plan",
            "1",
            UpdateMode::Replayable,
            Arc::new(rsi_agent_plan_policy::PlanPolicyFactory),
        )
        .unwrap();
    addon
        .register_factory(
            AddonScope::Service,
            "fixture.workbench.settings",
            "1",
            UpdateMode::Replayable,
            Arc::new(SettingsFactory),
        )
        .unwrap();
    addon
        .register_fragment_at(
            AddonScope::Service,
            ProfileFragment::new(
                "fixture.workbench.settings",
                [ProfileEntry::new(
                    "fixture-settings",
                    "fixture.workbench.settings",
                    Value::Null,
                )],
            ),
        )
        .unwrap();
    StandardAddonSet::new([addon.build().unwrap()]).unwrap()
}

pub fn profile(label: &str) -> String {
    format!(
        "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'context'\nplugin = 'rsi.agent.context.default'\n[[steps]]\nkind = 'plugin'\nid = 'plan'\nplugin = 'fixture.workbench.plan'\nconfig = {{ allow_tools = ['spawn_agent'] }}\n[[steps]]\nkind = 'plugin'\nid = 'tool'\nplugin = 'fixture.workbench.tool'\nconfig = {{ label = '{label}' }}\n[[steps]]\nkind = 'plugin'\nid = 'agents'\nplugin = 'rsi.agent.tools'\n"
    )
}

#[derive(Debug)]
struct ToolFactory(Arc<Evidence>);
#[derive(Debug)]
struct Echo {
    label: String,
    evidence: Arc<Evidence>,
}
#[async_trait]
impl ToolExecutor for Echo {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        self.evidence.tool_calls.fetch_add(1, Ordering::SeqCst);
        if arguments.get("hold").and_then(Value::as_bool) == Some(true) {
            self.evidence.entered.notify_one();
            tokio::select! {
                () = self.evidence.release.notified() => {},
                () = execution.cancellation.cancelled() => return Err(rsi_tools_protocol::ToolError::Cancelled),
            }
        }
        ToolResult::new(
            json!({"label":self.label,"arguments":arguments}),
            vec![],
            false,
        )
    }
}
#[async_trait]
impl PluginFactory for ToolFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let label = desired
            .get("label")
            .and_then(Value::as_str)
            .filter(|label| matches!(*label, "A" | "B"))
            .ok_or_else(|| MetaError::InvalidInput("fixture label must be A or B".into()))?;
        if desired.as_object().is_none_or(|object| object.len() != 1) {
            return Err(MetaError::InvalidInput(
                "fixture Tool accepts only label".into(),
            ));
        }
        Ok(
            PreparedActivation::with_state(desired.clone(), label.to_owned(), 1)
                .requiring_local::<ToolRegistrarContract>(),
        )
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let label = plan.take_state::<String>()?;
        let lease = plan
            .local::<ToolRegistrarContract>()?
            .register(ToolRegistration {
                definition: ToolDefinition::new(
                    "fixture_echo",
                    format!("Independent workbench {label}"),
                    json!({"type":"object"}),
                )
                .map_err(meta)?,
                timeout: ToolTimeoutPolicy::Execution { timeout_ms: 10_000 },
                executor: Arc::new(Echo {
                    label,
                    evidence: self.0.clone(),
                }),
            })
            .map_err(meta)?;
        let evidence = self.0.clone();
        plan.defer(
            "withdraw fixture Tool",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    evidence.live_tools.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        )?;
        self.0.live_tools.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}

#[derive(Debug)]
struct SettingsFactory;
#[async_trait]
impl PluginFactory for SettingsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()).requiring_local::<SettingsContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registration = plan.local::<SettingsContract>()?.register(SettingsSpec {
            namespace: "fixture.workbench".into(), defaults: json!({"note":"fixture"}), base: json!({}),
            metadata: rsi_settings_protocol::SettingsMetadata { schema:json!({"type":"object","properties":{"note":{"type":"string","maxLength":32}},"required":["note"],"additionalProperties":false}), applies:rsi_settings_protocol::SettingsApply::Live, description:"Independent addon operator note".into(), sensitive_fields:vec![] },
            validator:Arc::new(ValidateWith(|value: &Value| {
                if value.as_object().is_some_and(|object| object.len() == 1) && value.get("note").and_then(Value::as_str).is_some_and(|note| note.len() <= 32) { Ok(()) } else { Err(rsi_settings_protocol::SettingsError::InvalidInput("fixture note must have at most 32 bytes".into())) }
            })),
        }).map_err(meta)?;
        plan.defer(
            "withdraw fixture Settings",
            Box::new(move || {
                Box::pin(async move {
                    drop(registration);
                    Ok(())
                })
            }),
        )
    }
}
