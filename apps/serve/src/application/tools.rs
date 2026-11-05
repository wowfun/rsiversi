use super::failure;
use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    AgentCompositionError, RequestInputContext, RequestInputPreparer, RequestToolCatalog,
    RequestToolSource,
};
use rsi_agent_session_protocol::{ExecutionLocation, SessionHeader};
use rsi_agent_turn_protocol::AgentCallerAuthority;
use rsi_credentials_protocol::SecretValue;
use rsi_tools_protocol::{
    ToolCatalogProvider, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolOutput,
    ToolRegistration, ToolResult, ToolTimeoutPolicy,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Semaphore, watch};
use tokio_util::sync::CancellationToken;

const MAX_RESPONSE: usize = 32 * 1024 * 1024;
mod native;
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Listener {
    protocol: u32,
    generation: String,
    port: u16,
}
impl Listener {
    pub(super) fn validate(&self) -> rsi_meta::Result<()> {
        if self.protocol != 11 || self.port == 0 {
            return Err(failure("invalid application tool listener"));
        }
        rsi_agent_session_protocol::validate_identifier("application generation", &self.generation)
            .map_err(failure)
    }
}

#[derive(Debug)]
struct Endpoint {
    listener: Listener,
    key: SecretValue,
    client: reqwest::Client,
    retired: CancellationToken,
}
impl Endpoint {
    async fn call<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &(impl Serialize + Sync),
        maximum: usize,
    ) -> Result<T, ToolError> {
        let read = async {
            let request = self
                .client
                .post(format!("http://127.0.0.1:{}/v1/{path}", self.listener.port))
                .header("authorization", self.key.expose_secret())
                .header("content-type", "application/json")
                .body(
                    serde_json::to_vec(body)
                        .map_err(|_| ToolError::InvalidInput("application tool payload".into()))?,
                );
            let request = if matches!(path, "capture" | "prepare" | "start") {
                request
            } else {
                request.timeout(Duration::from_secs(10))
            };
            let reply = request
                .send()
                .await
                .map_err(|_| ToolError::OutcomeUnknown)?;
            if reply.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
                return Err(ToolError::Capacity);
            }
            let status = reply.status();
            if !status.is_success() {
                let mut message = format!("application tool {path} returned {status}");
                if status == reqwest::StatusCode::BAD_REQUEST
                    && let Ok(bytes) = response_bytes(reply, 4 * 1024).await
                    && let Ok(body) = serde_json::from_slice::<Value>(&bytes)
                    && let Some(detail) = body["error"].as_str().filter(|detail| !detail.is_empty())
                {
                    message.push_str(": ");
                    message.extend(detail.chars().filter(|c| !c.is_control()).take(512));
                }
                return Err(ToolError::Execution(message));
            }
            let bytes = response_bytes(reply, maximum).await?;
            serde_json::from_slice(&bytes)
                .map_err(|_| ToolError::Execution("invalid application tool response".into()))
        };
        tokio::select! {biased; ()=self.retired.cancelled()=>Err(ToolError::OutcomeUnknown), result=read=>result}
    }
}

