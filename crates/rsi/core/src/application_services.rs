//! Local product authorities consumed by native application plugins.
use async_trait::async_trait;
use rsi_meta::LocalContract;
use std::fmt;

/// Active local Host metadata, without backend ownership or preset evaluation.
#[derive(Debug)]
pub struct RemoteServiceContract;
impl LocalContract for RemoteServiceContract {
    const KEY: &'static str = "rsi.service.remote";
    type Service = rsi_service_host::HostOwnerMetadata;
}

/// One ACP attachment. Product launchers provide a remote stream; embedders may supply a backend.
#[derive(Clone, Debug)]
pub enum AcpAttachment {
    /// Connected stream after native peer and descriptor admission.
    Remote(std::sync::Arc<dyn rsi_acp::Transport>),
    /// Explicit in-process embedding.
    Backend(std::sync::Arc<dyn rsi_acp::server::AgentBackend>),
}
/// ACP application transport or explicitly embedded backend.
#[derive(Debug)]
pub struct AcpAttachmentContract;
impl LocalContract for AcpAttachmentContract {
    const KEY: &'static str = "rsi.acp.attachment";
    type Service = AcpAttachment;
}

/// Observation and reload of the service generation selected by the composition.
#[async_trait]
pub trait ServingService: fmt::Debug + Send + Sync + 'static {
    /// Waits for service retirement or failure without owning its shutdown authority.
    async fn stopped(&self) -> Result<(), String>;
    /// Requests a Profile reload; dropping the waiter must not own service shutdown.
    async fn reload(&self) -> Result<(), String>;
}

/// Composition-provided control for the independently owned service generation.
#[derive(Debug)]
pub struct ServingServiceContract;
impl LocalContract for ServingServiceContract {
    const KEY: &'static str = "rsi.serve.service";
    type Service = dyn ServingService;
}

/// Product-owned Local authority that rotates a managed device and grants configuration.
#[async_trait]
pub trait LocalBrowserAdministration: std::fmt::Debug + Send + Sync {
    /// Completes durable credential rotation and configuration authorization once per
    /// Service owner. Repeated or concurrent launches return that same credential;
    /// revocation requires restarting the owner before a new launch can authorize.
    async fn launch(&self) -> rsi_api_protocol::Result<rsi_api_protocol::RegisteredDevice>;
}
/// Local-only product launch authority, never exported to HTTP callers.
#[derive(Debug)]
pub struct LocalBrowserAdministrationContract;
impl LocalContract for LocalBrowserAdministrationContract {
    const KEY: &'static str = "rsi.web.local-administration";
    type Service = dyn LocalBrowserAdministration;
}
