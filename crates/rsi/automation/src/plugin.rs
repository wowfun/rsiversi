use crate::now;
use crate::{
    AdmissionError, AutomationService, BrowserRegistryContract, GoalExplorer, IngressSource,
    Ledger, Policy, PolicyOwner,
};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use rsi_api_protocol::{ApiError, ApiRegistrarContract, ApiRegistration, CallOrigin, json_handler};
use rsi_automation_api::{Request, decimal};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

#[derive(Debug)]
pub struct AutomationContract;
impl LocalContract for AutomationContract {
    const KEY: &'static str = "rsi.automation";
    type Service = AutomationService;
}
#[derive(Debug, Default)]
pub struct AutomationFactory {
    #[cfg(feature = "test-support")]
    fixture_port: Option<u16>,
}
impl AutomationFactory {
    #[cfg(feature = "test-support")]
    pub fn with_fixture_network(port: u16) -> Self {
        Self {
            fixture_port: Some(port),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    directory: PathBuf,
    #[serde(default)]
    runtime: Option<rsi_browser::RuntimeConfig>,
    #[serde(default)]
    listen: Option<std::net::SocketAddr>,
    #[serde(default)]
    sources: Vec<IngressSource>,
}
fn meta(e: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(e.to_string())
}
#[async_trait]
#[expect(
    clippy::too_many_lines,
    reason = "One ordered lifecycle retains ownership through failure and settlement"
)]
impl PluginFactory for AutomationFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let c: Config = serde_json::from_value(config.clone()).map_err(meta)?;
        if !c.directory.is_absolute()
            || c.sources.len() > 16
            || c.listen.is_some_and(|a| !a.ip().is_loopback())
        {
            return Err(meta(
                "explicit private Automation directory and loopback listener required",
            ));
        }
        if let Some(runtime) = c.runtime {
            runtime.validate().map_err(meta)?;
        }
        let mut ids = std::collections::BTreeSet::new();
        for source in c.sources {
            crate::protocol::identity(&source.id).map_err(meta)?;
            source.credential.validate().map_err(meta)?;
            if source.credential.owner.as_str() != "rsi.automation" || !ids.insert(source.id) {
                return Err(meta(
                    "Automation sources require unique identities and owner-local credentials",
                ));
            }
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<ApiRegistrarContract>()
            .requiring_local::<rsi_browser::RuntimePoolContract>()
            .requiring_local::<rsi_sandbox::SandboxContract>()
            .requiring_local::<rsi_credentials_protocol::CredentialsResolveContract>()
            .requiring_local::<rsi_session_protocol::FrozenSessionOwnerContract>()
            .requiring_local::<rsi_agent_composition_protocol::AgentCompositionContract>()
            .requiring_local::<rsi_workspace_protocol::WorkspaceRegistryContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config: Config =
            serde_json::from_value(plan.config().as_ref().clone()).map_err(meta)?;
        let directory = config.directory.clone();
        let (ledger, policy) = tokio::task::spawn_blocking(move || {
            let ledger = Ledger::open(&directory.join("ledger"), now())?;
            let policy = Arc::new(PolicyOwner::open(directory.join("policy"))?);
            Ok::<_, AdmissionError>((ledger, policy))
        })
        .await
        .map_err(meta)?
        .map_err(meta)?;
        let browser = match config.runtime {
            Some(config) => {
                let pool = plan.local::<rsi_browser::RuntimePoolContract>()?;
                #[cfg(feature = "test-support")]
                let runtime = if let Some(port) = self.fixture_port {
                    pool.acquire_with_fixture(config, port).await
                } else {
                    pool.acquire(config).await
                };
                #[cfg(not(feature = "test-support"))]
                let runtime = pool.acquire(config).await;
                Some(runtime.map_err(meta)?)
            }
            None => None,
        };
        let roots = Arc::new(crate::goal::BrowserRegistry::default());
        let roots_supply = plan
            .context()
            .provide_local::<BrowserRegistryContract>(roots.clone())?;
        let explorer = Arc::new(GoalExplorer {
            ledger: ledger.clone(),
            sessions: plan.local::<rsi_session_protocol::FrozenSessionOwnerContract>()?,
            composition: plan
                .local::<rsi_agent_composition_protocol::AgentCompositionContract>()?,
            workspaces: plan.local::<rsi_workspace_protocol::WorkspaceRegistryContract>()?,
            workspace: config.directory.join("workspace"),
            roots,
        });
        rsi_files_native_fs::create_absolute_directory_no_follow(&explorer.workspace)
            .map_err(meta)?;
        let credentials = plan.local::<rsi_credentials_protocol::CredentialsResolveContract>()?;
        let mut sources = BTreeMap::new();
        for source in config.sources {
            sources.insert(
                source.id,
                credentials
                    .resolve(&source.credential)
                    .await
                    .map_err(meta)?
                    .secret,
            );
        }
        let service = AutomationService::new(
            ledger,
            policy.clone(),
            browser,
            sources,
            Some(explorer.clone()),
        );
        let protection_supply = plan
            .context()
            .provide_local::<rsi_session_protocol::SessionProtectionContract>(policy)?;
        let supply = plan
            .context()
            .provide_local::<AutomationContract>(service.clone())?;
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let registrations = Arc::new(std::sync::Mutex::new(Vec::<ApiRegistration>::new()));
        let cleanup_registrations = registrations.clone();
        let cleanup_service = service.clone();
        plan.defer(
            "retire Automation listener and attempts",
            Box::new(move || {
                Box::pin(async move {
                    let registrations = std::mem::take(
                        &mut *cleanup_registrations
                            .lock()
                            .map_err(|_| "Automation registration lock poisoned")?,
                    );
                    futures_util::future::join_all(
                        registrations.into_iter().map(ApiRegistration::close),
                    )
                    .await;
                    cleanup_service.close().await;
                    drop(supply);
                    drop(protection_supply);
                    drop(roots_supply);
                    Ok(())
                })
            }),
        )?;
        for spec in rsi_automation_api::operations() {
            let owner = service.clone();
            let explorer = explorer.clone();
            let expected = spec.clone();
            registrations.lock().map_err(meta)?.push(
                registrar
                    .register(
                        spec,
                        json_handler(move |context, request: Request| {
                            let owner = owner.clone();
                            let explorer = explorer.clone();
                            let expected = expected.clone();
                            async move {
                                if request.spec() != expected {
                                    return Err(ApiError::Invalid(
                                        "Automation operation mismatch".into(),
                                    ));
                                }
                                request.validate()?;
                                owner
                                    .api(context.origin, request, Some(explorer))
                                    .await
                                    .map(Ok::<_, Never>)
                            }
                        }),
                    )
                    .map_err(meta)?,
            );
        }
        if let Some(address) = config.listen
            && let Err(error) = service.listen(address).await
        {
            service.close().await;
            return Err(meta(error));
        }
        service.start();
        Ok(())
    }
}
#[derive(Serialize)]
enum Never {}
fn api_error(error: AdmissionError) -> ApiError {
    match error {
        AdmissionError::Capacity => ApiError::Capacity,
        AdmissionError::NotFound | AdmissionError::Unavailable | AdmissionError::Corrupt => {
            ApiError::Unavailable
        }
        AdmissionError::OutcomeUnknown => ApiError::OutcomeUnknown,
        AdmissionError::Conflict => {
            ApiError::Invalid("automation revision or identity conflict".into())
        }
        AdmissionError::Unauthorized => ApiError::Unauthorized,
        AdmissionError::Invalid(message) => ApiError::Invalid(message),
    }
}
impl AutomationService {
    /// # Errors
    /// Rejects invalid requests, missing caller authority or failed durable operations.
    #[expect(
        clippy::too_many_lines,
        reason = "Keep one complete ownership operation or acceptance scenario together"
    )]
    pub async fn api(
        self: &Arc<Self>,
        origin: CallOrigin,
        request: Request,
        explorer: Option<Arc<GoalExplorer>>,
    ) -> rsi_api_protocol::Result<Value> {
        if self.stop.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        request.validate()?;
        match request {
            Request::Status => {
                let catalog = if matches!(origin, CallOrigin::Local) {
                    if let Some(explorer) = explorer {
                        explorer
                            .pin()
                            .await
                            .ok()
                            .map(|p| p.source_digest().to_owned())
                    } else {
                        None
                    }
                } else {
                    None
                };
                Ok(json!({"readiness":self.readiness(),"catalog_digest":catalog}))
            }
            Request::Diagnostics => {
                local(&origin)?;
                Ok(
                    json!({"browser":self.browser.as_ref().map(|b|b.diagnostic()),"storage_available":self.ledger.available(),"intake":self.policy.intake_diagnostics()}),
                )
            }
            Request::Policy => {
                local(&origin)?;
                serde_json::to_value(self.policy.snapshot().map_err(api_error)?.as_ref())
                    .map_err(|e| ApiError::Invalid(e.to_string()))
            }
            Request::SetPolicy {
                expected_revision,
                policy,
            } => {
                local(&origin)?;
                let policy: Policy =
                    serde_json::from_value(policy).map_err(|e| ApiError::Invalid(e.to_string()))?;
                let owner = self.policy.clone();
                let expected = decimal(&expected_revision)?;
                let policy = self
                    .tasks
                    .spawn_blocking(move || owner.update(expected, policy))
                    .await
                    .map_err(|_| ApiError::OutcomeUnknown)?
                    .map_err(api_error)?;
                self.revoke_disabled().await;
                Ok(json!({"revision":policy.revision.to_string()}))
            }
            Request::List {
                after,
                watermark,
                limit,
            } => {
                let cut = decimal(&after)?;
                let mark = watermark.as_deref().map(decimal).transpose()?;
                let (rows, watermark, more) = self
                    .ledger
                    .run(move |ledger| ledger.list_with_sources(cut, mark, usize::from(limit)))
                    .await
                    .map_err(api_error)?;
                let next = rows.last().map_or(after, |(a, _)| a.id.to_string());
                let mut entries = vec![];
                let mut leases = vec![];
                for (a, source) in rows {
                    let lease = match self.policy.permits(&origin, &source, &a.rule.id, "view") {
                        Ok(lease) => lease,
                        Err(AdmissionError::Unauthorized | AdmissionError::NotFound) => continue,
                        Err(error) => return Err(api_error(error)),
                    };
                    {
                        leases.push(lease);
                        entries.push(json!({"id":a.id.to_string(),"task_id":a.task_id.to_string(),"source":source,"rule":a.rule.id,"rule_revision":a.rule.revision.to_string(),"state":a.state,"exploration":a.exploration,"url":a.deployment.url,"sha":a.deployment.sha,"environment":a.deployment.environment,"created_ms":a.created_ms.to_string(),"verdict":a.result.as_ref().map(|r|&r.outcome),"session_id":a.session_id}));
                    }
                }
                if leases
                    .iter()
                    .any(tokio_util::sync::CancellationToken::is_cancelled)
                    || matches!(&origin,CallOrigin::Device(d)if d.revoked.is_cancelled())
                {
                    return Err(ApiError::Unauthorized);
                }
                Ok(
                    json!({"entries":entries,"after":next,"watermark":watermark.to_string(),"more":more}),
                )
            }
            request => {
                let (id, operation) = match &request {
                    Request::Get { id } | Request::Artifact { id, .. } => (id, "view"),
                    Request::Cancel { id, .. } => (id, "cancel"),
                    Request::Resume { id, .. } => (id, "resume"),
                    _ => unreachable!(),
                };
                let id = decimal(id)?;
                let (attempt, source) = self
                    .ledger
                    .run(move |ledger| ledger.get_with_source(id))
                    .await
                    .map_err(api_error)?;
                let lease = self
                    .policy
                    .permits(&origin, &source, &attempt.rule.id, operation)
                    .map_err(api_error)?;
                let reply = match request {
                    Request::Get { .. } => {
                        let current_policy = self.policy.snapshot().map_err(api_error)?;
                        let current_rule = current_policy.rule(&source, &attempt.rule.id);
                        let terminal = !attempt.work_pending();
                        let mut value = serde_json::to_value(&attempt)
                            .map_err(|e| ApiError::Invalid(e.to_string()))?;
                        value["id"] = json!(attempt.id.to_string());
                        value["task_id"] = json!(attempt.task_id.to_string());
                        value["created_ms"] = json!(attempt.created_ms.to_string());
                        value["current_rule_revision"] =
                            json!(current_rule.map(|r| r.revision.to_string()));
                        value["may_cancel"] = json!(
                            !terminal
                                && self
                                    .policy
                                    .permits(&origin, &source, &attempt.rule.id, "cancel")
                                    .is_ok()
                        );
                        value["may_resume"] = json!(
                            terminal
                                && current_rule.is_some_and(|r| r.enabled)
                                && self
                                    .policy
                                    .permits(&origin, &source, &attempt.rule.id, "resume")
                                    .is_ok()
                        );
                        value
                    }
                    Request::Artifact { ordinal, .. } => {
                        json!({"png":STANDARD.encode(self.ledger.run(move |ledger| ledger.artifact(id,ordinal)).await.map_err(api_error)?)})
                    }
                    Request::Cancel { request_id, .. } => {
                        let request_id = principal_request(&origin, &request_id);
                        let a = self.cancel(id, &request_id).await.map_err(api_error)?;
                        json!({"id":a.id.to_string(),"state":a.state})
                    }
                    Request::Resume {
                        request_id,
                        rule_revision,
                        ..
                    } => {
                        let request_id = principal_request(&origin, &request_id);
                        let policy = self.policy.snapshot().map_err(api_error)?;
                        let rule = policy
                            .rule(&source, &attempt.rule.id)
                            .ok_or(ApiError::Unavailable)?;
                        if !rule.enabled {
                            return Err(ApiError::Invalid("current rule is disabled".into()));
                        }
                        if rule.revision != decimal(&rule_revision)? {
                            return Err(ApiError::Invalid("current rule revision required".into()));
                        }
                        {
                            let rule = rule.clone();
                            let a = self
                                .ledger
                                .run(move |ledger| ledger.resume(id, &request_id, rule))
                                .await
                                .map_err(api_error)?;
                            json!({"id":a.id.to_string(),"state":a.state})
                        }
                    }
                    _ => unreachable!(),
                };
                if operation == "view" {
                    authorize_read_return(&origin, &lease)?;
                }
                Ok(reply)
            }
        }
    }
}
fn local(origin: &CallOrigin) -> rsi_api_protocol::Result<()> {
    if matches!(origin, CallOrigin::Local) {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}
fn authorize_read_return(
    origin: &CallOrigin,
    lease: &tokio_util::sync::CancellationToken,
) -> rsi_api_protocol::Result<()> {
    if lease.is_cancelled() || matches!(origin, CallOrigin::Device(d) if d.revoked.is_cancelled()) {
        return Err(ApiError::Unauthorized);
    }
    Ok(())
}

fn principal_request(origin: &CallOrigin, request: &str) -> String {
    let principal = match origin {
        CallOrigin::Local => "local".to_owned(),
        CallOrigin::Device(device) => format!("device:{}", device.id.as_str()),
    };
    hex::encode(Sha256::digest(format!("{principal}:{request}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn return_of_protected_evidence_requires_both_transport_and_policy_to_remain_live() {
        let revoked = tokio_util::sync::CancellationToken::new();
        let origin = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
            id: rsi_api_protocol::DeviceId::from_bytes([9; 16]),
            revoked: revoked.clone(),
        });
        let grant = tokio_util::sync::CancellationToken::new();
        authorize_read_return(&origin, &grant).unwrap();
        revoked.cancel();
        assert!(!grant.is_cancelled());
        assert!(matches!(
            authorize_read_return(&origin, &grant),
            Err(ApiError::Unauthorized)
        ));
        grant.cancel();
        assert!(matches!(
            authorize_read_return(&CallOrigin::Local, &grant),
            Err(ApiError::Unauthorized)
        ));
    }
}
