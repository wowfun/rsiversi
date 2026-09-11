use crate::{
    ApiError, ApiOutput, ByteBudget, EndpointId, HostEpoch, OperationClass, OperationEffect,
    OperationId, OperationSpec, RequestEncoding, Result, RetainedBytes,
};
use async_trait::async_trait;
use rsi_meta_contract::LocalContract;
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, SeqAccess, Visitor},
};
use std::{collections::BTreeSet, fmt};

/// Maximum negotiated operations in one connection generation.
pub const MAXIMUM_OPERATIONS: usize = 2048;

/// Closed connection negotiation request, before any generation-specific domain call.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionHello {
    /// Exact framing version required by this client.
    pub wire_version: u16,
    /// Expected persisted deployment identity, when already selected by the operator.
    #[serde(default)]
    pub expected_endpoint: Option<EndpointId>,
}

/// Closed description of the deployment and running connection generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionDescription {
    /// Exact framing version, independently checked by the connecting client.
    pub wire_version: u16,
    /// Persisted deployment identity.
    pub endpoint_id: EndpointId,
    /// Current running generation.
    pub host_epoch: HostEpoch,
}

/// Non-secret caller identity selected by trusted connection authentication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CallerIdentity {
    /// Trusted local invocation; never selectable by a remote request.
    Local,
    /// Authenticated device invocation.
    Device {
        /// Exact non-secret device registration identity.
        device_id: crate::DeviceId,
    },
}

/// Published identity of the independently owned connection API registrations.
#[derive(Debug)]
pub struct ConnectionDescriptionContract;
impl LocalContract for ConnectionDescriptionContract {
    const KEY: &'static str = "rsi.api.connection.description";
    type Service = ConnectionDescription;
}

/// Bounded, duplicate-free operation descriptors belonging to one negotiation.
#[derive(Clone, Debug, Serialize)]
#[serde(transparent)]
pub struct OperationCatalog(Vec<OperationSpec>);
impl OperationCatalog {
    /// Validates trusted typed descriptors before publishing a catalog.
    pub fn new(operations: Vec<OperationSpec>) -> Result<Self> {
        if operations.len() > MAXIMUM_OPERATIONS {
            return Err(ApiError::Capacity);
        }
        let mut seen = BTreeSet::new();
        for operation in &operations {
            operation.validate()?;
            if !seen.insert(&operation.id) {
                return Err(ApiError::Invalid("duplicate operation in catalog".into()));
            }
        }
        Ok(Self(operations))
    }
    /// Borrows exact negotiated descriptors without allocating another catalog.
    pub fn operations(&self) -> &[OperationSpec] {
        &self.0
    }
}
impl<'de> Deserialize<'de> for OperationCatalog {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct CatalogVisitor;
        impl<'de> Visitor<'de> for CatalogVisitor {
            type Value = OperationCatalog;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("at most 2048 unique operation descriptors")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut operations = Vec::new();
                while operations.len() < MAXIMUM_OPERATIONS {
                    let Some(operation) = sequence.next_element::<OperationSpec>()? else {
                        return OperationCatalog::new(operations).map_err(de::Error::custom);
                    };
                    operation.validate().map_err(de::Error::custom)?;
                    operations.push(operation);
                }
                if sequence.next_element::<de::IgnoredAny>()?.is_some() {
                    return Err(de::Error::custom("operation catalog exceeds 2048 entries"));
                }
                OperationCatalog::new(operations).map_err(de::Error::custom)
            }
        }
        deserializer.deserialize_seq(CatalogVisitor)
    }
}

/// Resource contract for connection negotiation, shared by the endpoint and client.
pub fn describe_operation() -> OperationSpec {
    connection_operation("describe", OperationClass::Control, 1024)
}
/// Resource contract for generation-specific operation discovery.
pub fn operations_operation() -> OperationSpec {
    connection_operation("operations", OperationClass::Data, 1024 * 1024)
}
/// Bounded authenticated discovery of the current invocation's principal.
pub fn caller_operation() -> OperationSpec {
    connection_operation("caller", OperationClass::Control, 1024)
}
fn connection_operation(
    name: &str,
    class: OperationClass,
    maximum_response_bytes: usize,
) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("connection", name, 1).expect("constant operation"),
        class,
        effect: OperationEffect::Read,
        access: crate::OperationAccess::Authenticated,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 1024,
        maximum_response_bytes,
    }
}

/// Domain-independent client capability for one negotiated connection generation.
#[async_trait]
pub trait ApiClient: fmt::Debug + Send + Sync + 'static {
    /// Returns the deployment and generation established before publishing this client.
    fn description(&self) -> &ConnectionDescription;
    /// Borrows the bounded exact operation descriptors advertised by this generation.
    fn operations(&self) -> &[OperationSpec];
    /// Supplies the input lane's budget to reserve before encoding or reading a source.
    fn input_budget(&self, class: OperationClass) -> ByteBudget;
    /// Requires an exact negotiated descriptor and never automatically replays a mutation.
    async fn call(&self, operation: &OperationSpec, input: RetainedBytes) -> Result<ApiOutput>;
}

/// Nominal Local contract consumed by domain client plugins.
#[derive(Debug)]
pub struct ApiClientContract;
impl LocalContract for ApiClientContract {
    const KEY: &'static str = "rsi.api.client";
    type Service = dyn ApiClient;
}
