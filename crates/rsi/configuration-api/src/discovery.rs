//! Explicit provider I/O for setup, independent of registered route enumeration.
use super::{
    ApiError, Deserialize, ManagedProvidersClient, Never, ProviderKind, ProvidersOperation, Result,
    Serialize, call_json,
};
use rsi_ai_protocol::{DiscoveredModel, validate_discovered_models};

/// A non-secret configuration draft, valid before a deployment exists.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryRequest {
    /// Concrete provider family owning URL and response translation.
    pub provider: ProviderKind,
    /// API root; compatible services include their API prefix here.
    pub endpoint: String,
    /// Credential address under the selected provider owner.
    pub slot: String,
}
impl DiscoveryRequest {
    /// Validates wire inputs before any credential or network access.
    pub fn validate(&self) -> Result<()> {
        rsi_credentials_protocol::CredentialRef::new(self.provider.owner(), &self.slot)
            .map_err(|_| ApiError::Invalid("invalid discovery credential slot".into()))?;
        if self.endpoint.len() > 2048 {
            return Err(ApiError::Invalid("invalid discovery endpoint".into()));
        }
        let url = url::Url::parse(&self.endpoint)
            .map_err(|_| ApiError::Invalid("invalid discovery endpoint".into()))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ApiError::Invalid("invalid discovery endpoint".into()));
        }
        Ok(())
    }
}
/// Candidates, with no assertion about callable Language routes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoverySnapshot {
    /// Exact request identity used to reject obsolete presentation results.
    pub request: DiscoveryRequest,
    /// Provider-advertised model identifiers and optional capacities.
    pub models: Vec<DiscoveredModel>,
}
impl DiscoverySnapshot {
    /// Checks a decoded reply against an already validated outgoing request.
    /// The API encoder and transport own encoded byte limits.
    pub fn validate_reply(&self, expected: &DiscoveryRequest) -> Result<()> {
        if &self.request != expected {
            return Err(ApiError::Invalid(
                "discovery reply names a different request".into(),
            ));
        }
        validate_discovered_models(&self.models).map_err(|error| ApiError::Invalid(error.into()))
    }
}
impl ManagedProvidersClient {
    /// Discovers once; caller cancellation drops the waiter and never applies candidates.
    pub async fn discover(&self, request: DiscoveryRequest) -> Result<DiscoverySnapshot> {
        request.validate()?;
        let snapshot = match call_json::<_, DiscoverySnapshot, Never>(
            self.api.as_ref(),
            &ProvidersOperation::Discover.spec(),
            &request,
        )
        .await?
        {
            Ok(snapshot) => snapshot,
            Err(never) => match never {},
        };
        snapshot.validate_reply(&request)?;
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_replies_check_identity_and_candidate_semantics() {
        let request = DiscoveryRequest {
            provider: ProviderKind::Openai,
            endpoint: "https://api.openai.com".into(),
            slot: "default".into(),
        };
        request.validate().unwrap();
        let model = DiscoveredModel {
            id: "candidate".into(),
            name: None,
            context_window_tokens: None,
            max_output_tokens: None,
        };
        let valid = DiscoverySnapshot {
            request: request.clone(),
            models: vec![model.clone()],
        };
        valid.validate_reply(&request).unwrap();
        let mut stale = valid.clone();
        stale.request.endpoint = "https://another.invalid".into();
        assert!(stale.validate_reply(&request).is_err());
        DiscoverySnapshot {
            request: request.clone(),
            models: vec![DiscoveredModel {
                context_window_tokens: Some(10),
                max_output_tokens: Some(10),
                ..model.clone()
            }],
        }
        .validate_reply(&request)
        .expect("discovery reports capacity, not an execution reserve");
        let invalid = DiscoveredModel {
            context_window_tokens: Some(10),
            max_output_tokens: Some(11),
            ..model.clone()
        };
        for models in [
            vec![model.clone(), model.clone()],
            vec![invalid],
            vec![model; 4097],
        ] {
            let bytes = serde_json::to_vec(&DiscoverySnapshot {
                request: request.clone(),
                models,
            })
            .unwrap();
            let decoded: DiscoverySnapshot = serde_json::from_slice(&bytes).unwrap();
            assert!(decoded.validate_reply(&request).is_err());
        }
        DiscoverySnapshot {
            request: request.clone(),
            models: vec![],
        }
        .validate_reply(&request)
        .unwrap();
    }
}
