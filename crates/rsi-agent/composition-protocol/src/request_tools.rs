use super::{AgentCompositionError, AgentCompositionPin, Result};
use async_trait::async_trait;
use rsi_agent_session_protocol::{
    ApplicationToolBinding, ApplicationToolView, ModelSelection, SessionHeader,
};
use rsi_tools_protocol::{
    PreparedToolCall, RetainedToolResult, ToolCall, ToolDefinition, ToolResultIdentity, ToolRuntime,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

/// A process has resolved a registry view and sealed its admitted native executors.
#[derive(Debug)]
pub struct RequestToolCatalog {
    /// Exact process incarnation.
    pub generation: String,
    /// Stable source of read-only recovery receipts, if supplied.
    pub receipt_source: Option<String>,
    /// Name to registration binding in that incarnation.
    pub registrations: BTreeMap<String, String>,
    /// Supplemental runtime backed by the captured view.
    pub runtime: Arc<dyn ToolRuntime>,
    /// Rendered current system guidance from the same request capture; empty means none.
    pub instructions: String,
    /// Final input preparation owned by this same request capture.
    pub inputs: Option<Arc<dyn RequestInputPreparer>>,
    /// Optional middleware for native Tools called through this captured request.
    pub middleware: Option<Arc<dyn rsi_tools_protocol::ToolMiddleware>>,
}

/// Borrowed native input cut for a single provider request series.
#[derive(Debug)]
pub struct RequestInputContext<'a> {
    /// Native Turn owning the claims.
    pub turn_id: &'a rsi_agent_session_protocol::TurnId,
    /// Native execution Step; several request series may share it.
    pub step_id: &'a rsi_agent_session_protocol::StepId,
    /// Last native Fact captured before preparation.
    pub fact_horizon: u64,
    /// Unprepared original input Facts, in canonical order.
    pub claims: &'a [rsi_agent_session_protocol::SessionFact],
    /// Ordered preset decision over the captured claims, if supplied.
    pub previous: Option<&'a rsi_agent_session_protocol::StepInputDecision>,
    /// New native context claims staged alongside the final decision.
    pub contributed: &'a [rsi_agent_session_protocol::SessionFactBody],
    /// Other Facts between those staged claims and the decision.
    pub trailing_facts: usize,
}

impl serde::Serialize for RequestInputContext<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut input = serializer.serialize_struct("RequestInputContext", 7)?;
        input.serialize_field("turn_id", self.turn_id)?;
        input.serialize_field("step_id", self.step_id)?;
        input.serialize_field("fact_horizon", &self.fact_horizon)?;
        if let Some(previous) = self.previous {
            // A replacement already carries its final body; omission needs no source bytes.
            let unchanged: BTreeSet<_> = previous
                .messages()
                .unwrap_or_default()
                .iter()
                .filter_map(|entry| {
                    if let rsi_agent_session_protocol::PreparedInput::Claimed {
                        claim: rsi_agent_session_protocol::InputClaimRef::Captured { fact_seq, .. },
                    } = entry
                    {
                        Some(fact_seq.get())
                    } else {
                        None
                    }
                })
                .collect();
            let originals: Vec<_> = self
                .claims
                .iter()
                .filter(|fact| unchanged.contains(&fact.seq()))
                .collect();
            input.serialize_field("claims", &originals)?;
        } else {
            input.serialize_field("claims", self.claims)?;
        }
        input.serialize_field("previous", &self.previous)?;
        input.serialize_field("contributed", self.contributed)?;
        input.serialize_field("trailing_facts", &self.trailing_facts)?;
        input.end()
    }
}

/// Application-owned final decision using its retained request capture.
#[async_trait]
pub trait RequestInputPreparer: fmt::Debug + Send + Sync + 'static {
    /// Runs once; the executor publishes its decision and reuses it for retries.
    async fn prepare(
        &self,
        context: &RequestInputContext<'_>,
        stop: CancellationToken,
    ) -> Result<rsi_agent_session_protocol::StepInputDecision>;
}

#[derive(Debug)]
pub(super) struct RequestPreparation {
    view: ApplicationToolView,
    instruction: Option<rsi_agent_session_protocol::StepInstruction>,
    inputs: Option<Arc<dyn RequestInputPreparer>>,
    middleware: Option<Arc<dyn rsi_tools_protocol::ToolMiddleware>>,
}

