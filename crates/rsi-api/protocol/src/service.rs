use crate::{
    ApiError, AuthenticatedDevice, ByteBudget, ByteReservation, MAXIMUM_API_BYTES, OperationClass,
    OperationEffect, OperationId, Result, RetainedBytes,
};
use async_trait::async_trait;
use futures_util::{Stream, future::BoxFuture};
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use std::{fmt, pin::Pin, sync::Arc};
use tokio_util::sync::CancellationToken;

/// Request body representation selected by the operation owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestEncoding {
    /// Closed domain JSON, decoded by Rust at its owning boundary.
    Json,
    /// Bounded binary source with no domain JSON envelope.
    Binary,
}

/// Caller authority selected by the registered operation owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationAccess {
    /// Any authenticated device or trusted local caller.
    Authenticated,
    /// Trusted local callers only; device credentials grant no authority.
    Local,
}

impl OperationAccess {
    /// Checks the trusted transport origin, never an untrusted request field.
    pub fn permits(self, origin: &CallOrigin) -> bool {
        self == Self::Authenticated || matches!(origin, CallOrigin::Local)
    }
}

/// Immutable dispatch and resource policy registered by a domain plugin.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationSpec {
    /// Exact owning domain, operation name and version.
    pub id: OperationId,
    /// Caller authority required before acquiring a resource lane.
    pub access: OperationAccess,
    /// Resource lane selected by this registered operation.
    pub class: OperationClass,
    /// Whether admitted work survives response-waiter cancellation.
    pub effect: OperationEffect,
    /// Request representation accepted at the transport boundary.
    pub encoding: RequestEncoding,
    /// Maximum retained input bytes, admitted before allocation.
    pub maximum_request_bytes: usize,
    /// Maximum finite response or individual stream item bytes.
    pub maximum_response_bytes: usize,
}

impl OperationSpec {
    /// Rejects contradictory ownership or impossible configured reservations.
    pub fn validate(&self) -> Result<()> {
        if self.maximum_request_bytes > MAXIMUM_API_BYTES
            || self.maximum_response_bytes > MAXIMUM_API_BYTES
        {
            return Err(ApiError::Invalid(
                "operation bytes exceed the 64 MiB API ceiling".into(),
            ));
        }
        if self.class == OperationClass::Subscription && self.effect != OperationEffect::Read {
            return Err(ApiError::Invalid(
                "subscriptions must be read operations".into(),
            ));
        }
        if self.class == OperationClass::Control
            && (self.maximum_request_bytes > 128 * 1024
                || self.maximum_response_bytes > 128 * 1024
                || self.encoding != RequestEncoding::Json)
        {
            return Err(ApiError::Invalid(
                "controls require JSON and at most 128 KiB per payload".into(),
            ));
        }
        Ok(())
    }
}

/// Trusted origin supplied by connection authentication, never decoded from a body.
#[derive(Clone, Debug)]
pub enum CallOrigin {
    /// Explicit trusted process-local or same-user transport use.
    Local,
    /// Authenticated device identity for per-device admission.
    Device(AuthenticatedDevice),
}

/// Invocation context supplied to a domain handler after admission.
#[derive(Clone, Debug)]
pub struct ApiContext {
    /// Authenticated caller used for domain-specific ownership policy.
    pub origin: CallOrigin,
    /// Registration retirement signal; mutation ownership is independent of waiter drop.
    pub retiring: CancellationToken,
}

/// Response admission transferred to a handler before finite work begins.
#[derive(Debug)]
pub enum ApiResponseCapacity {
    /// Reserved mutation/receiving storage or measured read delivery capacity.
    Finite(FiniteResponseCapacity),
    /// Each item must acquire its maximum before reading or materializing domain state.
    Subscription {
        /// Retention budget shared by all subscriptions in this dispatcher.
        budget: ByteBudget,
        /// Registered maximum for one complete item, including binary bytes.
        maximum: usize,
    },
}

/// Finite response storage. Reads admit exact payload storage after measuring;
/// mutations and incoming responses can carry an already acquired maximum.
#[derive(Debug)]
pub enum FiniteResponseCapacity {
    /// Ownership acquired before work or reception starts.
    Reserved(ByteReservation),
    /// Per-response ceiling and shared pool, without speculative allocation.
    Measured {
        /// Shared retained-response pool.
        budget: ByteBudget,
        /// Remaining maximum, including binary and JSON storage.
        maximum: usize,
    },
}
impl From<ByteReservation> for FiniteResponseCapacity {
    fn from(value: ByteReservation) -> Self {
        Self::Reserved(value)
    }
}
impl FiniteResponseCapacity {
    /// Acquires the entire remaining ceiling before unknown-length reception.
    pub fn reserve(self) -> Result<ByteReservation> {
        match self {
            Self::Reserved(reservation) => Ok(reservation),
            Self::Measured { budget, maximum } => budget.reserve(maximum),
        }
    }
    /// Admits a known binary part and removes it from the remaining response ceiling.
    pub fn split(&mut self, bytes: usize) -> Result<ByteReservation> {
        match self {
            Self::Reserved(reservation) => reservation.split(bytes),
            Self::Measured { budget, maximum } => {
                if bytes > *maximum {
                    return Err(ApiError::Invalid("split exceeds response maximum".into()));
                }
                let reservation = budget.reserve(bytes)?;
                *maximum -= bytes;
                Ok(reservation)
            }
        }
    }
    /// Measures JSON before reserving and allocating its exact response storage.
    pub fn encode<T: serde::Serialize + ?Sized>(self, value: &T) -> Result<RetainedBytes> {
        match self {
            Self::Reserved(reservation) => reservation.encode(value),
            Self::Measured { budget, maximum } => budget.encode(value, maximum),
        }
    }
    /// Admits a known payload before copying it into retained response storage.
    pub fn copy(self, bytes: &[u8]) -> Result<RetainedBytes> {
        match self {
            Self::Reserved(reservation) => reservation.copy(bytes),
            Self::Measured { budget, maximum } => {
                if bytes.len() > maximum {
                    return Err(ApiError::Invalid("payload exceeds response maximum".into()));
                }
                budget.copy(bytes)
            }
        }
    }
}

