use crate::TlsFiles;
use rsi_api_protocol::{ApiError, Result};
use std::{path::Path, sync::Arc};
use tokio::io::AsyncReadExt;
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    },
};
use zeroize::Zeroizing;

pub(crate) async fn acceptor(files: &TlsFiles) -> Result<TlsAcceptor> {
    let certificates = read(&files.certificate).await?;
    let key = read(&files.key).await?;
    let certificates = CertificateDer::pem_slice_iter(&certificates)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| ApiError::Invalid("invalid TLS certificate chain".into()))?;
    let key = PrivateKeyDer::from_pem_slice(&key)
        .map_err(|_| ApiError::Invalid("invalid TLS private key".into()))?;
    let provider = tokio_rustls::rustls::crypto::ring::default_provider();
    let mut config = ServerConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|_| ApiError::Invalid("unsupported TLS versions".into()))?
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|_| ApiError::Invalid("TLS certificate and key do not match".into()))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    config.max_fragment_size = Some(16 * 1024);
    Ok(TlsAcceptor::from(Arc::new(config)))
}

async fn read(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    let file = options
        .open(path)
        .await
        .map_err(|_| ApiError::Invalid("cannot open TLS asset".into()))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|_| ApiError::Invalid("cannot inspect TLS asset".into()))?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err(ApiError::Invalid(
            "TLS asset must be a regular file of at most 1 MiB".into(),
        ));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ApiError::Invalid("cannot read TLS asset".into()))?;
    if bytes.len() > 1024 * 1024 {
        return Err(ApiError::Invalid("TLS asset grew beyond its bound".into()));
    }
    Ok(bytes)
}
