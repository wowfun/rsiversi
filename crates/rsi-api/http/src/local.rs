use crate::{
    access::Access,
    delivery::Delivery,
    server::{State, serve_connection},
};
use rsi_api_protocol::{
    ApiDispatch, ApiError, ConnectionDescription, LocalCompatibilityKey, Result,
};
use rsi_meta::Execution;
use std::sync::Arc;
use tokio::{net::UnixStream, sync::Semaphore};
use tokio_util::sync::CancellationToken;

/// Same-UID Unix service using the shared API HTTP codec and request supervision.
/// The deployment owner retains socket publication and supplies its opaque local gate.
#[derive(Clone, Debug)]
pub struct LocalHttpService {
    state: Arc<State>,
    connections: Arc<Semaphore>,
    uid: u32,
}
impl LocalHttpService {
    /// Pins one dispatcher and connection identity without owning their registrations.
    pub fn new(
        execution: Execution,
        dispatch: Arc<dyn ApiDispatch>,
        description: &ConnectionDescription,
        compatibility: LocalCompatibilityKey,
    ) -> Result<Self> {
        if description.wire_version != 1 {
            return Err(ApiError::Unavailable);
        }
        Ok(Self {
            state: Arc::new(State {
                assets: None,
                asset_deliveries: Arc::new(Semaphore::new(8)),
                diagnostics: crate::HttpDiagnostics::default(),
                dispatch,
                endpoint: description.endpoint_id.clone(),
                epoch: description.host_epoch.clone(),
                access: Access::Local(compatibility),
                execution,
                unclassified: Arc::new(Semaphore::new(32)),
            }),
            connections: Arc::new(Semaphore::new(128)),
            uid: rustix::process::geteuid().as_raw(),
        })
    }

    /// Verifies the actual peer, admits one connection and serves exactly one exchange.
    /// Dropping this waiter closes transport delivery; admitted mutations retain API ownership.
    pub async fn serve(&self, stream: UnixStream, stop: CancellationToken) -> Result<()> {
        if stream
            .peer_cred()
            .map_err(|_| ApiError::Unauthorized)?
            .uid()
            != self.uid
        {
            return Err(ApiError::Unauthorized);
        }
        let _connection = self
            .connections
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let handshake = self
            .state
            .unclassified
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        serve_connection(
            stream,
            self.state.clone(),
            stop,
            Delivery::new(handshake, None),
        )
        .await;
        Ok(())
    }

    /// Observes the shared HTTP protocol and I/O boundary independently of publication.
    pub fn diagnostics(&self) -> crate::HttpDiagnostics {
        self.state.diagnostics.clone()
    }
}