/// Optional Host-selected source at the Conversation request boundary.
#[async_trait]
pub trait RequestToolSource: fmt::Debug + Send + Sync + 'static {
    /// Preset names assigned exclusively to this source by the Host profile.
    fn replaced_preset_tools(&self) -> Option<Arc<BTreeSet<String>>> {
        None
    }
    /// Waits for configured input preparation outside the per-Step callback deadline.
    /// Returns true when this Session requires the input preparer on the captured catalog.
    /// A tools-only source can use the default and fall back when unavailable.
    async fn await_input_preparer(&self, _header: &SessionHeader) -> Result<bool> {
        Ok(false)
    }

    /// Captures once for the exact provider selection. Preset names reserve their slots.
    async fn capture(
        &self,
        header: &SessionHeader,
        selection: &ModelSelection,
        reserved: &BTreeSet<String>,
        stop: CancellationToken,
    ) -> Result<RequestToolCatalog>;
}

/// Nominal service for the process selected by this Host profile.
#[derive(Debug)]
pub struct RequestToolSourceContract;
impl rsi_meta_contract::LocalContract for RequestToolSourceContract {
    const KEY: &'static str = "rsi.agent.request-tools";
    type Service = dyn RequestToolSource;
}

/// Which executor owns this call in the producing request.
#[derive(Debug)]
pub enum RequestToolRoute<'a> {
    /// The application executes its own registered definition.
    Application,
    /// The preset owns the body and may offer it to this application's middleware.
    Native(Option<&'a Arc<dyn rsi_tools_protocol::ToolMiddleware>>),
}

impl AgentCompositionPin {
    /// Attaches only the selected request's catalog to a copy of the generation pin.
    /// # Errors
    /// Rejects a collision, an unbound definition or a catalog beyond the model's budget.
    pub fn with_request_catalog(
        mut self,
        catalog: RequestToolCatalog,
        header: &SessionHeader,
    ) -> Result<Self> {
        let instruction = if catalog.instructions.is_empty() {
            None
        } else {
            let instruction = rsi_agent_session_protocol::StepInstruction {
                producer: rsi_agent_session_protocol::ContributionId::new(
                    "rsi.application.system-prompt",
                )
                .map_err(|error| AgentCompositionError::InvalidInput(error.to_string()))?,
                text: catalog.instructions,
            };
            rsi_agent_session_protocol::validate_step_instructions(std::slice::from_ref(
                &instruction,
            ))
            .map_err(|error| AgentCompositionError::InvalidInput(error.to_string()))?;
            Some(instruction)
        };
        let mut bindings = Vec::new();
        let mut owners = BTreeSet::new();
        for definition in catalog.runtime.definitions() {
            let name = definition.name();
            if self.tools.scheduling(name).is_some()
                || header
                    .delegation_policy()
                    .is_some_and(|policy| !policy.tools().contains(name))
                || definition.scheduling() != rsi_tools_protocol::ToolScheduling::Exclusive
                || definition.program_role() != rsi_tools_protocol::ToolProgramRole::Unavailable
            {
                return Err(AgentCompositionError::InvalidInput(
                    "inadmissible request tool definition".into(),
                ));
            }
            let registration = catalog.registrations.get(name).ok_or_else(|| {
                AgentCompositionError::InvalidInput("missing application registration".into())
            })?;
            let owner = catalog.runtime.registration_owner(name).ok_or_else(|| {
                AgentCompositionError::InvalidInput("missing request tool owner".into())
            })?;
            owners.insert(owner.to_owned());
            bindings.push(ApplicationToolBinding {
                registration: registration.clone(),
                owner: owner.to_owned(),
                definition,
            });
        }
        if bindings.len() != catalog.registrations.len() {
            return Err(AgentCompositionError::InvalidInput(
                "unbound application registration".into(),
            ));
        }
        bindings.sort_by(|a, b| a.definition.name().cmp(b.definition.name()));
        let names: Vec<_> = bindings
            .iter()
            .map(|b| b.definition.name().to_owned())
            .collect();
        let view = ApplicationToolView::Ready {
            generation: catalog.generation,
            receipt_source: catalog.receipt_source,
            tools: bindings,
        };
        view.validate(&names)
            .map_err(|e| AgentCompositionError::InvalidInput(e.to_string()))?;
        self.tools = Arc::new(RequestTools {
            preset: self.tools,
            additional: catalog.runtime,
            names: names.into_iter().collect(),
            owners,
        });
        self.tool_weight = Arc::default();
        self.tool_definition_weight()?;
        self.application = Some(Arc::new(RequestPreparation {
            view,
            instruction,
            inputs: catalog.inputs,
            middleware: catalog.middleware,
        }));
        Ok(self)
    }

