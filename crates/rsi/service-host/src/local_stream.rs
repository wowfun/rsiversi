use crate::{HostOwnerLease, HostOwnerMetadata, HostOwnerMode, ServiceHostError, owner::io_error};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

/// Authenticated byte stream; the consuming plugin owns its wire protocol.
pub trait LocalStream: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug> LocalStream for T {}

/// Native listener retaining the two-home authority for its entire publication.
#[derive(Debug)]
pub struct LocalServiceListener {
    #[cfg(unix)]
    listener: tokio::net::UnixListener,
    #[cfg(unix)]
    _publication: crate::socket::PublishedSocket,
    #[cfg(windows)]
    listener: rsi_api_local_transport::PipeListener,
    _owner: Arc<HostOwnerLease>,
}
impl LocalServiceListener {
    /// Binds a named sibling after the product has acquired both homes.
    pub fn bind(owner: Arc<HostOwnerLease>, name: &str) -> Result<Self, ServiceHostError> {
        let paths = owner.paths().sibling(name)?;
        #[cfg(unix)]
        let (listener, publication) = crate::socket::PublishedSocket::bind(&paths)?;
        #[cfg(windows)]
        let listener = rsi_api_local_transport::PipeListener::bind(
            rsi_api_local_transport::PipeName::new(paths.socket()).map_err(io_error)?,
        )
        .map_err(io_error)?;
        Ok(Self {
            listener,
            #[cfg(unix)]
            _publication: publication,
            _owner: owner,
        })
    }
    /// Accepts an authenticated same-user peer; errors confer no stream authority.
    pub async fn accept(&self) -> Result<Box<dyn LocalStream>, ServiceHostError> {
        #[cfg(unix)]
        let stream = {
            let (stream, _) = self.listener.accept().await.map_err(io_error)?;
            if stream.peer_cred().map_err(io_error)?.uid() != rustix::process::geteuid().as_raw() {
                return Err(ServiceHostError::Invalid("foreign local peer".into()));
            }
            stream
        };
        #[cfg(windows)]
        let stream = self.listener.accept().await.map_err(io_error)?;
        Ok(Box::new(stream))
    }
}

/// Connects to a recorded generation and rechecks its native process identity.
pub async fn connect_local_service(
    owner: &HostOwnerMetadata,
    name: &str,
) -> Result<Box<dyn LocalStream>, ServiceHostError> {
    owner.validate()?;
    if owner.mode != HostOwnerMode::Daemon || !crate::owner_process_is_current(owner)? {
        return Err(ServiceHostError::Invalid(
            "local owner is no longer current".into(),
        ));
    }
    let endpoint = crate::owner::sibling_endpoint(
        owner
            .socket_path
            .as_deref()
            .ok_or_else(|| ServiceHostError::Invalid("missing local endpoint".into()))?,
        name,
    )?;
    #[cfg(unix)]
    let stream = {
        let stream = tokio::net::UnixStream::connect(&endpoint)
            .await
            .map_err(io_error)?;
        let peer = stream.peer_cred().map_err(io_error)?;
        if peer.uid() != rustix::process::geteuid().as_raw()
            || peer.pid().and_then(|pid| u32::try_from(pid).ok()) != Some(owner.pid)
            || !crate::owner_process_is_current(owner)?
        {
            return Err(ServiceHostError::Invalid(
                "local server generation differs".into(),
            ));
        }
        stream
    };
    #[cfg(windows)]
    let stream = {
        let current =
            rsi_process_native::ProcessHandle::open(std::process::id()).map_err(io_error)?;
        let expected = rsi_process_native::ProcessIdentity {
            pid: owner.pid,
            start_token: owner.process_start_token.clone(),
            user_sid: current.identity().user_sid.clone(),
        };
        rsi_api_local_transport::connect(
            &rsi_api_local_transport::PipeName::new(&endpoint).map_err(io_error)?,
            &expected,
        )
        .await
        .map_err(io_error)?
    };
    Ok(Box::new(stream))
}
