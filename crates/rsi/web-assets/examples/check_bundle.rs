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
    let family = match arguments.next() {
        Some(flag) if flag == "--write-pairing" => Some(
            arguments
                .next()
                .ok_or("missing family")?
                .into_string()
                .map_err(|_| "family must be UTF-8")?,
        ),
        Some(_) => return Err("unknown option".into()),
        None => None,
    };
    let bridge = if family.is_some() {
        Some(
            serde_json::from_str::<rsi_api_protocol::ProtocolDescriptor>(
                &arguments
                    .next()
                    .ok_or("missing built Desktop bridge descriptor JSON")?
                    .into_string()
                    .map_err(|_| "descriptor must be UTF-8")?,
            )?,
        )
    } else {
        None
    };
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Document {
        files: Vec<String>,
        trusted_inline_styles: bool,
    }
    let document = match arguments.next() {
        Some(flag) if flag == "--document" => {
            use std::io::Read as _;
            let mut bytes = Vec::new();
            std::fs::File::open(PathBuf::from(
                arguments.next().ok_or("missing document JSON path")?,
            ))?
            .take(32 * 1024 + 1)
            .read_to_end(&mut bytes)?;
            if bytes.len() > 32 * 1024 {
                return Err("document JSON exceeds 32 KiB".into());
            }
            Some(serde_json::from_slice::<Document>(&bytes)?)
        }
        Some(_) => return Err("unknown option".into()),
        None => None,
    };
    if !directory.is_absolute() || arguments.next().is_some() {
        return Err("expected one absolute bundle directory".into());
    }
    if let Some(family) = family {
        let receipt = rsi_web_assets::pairing_receipt_with_document(
            &directory,
            &family,
            bridge.expect("producer descriptor"),
            document
                .as_ref()
                .map(|d| (d.files.clone(), d.trusted_inline_styles)),
        )?;
        std::fs::write(directory.join("rsi-build.json"), receipt)?;
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
            if let Some(document) = document { serde_json::json!({"directory":directory,"files":document.files,"trusted_inline_styles":document.trusted_inline_styles}) }
            else { serde_json::json!({"directory": directory}) },
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