    /// Records that the selected application could not supply a view.
    #[must_use]
    pub fn with_unavailable_request_tools(mut self, reason: &str) -> Self {
        let mut bounded = String::new();
        for c in reason.chars().filter(|c| !c.is_control()) {
            if bounded.len() + c.len_utf8() > 512 {
                break;
            }
            bounded.push(c);
        }
        if bounded.trim().is_empty() {
            bounded = "application tool source unavailable".into();
        }
        self.application = Some(Arc::new(RequestPreparation {
            view: ApplicationToolView::Unavailable { reason: bounded },
            instruction: None,
            inputs: None,
            middleware: None,
        }));
        self
    }

    /// Supplemental provenance for the current provider request series.
    #[must_use]
    pub fn request_tool_view(&self) -> Option<&ApplicationToolView> {
        self.application.as_deref().map(|prepared| &prepared.view)
    }

    /// Current application guidance captured alongside this request's Tools.
    #[must_use]
    pub fn request_instruction(&self) -> Option<&rsi_agent_session_protocol::StepInstruction> {
        self.application.as_deref()?.instruction.as_ref()
    }

    /// Input callback retained by the same application capture as its Tools.
    #[must_use]
    pub fn request_input_preparer(&self) -> Option<&dyn RequestInputPreparer> {
        self.application.as_deref()?.inputs.as_deref()
    }

    /// Routes a prepared name within this request's collision-free catalog.
    #[must_use]
    pub fn request_tool_route(&self, name: &str) -> RequestToolRoute<'_> {
        if let Some(application) = &self.application {
            if application
                .view
                .bindings()
                .binary_search_by(|binding| binding.definition.name().cmp(name))
                .is_ok()
            {
                return RequestToolRoute::Application;
            }
            return RequestToolRoute::Native(application.middleware.as_ref());
        }
        RequestToolRoute::Native(None)
    }
}

#[derive(Debug)]
struct RequestTools {
    preset: Arc<dyn ToolRuntime>,
    additional: Arc<dyn ToolRuntime>,
    names: BTreeSet<String>,
    owners: BTreeSet<String>,
}
impl RequestTools {
    fn by_name(&self, name: &str) -> &dyn ToolRuntime {
        if self.names.contains(name) {
            self.additional.as_ref()
        } else {
            self.preset.as_ref()
        }
    }
    fn by_owner(&self, identity: &ToolResultIdentity) -> &dyn ToolRuntime {
        if self.owners.contains(identity.owner_id()) {
            self.additional.as_ref()
        } else {
            self.preset.as_ref()
        }
    }
}
#[async_trait]
impl ToolRuntime for RequestTools {
    fn visit_definitions(&self, visitor: &mut dyn FnMut(&ToolDefinition)) {
        self.preset.visit_definitions(visitor);
        self.additional.visit_definitions(visitor);
    }
    fn definition(&self, name: &str) -> Option<ToolDefinition> {
        self.by_name(name).definition(name)
    }
    fn registration_owner(&self, name: &str) -> Option<&str> {
        self.by_name(name).registration_owner(name)
    }
    fn scheduling(&self, name: &str) -> Option<rsi_tools_protocol::ToolScheduling> {
        self.by_name(name).scheduling(name)
    }
    fn program_role(&self, name: &str) -> Option<rsi_tools_protocol::ToolProgramRole> {
        self.by_name(name).program_role(name)
    }
    fn program_roles(&self) -> BTreeMap<String, rsi_tools_protocol::ToolProgramRole> {
        self.preset.program_roles()
    }
    fn output_declarations(&self) -> BTreeMap<String, rsi_tools_protocol::ToolOutputDeclaration> {
        self.preset.output_declarations()
    }
    fn prepare(
        &self,
        id: &str,
        call: ToolCall,
    ) -> rsi_tools_protocol::Result<Box<dyn PreparedToolCall>> {
        self.by_name(&call.name).prepare(id, call)
    }
    fn query(&self, id: &ToolResultIdentity) -> rsi_tools_protocol::Result<RetainedToolResult> {
        self.by_owner(id).query(id)
    }
    async fn wait(
        &self,
        id: &ToolResultIdentity,
        stop: CancellationToken,
    ) -> rsi_tools_protocol::Result<RetainedToolResult> {
        self.by_owner(id).wait(id, stop).await
    }
    fn commit(&self, id: &ToolResultIdentity) -> rsi_tools_protocol::Result<()> {
        self.by_owner(id).commit(id)
    }
}
