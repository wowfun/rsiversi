use super::{
    ApiClient, ApiError, Arc, Deserialize, Empty, Never, OperationAccess, OperationClass,
    OperationEffect, OperationId, OperationSpec, RequestEncoding, Result, Serialize, call_json,
    revision,
};
use serde_json::Value;

/// The three existing provider families permitted in managed configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    /// `OpenAI` Responses and optional Image capabilities.
    Openai,
    /// Explicit OpenAI-compatible Chat Completions capability.
    OpenaiCompatible,
    /// `DeepSeek` with explicit Responses or Chat Completions configuration.
    Deepseek,
}
impl ProviderKind {
    /// Existing provider plugin and credential-owner identity.
    pub const fn owner(self) -> &'static str {
        match self {
            Self::Openai => "rsi.ai.provider.openai",
            Self::OpenaiCompatible => "rsi.ai.provider.openai-compatible",
            Self::Deepseek => "rsi.ai.provider.deepseek",
        }
    }
}
/// One definition, validated by its selected concrete provider before publication.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedProvider {
    /// Closed provider family; it cannot name an arbitrary plugin.
    pub provider: ProviderKind,
    /// The existing factory's closed non-secret configuration document.
    pub config: Value,
}
/// Desired and observed applied managed-provider revisions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvidersSnapshot {
    /// Durable desired revision as exact decimal text.
    pub desired_revision: String,
    /// Last successfully converged desired revision in this owner generation.
    pub applied_revision: String,
    /// Desired provider configurations, without secrets.
    pub deployments: Vec<ManagedProvider>,
    /// Whether the current desired configuration is still converging.
    pub applying: bool,
    /// Bounded diagnostic for failed convergence; absence does not prove connectivity.
    pub diagnostic: Option<String>,
}
impl ProvidersSnapshot {
    /// Checks external response bounds and revision consistency.
    pub fn validate(&self) -> Result<()> {
        let desired = revision(&self.desired_revision)?;
        let applied = revision(&self.applied_revision)?;
        if applied > desired
            || self
                .diagnostic
                .as_ref()
                .is_some_and(|value| value.len() > 4096)
            || self.deployments.len() > 64
            || serde_json::to_vec(&self.deployments)
                .map_err(|_| ApiError::Invalid("invalid provider definitions".into()))?
                .len()
                > 1024 * 1024
        {
            return Err(ApiError::Invalid(
                "invalid managed provider snapshot".into(),
            ));
        }
        Ok(())
    }
}
/// Exact managed-provider wire operations.
#[derive(Clone, Copy, Debug)]
pub enum ProvidersOperation {
    /// Read current desired/applied state.
    Read,
    /// Preflight and apply one desired replacement against its revision.
    Replace,
}
impl ProvidersOperation {
    /// Returns the registered finite wire contract.
    ///
    /// # Panics
    /// Panics if the static operation identities are invalid.
    pub fn spec(self) -> OperationSpec {
        OperationSpec {
            id: OperationId::new(
                "providers",
                match self {
                    Self::Read => "read",
                    Self::Replace => "replace",
                },
                1,
            )
            .expect("static operation"),
            access: OperationAccess::Authenticated,
            class: OperationClass::Data,
            effect: match self {
                Self::Read => OperationEffect::Read,
                Self::Replace => OperationEffect::Mutation,
            },
            encoding: RequestEncoding::Json,
            maximum_request_bytes: 2 * 1024 * 1024,
            maximum_response_bytes: 2 * 1024 * 1024,
        }
    }
}
/// Managed provider configuration client; no operation tests a remote model implicitly.
#[derive(Clone, Debug)]
pub struct ManagedProvidersClient {
    api: Arc<dyn ApiClient>,
}
impl ManagedProvidersClient {
    /// Requires both exact managed-provider operations.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if [ProvidersOperation::Read, ProvidersOperation::Replace]
            .into_iter()
            .any(|operation| !api.operations().contains(&operation.spec()))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    async fn call<I: Serialize + Sync>(
        &self,
        operation: ProvidersOperation,
        input: &I,
    ) -> Result<ProvidersSnapshot> {
        let value = match call_json::<_, ProvidersSnapshot, Never>(
            self.api.as_ref(),
            &operation.spec(),
            input,
        )
        .await?
        {
            Ok(value) => value,
            Err(never) => match never {},
        };
        value.validate().map_err(|error| match operation {
            ProvidersOperation::Read => error,
            ProvidersOperation::Replace => ApiError::OutcomeUnknown,
        })?;
        Ok(value)
    }
    /// Reads desired configuration and the actual convergence result.
    pub async fn read(&self) -> Result<ProvidersSnapshot> {
        self.call(ProvidersOperation::Read, &Empty {}).await
    }
    /// Replaces once; desired publication and successful convergence are separate facts.
    pub async fn replace(
        &self,
        expected_revision: &str,
        deployments: Vec<ManagedProvider>,
    ) -> Result<ProvidersSnapshot> {
        #[derive(Serialize)]
        struct Replace<'a> {
            expected_revision: &'a str,
            deployments: &'a [ManagedProvider],
        }
        let next = revision(expected_revision)?
            .checked_add(1)
            .ok_or_else(|| ApiError::Invalid("provider revision exhausted".into()))?;
        let probe = ProvidersSnapshot {
            desired_revision: expected_revision.into(),
            applied_revision: "0".into(),
            deployments: deployments.clone(),
            applying: false,
            diagnostic: None,
        };
        probe.validate()?;
        let snapshot = self
            .call(
                ProvidersOperation::Replace,
                &Replace {
                    expected_revision,
                    deployments: &deployments,
                },
            )
            .await?;
        if snapshot.desired_revision != next.to_string() || snapshot.deployments != deployments {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(snapshot)
    }
}