/// JSON metadata and optional binary bytes, each retaining its own budget owner.
#[derive(Clone, Debug)]
pub struct ApiMessage {
    /// Exact Rust-encoded domain JSON.
    pub json: RetainedBytes,
    /// Optional binary body described by the JSON metadata.
    pub binary: Option<RetainedBytes>,
}
impl ApiMessage {
    /// Returns total encoded payload bytes without releasing either owner.
    pub fn encoded_len(&self) -> usize {
        self.json
            .len()
            .saturating_add(self.binary.as_ref().map_or(0, RetainedBytes::len))
    }
}

/// Reconnectable stream of bounded domain items; a transport must send an explicit end.
pub type ApiStream = Pin<Box<dyn Stream<Item = Result<ApiMessage>> + Send + 'static>>;

/// One finite reply or one owned subscription.
pub enum ApiOutput {
    /// Finite data whose byte owners survive completion of the call slot.
    Reply(ApiMessage),
    /// Stream whose work/admission lasts until termination or drop.
    Stream(ApiStream),
}
impl fmt::Debug for ApiOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reply(message) => formatter.debug_tuple("Reply").field(message).finish(),
            Self::Stream(_) => formatter.write_str("Stream(..)"),
        }
    }
}

/// Domain-owned request decoder and behavior, independent of a wire adapter.
#[async_trait]
pub trait ApiHandler: fmt::Debug + Send + Sync + 'static {
    /// Uses the supplied reservation for finite work, or reserves before each stream item.
    async fn invoke(
        &self,
        context: ApiContext,
        input: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput>;
}

/// An admitted operation whose resource owner transfers into its invocation.
pub trait ApiInvocation: fmt::Debug + Send + 'static {
    /// Returns the exact registered resource and request contract.
    fn spec(&self) -> &OperationSpec;
    /// Clones the input lane's budget so adapters reserve before receiving bytes.
    fn input_budget(&self) -> ByteBudget;
    /// Registration retirement signal for cancelling incomplete request-body reception.
    fn retiring(&self) -> CancellationToken;
    /// Retains quota through transport delivery without pinning domain work.
    fn retain_admission(&self) -> ApiAdmission;
    /// Transfers work ownership now, before its returned response waiter is polled.
    fn invoke(self: Box<Self>, input: RetainedBytes) -> BoxFuture<'static, Result<ApiOutput>>;
}

/// A transport-held class/device admission lease, independent of domain retirement.
#[derive(Clone)]
pub struct ApiAdmission(Arc<dyn fmt::Debug + Send + Sync>);
impl ApiAdmission {
    /// Wraps the dispatcher's quota owner for retention by an admitted transport.
    pub fn new(owner: Arc<impl fmt::Debug + Send + Sync + 'static>) -> Self {
        Self(owner)
    }
}
impl fmt::Debug for ApiAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApiAdmission")
            .field("holders", &Arc::strong_count(&self.0))
            .finish_non_exhaustive()
    }
}

/// Transport-independent lookup and admission for trusted connections.
pub trait ApiDispatch: fmt::Debug + Send + Sync + 'static {
    /// Admits using registered metadata and an authenticated origin.
    fn admit(&self, operation: &OperationId, origin: CallOrigin) -> Result<Box<dyn ApiInvocation>>;
    /// Returns the bounded currently registered domain contracts for negotiation.
    fn operations(&self) -> Vec<OperationSpec>;
}

/// Domain-registration lifetime implemented by a registry provider.
#[async_trait]
pub trait RegistrationControl: fmt::Debug + Send + Sync + 'static {
    /// Fences new work immediately and cancels read/stream owners.
    fn retire(&self);
    /// Waits for already owned work to settle.
    async fn drain(&self);
}

/// Unique effect lease for a domain's registered operation.
#[derive(Debug)]
pub struct ApiRegistration(Arc<dyn RegistrationControl>);
impl ApiRegistration {
    /// Constructs a lease from its registry-owned retirement authority.
    pub fn new(control: Arc<dyn RegistrationControl>) -> Self {
        Self(control)
    }
    /// Retires immediately, then drains work before the domain's dependencies retire.
    pub async fn close(self) {
        self.0.retire();
        self.0.drain().await;
    }
}
impl Drop for ApiRegistration {
    fn drop(&mut self) {
        self.0.retire();
    }
}

/// Write-only operation registration used by ordinary domain plugins.
pub trait ApiRegistrar: fmt::Debug + Send + Sync + 'static {
    /// Registers one validated descriptor until its effect lease retires.
    fn register(
        &self,
        spec: OperationSpec,
        handler: Arc<dyn ApiHandler>,
    ) -> Result<ApiRegistration>;
}

/// Nominal Local contract for domain registration.
#[derive(Debug)]
pub struct ApiRegistrarContract;
impl LocalContract for ApiRegistrarContract {
    const KEY: &'static str = "rsi.api.registrar";
    type Service = dyn ApiRegistrar;
}
/// Nominal Local contract for transport admission and dispatch.
#[derive(Debug)]
pub struct ApiDispatchContract;
impl LocalContract for ApiDispatchContract {
    const KEY: &'static str = "rsi.api.dispatch";
    type Service = dyn ApiDispatch;
}