async fn response_bytes(
    mut reply: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, ToolError> {
    if reply
        .content_length()
        .is_some_and(|len| len > maximum as u64)
    {
        return Err(ToolError::Capacity);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = reply.chunk().await.map_err(|_| ToolError::OutcomeUnknown)? {
        if chunk.len() > maximum - bytes.len() {
            return Err(ToolError::Capacity);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[derive(Debug)]
pub(super) struct Source {
    profile: String,
    replaced_preset_tools: Option<Arc<BTreeSet<String>>>,
    catalogs: Arc<dyn ToolCatalogProvider>,
    endpoint: watch::Receiver<Option<Arc<Endpoint>>>,
    lifetime: CancellationToken,
    executions: Arc<Semaphore>,
}
/// Owned by the application supervisor. A failed startup drops the last sender.
#[derive(Clone, Debug)]
pub(super) struct Publisher {
    endpoint: watch::Sender<Option<Arc<Endpoint>>>,
    client: reqwest::Client,
    pub(super) receipt_directory: std::path::PathBuf,
}

impl Source {
    pub(super) fn new(
        catalogs: Arc<dyn ToolCatalogProvider>,
        lifetime: CancellationToken,
        profile: String,
        replaced_preset_tools: BTreeSet<String>,
        receipt_directory: std::path::PathBuf,
    ) -> rsi_meta::Result<(Self, Publisher)> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(failure)?;
        let (sender, endpoint) = watch::channel(None);
        Ok((
            Self {
                profile,
                replaced_preset_tools: (!replaced_preset_tools.is_empty())
                    .then(|| Arc::new(replaced_preset_tools)),
                catalogs,
                endpoint,
                lifetime,
                executions: Arc::new(Semaphore::new(8)),
            },
            Publisher {
                endpoint: sender,
                client,
                receipt_directory,
            },
        ))
    }
}

impl Publisher {
    pub(super) fn publish(&self, listener: Listener, key: SecretValue, retired: CancellationToken) {
        self.endpoint.send_replace(Some(Arc::new(Endpoint {
            listener,
            key,
            retired,
            client: self.client.clone(),
        })));
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Captured {
    generation: String,
    view: String,
    tools: Vec<Row>,
    instructions: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    definition: ToolDefinition,
    registration: String,
}
#[derive(Debug)]
struct View {
    id: String,
    session: rsi_agent_session_protocol::SessionId,
    endpoint: Arc<Endpoint>,
    current: watch::Receiver<Option<Arc<Endpoint>>>,
    lifetime: CancellationToken,
    executions: Arc<Semaphore>,
}
impl Drop for View {
    fn drop(&mut self) {
        if !self.endpoint.retired.is_cancelled()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let endpoint = self.endpoint.clone();
            let id = self.id.clone();
            runtime.spawn(async move {
                let _ = endpoint
                    .call::<Value>("release", &json!({"view":id}), 128)
                    .await;
            });
        }
    }
}
fn invalid(error: impl std::fmt::Display) -> AgentCompositionError {
    AgentCompositionError::InvalidInput(error.to_string())
}

#[async_trait]
impl RequestToolSource for Source {
    fn replaced_preset_tools(&self) -> Option<Arc<BTreeSet<String>>> {
        self.replaced_preset_tools.clone()
    }
    async fn await_input_preparer(
        &self,
        header: &SessionHeader,
    ) -> rsi_agent_composition_protocol::Result<bool> {
        if header.coordinates().location() != &ExecutionLocation::Local {
            return Ok(false);
        }
        let mut endpoints = self.endpoint.clone();
        tokio::select! { biased;
            () = self.lifetime.cancelled() => Err(AgentCompositionError::ShuttingDown),
            result = endpoints.wait_for(|candidate| candidate.as_ref().is_some_and(|peer| !peer.retired.is_cancelled())) =>
                result.map(|_| true).map_err(|_| invalid("application stopped before its input preparer became ready")),
        }
    }

    async fn capture(
        &self,
        header: &SessionHeader,
        selection: &rsi_agent_session_protocol::ModelSelection,
        reserved: &BTreeSet<String>,
        stop: CancellationToken,
    ) -> rsi_agent_composition_protocol::Result<RequestToolCatalog> {
        if header.coordinates().location() != &ExecutionLocation::Local {
            return Err(invalid("application tools require a local workspace"));
        }
        let endpoint = self
            .endpoint
            .borrow()
            .as_ref()
            .filter(|e| !e.retired.is_cancelled())
            .cloned()
            .ok_or_else(|| invalid("application tools are preparing"))?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(invalid)?;
        let view = Arc::new(View {
            id: hex::encode(nonce),
            session: header.session_id().clone(),
            endpoint: endpoint.clone(),
            current: self.endpoint.clone(),
            lifetime: self.lifetime.clone(),
            executions: self.executions.clone(),
        });
        let mut model_selection =
            json!({"provider":selection.model.deployment(),"model":selection.model.model()});
        if let Some(effort) = &selection.reasoning_effort {
            model_selection["reasoningEffort"] = json!(effort.as_str());
        }
        let request = json!({"view":view.id,"modelSelection":model_selection,"reserved":reserved,"allowed":header.delegation_policy().map(rsi_agent_session_protocol::DelegationPolicy::tools),
            "header":{"version":4,"id":header.session_id(),"cwd":header.canonical_cwd(),"createdAt":header.created_at_ms()}});
        let captured: Captured = tokio::select! {biased; ()=stop.cancelled()=>return Err(AgentCompositionError::ShuttingDown),
        result=endpoint.call("capture",&request,1024*1024)=>result.map_err(invalid)?};
        if captured.generation != endpoint.listener.generation
            || captured.view != view.id
            || captured.tools.len() + reserved.len() > rsi_tools_protocol::MAXIMUM_REGISTERED_TOOLS
        {
            return Err(invalid("application tool capture differs from admission"));
        }
        let stage = self.catalogs.begin_stage().map_err(invalid)?;
        let mut registrations = BTreeMap::new();
        let tools = captured
            .tools
            .into_iter()
            .map(|row| {
                let name = row.definition.name().to_owned();
                registrations.insert(name.clone(), row.registration.clone());
                ToolRegistration {
                    definition: row.definition,
                    output: None,
                    timeout: ToolTimeoutPolicy::Execution {
                        timeout_ms: rsi_tools_protocol::MAXIMUM_TOOL_TIMEOUT_MS,
                    },
                    executor: Arc::new(Body {
                        view: view.clone(),
                        name,
                        registration: row.registration,
                    }),
                }
            })
            .collect::<Vec<_>>();
        let _lease = if tools.is_empty() {
            None
        } else {
            Some(stage.registrar().register_batch(tools).map_err(invalid)?)
        };
        let runtime = stage.seal().map_err(invalid)?;
        Ok(RequestToolCatalog {
            generation: captured.generation,
            receipt_source: Some(self.profile.clone()),
            registrations,
            runtime,
            instructions: captured.instructions,
            middleware: Some(view.clone()),
            inputs: Some(view),
        })
    }
}

#[async_trait]
impl RequestInputPreparer for View {
    async fn prepare(
        &self,
        context: &RequestInputContext<'_>,
        stop: CancellationToken,
    ) -> rsi_agent_composition_protocol::Result<rsi_agent_session_protocol::StepInputDecision> {
        #[derive(Serialize)]
        struct Request<'a, 'b> {
            view: &'a str,
            session: &'a rsi_agent_session_protocol::SessionId,
            input: &'a RequestInputContext<'b>,
        }
        let request = Request {
            view: &self.id,
            session: &self.session,
            input: context,
        };
        tokio::select! {biased;
            () = stop.cancelled() => Err(AgentCompositionError::ShuttingDown),
            result = self.endpoint.call("prepare", &request, rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES) => result.map_err(invalid),
        }
    }
}

#[derive(Debug)]
struct Body {
    view: Arc<View>,
    name: String,
    registration: String,
}
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Outcome {
    Pending,
    Settled { result: ToolOutput },
    Unknown,
}
#[async_trait]
impl ToolExecutor for Body {
    async fn execute(
        &self,
        args: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let caller = execution
            .extension::<AgentCallerAuthority>()
            .ok_or_else(|| {
                ToolError::Execution("missing started application tool authority".into())
            })?;
        if caller.session_id() != &self.view.session {
            return Err(ToolError::Execution(
                "application tool Session mismatch".into(),
            ));
        }
        let effect = caller.tool_effect_id().ok_or_else(|| {
            ToolError::Execution("application tool requires a started model effect".into())
        })?;
        let _permit = self
            .view
            .executions
            .clone()
            .try_acquire_owned()
            .map_err(|_| ToolError::Capacity)?;
        let started = caller.tool_identity().ok_or(ToolError::OutcomeUnknown)?;
        if started.call_id() != execution.call_id || started.invocation_id() != effect.as_str() {
            return Err(ToolError::OutcomeUnknown);
        }
        let identity = json!({"session":caller.session_id(),"effect":effect,
            "registration":self.registration,"identity":started});
        let mut request = identity.clone();
        request["view"] = json!(self.view.id);
        request["name"] = json!(self.name);
        request["args"] = args;
        if execution.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        self.view
            .start_and_observe(&request, &identity, &execution.cancellation)
            .await
    }
}

impl View {
    async fn start_and_observe(
        &self,
        request: &Value,
        identity: &Value,
        stop: &CancellationToken,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let observing = self.run(request, identity, stop);
        tokio::pin!(observing);
        tokio::select! {biased;
            result=&mut observing=>result,
            ()=self.lifetime.cancelled()=>Err(ToolError::OutcomeUnknown),
            ()=stop.cancelled()=>tokio::time::timeout(Duration::from_secs(5), observing)
                .await.unwrap_or(Err(ToolError::OutcomeUnknown)),
        }
    }
    async fn run(
        &self,
        request: &Value,
        identity: &Value,
        stop: &CancellationToken,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        // A lost start reply authorizes only outcome lookup.
        let start = tokio::select! {biased;
            reply=self.endpoint.call::<Outcome>("start", request, MAX_RESPONSE)=>reply,
            ()=stop.cancelled()=>Err(ToolError::OutcomeUnknown),
        };
        match start {
            Ok(Outcome::Settled { result }) => {
                return result.into_result().map_err(|_| ToolError::OutcomeUnknown);
            }
            Ok(Outcome::Unknown) => return Err(ToolError::OutcomeUnknown),
            Ok(Outcome::Pending) | Err(ToolError::OutcomeUnknown | ToolError::Execution(_)) => {}
            Err(error) => return Err(error),
        }
        let mut current = self.current.clone();
        let mut endpoint = self.endpoint.clone();
        let mut cancel_sent = false;
        loop {
            if endpoint.retired.is_cancelled() {
                let ready = current.wait_for(|candidate| {
                    candidate
                        .as_ref()
                        .is_some_and(|peer| !peer.retired.is_cancelled())
                });
                endpoint = tokio::select! {biased;
                    ()=self.lifetime.cancelled()=>return Err(ToolError::OutcomeUnknown),
                    next=ready=>next.map_err(|_|ToolError::OutcomeUnknown)?.as_ref().cloned().ok_or(ToolError::OutcomeUnknown)?,
                };
                cancel_sent = false;
            }
            let cancelling = stop.is_cancelled();
            if cancelling && !cancel_sent {
                cancel_sent = endpoint
                    .call::<Value>("cancel", identity, 128)
                    .await
                    .is_ok_and(|reply| reply["accepted"] == true);
            }
            let outcome = tokio::select! {biased;
                ()=stop.cancelled(), if !cancelling=>continue,
                result=endpoint.call::<Outcome>("outcome", identity, MAX_RESPONSE)=>result,
            };
            match outcome {
                Ok(Outcome::Settled { result }) => {
                    return result.into_result().map_err(|_| ToolError::OutcomeUnknown);
                }
                Ok(Outcome::Pending) | Err(ToolError::OutcomeUnknown | ToolError::Capacity) => {}
                Ok(Outcome::Unknown) | Err(_) => return Err(ToolError::OutcomeUnknown),
            }
            tokio::select! {biased;
                ()=self.lifetime.cancelled()=>return Err(ToolError::OutcomeUnknown),
                ()=endpoint.retired.cancelled()=>{},
                ()=tokio::time::sleep(Duration::from_millis(100))=>{},
            }
        }
    }
}

#[cfg(test)]
mod tests;
