//! Fixture-only Local grant setup through the public API, never the storage backend.
use rsi_configuration_api::leaf::{CatalogRequest, Client, SetGrant};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{io::Read, sync::Arc};

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Catalog { query: CatalogRequest },
    Grants,
    SetGrant { request: SetGrant },
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = Vec::new();
    std::io::stdin()
        .take(128 * 1024 + 1)
        .read_to_end(&mut input)?;
    if input.len() > 128 * 1024 {
        return Err("fixture request too large".into());
    }
    let request: Request = serde_json::from_slice(&input)?;
    let paths = rsi_service_host::ServiceHostPaths::from_host_paths(&rsi::standard_paths()?)?;
    let owner = paths.read_metadata()?.ok_or("missing isolated owner")?;
    if !rsi_service_host::owner_process_is_current(&owner)? {
        return Err("stale owner".into());
    }
    let mut key = Sha256::new();
    key.update(b"rsi.local.api.v1\0");
    key.update(owner.launch_key.as_bytes());
    key.update(owner.product_build.as_bytes());
    let api = Arc::new(
        rsi_api_uds_client::UdsClient::connect(
            rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            rsi_api_uds_client::UdsClientConfig {
                socket: owner.socket_path.ok_or("missing isolated socket")?,
                endpoint_id: owner.endpoint_id.ok_or("missing endpoint")?,
                host_epoch: owner.host_epoch,
                compatibility: rsi_api_protocol::LocalCompatibilityKey::from_bytes(
                    key.finalize().into(),
                ),
            },
        )
        .await?,
    );
    let client = Client::new(api.clone())?;
    let result = async {
        Ok::<_, Box<dyn std::error::Error>>(match request {
            Request::Catalog { query } => serde_json::to_value(client.catalog(query).await?)?,
            Request::Grants => serde_json::to_value(client.grants().await?)?,
            Request::SetGrant { request } => {
                serde_json::to_value(client.set_grant(request).await?)?
            }
        })
    }
    .await;
    api.close().await;
    println!("{}", serde_json::to_string(&result?)?);
    Ok(())
}
