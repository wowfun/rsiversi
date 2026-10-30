//! Explicit Local authorization for one managed Desktop Device in the visual fixture.
#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use rsi_automation_api::{Client, Request};
    use serde_json::json;
    use std::sync::Arc;

    let mut arguments = std::env::args().skip(1);
    let device: rsi_api_protocol::DeviceId =
        serde_json::from_value(json!(arguments.next().ok_or("Device identity required")?))?;
    if arguments.next().is_some() {
        return Err("expected exactly one Device identity".into());
    }
    let paths = rsi_service_host::ServiceHostPaths::from_host_paths(&rsi::standard_paths()?)?;
    let owner = paths.read_metadata()?.ok_or("missing isolated owner")?;
    if !rsi_service_host::owner_process_is_current(&owner)? {
        return Err("stale owner".into());
    }
    let compatibility = rsi_service_host::local_compatibility_key(&owner.launch_key)?;
    let api = Arc::new(
        rsi_api_uds_client::UdsClient::connect(
            rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            rsi_api_uds_client::UdsClientConfig {
                socket: owner.socket_path.ok_or("missing isolated socket")?,
                endpoint_id: owner.endpoint_id.ok_or("missing endpoint")?,
                host_epoch: owner.host_epoch,
                compatibility,
            },
        )
        .await?,
    );
    let client = Client::new(api.clone());
    let result = async {
        let mut policy = client.call(Request::Policy).await?;
        let revision = policy["revision"]
            .as_u64()
            .ok_or("invalid policy revision")?;
        policy["grants"]
            .as_array_mut()
            .ok_or("invalid policy grants")?
            .push(json!({"device":device,"source":"visual","rule":"visual",
                "view":true,"cancel":true,"resume":true}));
        Ok::<_, Box<dyn std::error::Error>>(
            client
                .call(Request::SetPolicy {
                    expected_revision: revision.to_string(),
                    policy,
                })
                .await?,
        )
    }
    .await;
    api.close().await;
    println!("{}", result?);
    Ok(())
}

#[cfg(not(unix))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("desktop-automation-grant requires the native Unix Service transport".into())
}
