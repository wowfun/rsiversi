use rsi_api_http::HttpAssetsContract;
use rsi_host::{HostBuilder, Profile, ProfileEntry};
use rsi_meta::UpdateMode;
use rsi_web_assets::{WebAssetControlContract, WebAssetsFactory};
use std::{path::PathBuf, sync::Arc};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let directory = PathBuf::from(
        arguments
            .next()
            .ok_or("expected absolute bundle directory")?,
    );
    if !directory.is_absolute() || arguments.next().is_some() {
        return Err("expected one absolute bundle directory".into());
    }
    let mut host = HostBuilder::without_paths("bundle-check");
    host.register_local_contract::<HttpAssetsContract>()?;
    host.register_local_contract::<WebAssetControlContract>()?;
    host.register_linked(
        "assets",
        "1",
        UpdateMode::RestartRequired,
        Arc::new(WebAssetsFactory),
    )?;
    let running = host
        .build()?
        .start(Profile::new(vec![ProfileEntry::new(
            "assets",
            "assets",
            serde_json::json!({"directory": directory}),
        )]))
        .await?;
    let bytes = running
        .lookup_local::<WebAssetControlContract>()
        .ok_or("asset control missing")?
        .retained_bytes();
    if !running.shutdown().await.is_clean() {
        return Err("bundle preflight cleanup failed".into());
    }
    println!(
        "{}",
        serde_json::json!({"event":"web-assets-admitted", "bytes":bytes})
    );
    Ok(())
}
