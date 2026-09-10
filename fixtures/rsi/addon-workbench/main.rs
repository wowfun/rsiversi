//! Minimal public embedder for actual workbench addon rendering evidence.
mod addon;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            eprintln!("fixture application failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
async fn run() -> Result<u8, Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let mode = arguments
        .next()
        .ok_or("explicit --profile or fixture-profile required")?;
    if mode == "fixture-profile" {
        println!("{}", addon::profile("A"));
        return Ok(0);
    }
    if mode != "--profile" {
        return Err("explicit --profile required".into());
    }
    let selected = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or("profile id required")?;
    let paths = rsi::standard_paths()?;
    let profile = rsi::ProfileCatalog::new(paths.clone())
        .application(&rsi::ApplicationProfileId::new(selected)?)?;
    let composition =
        rsi::StandardComposition::new(paths, rsi::capture_standard_environment()?, None)
            .with_addons(addon::addons(Arc::new(addon::Evidence::default())))
            .with_credential_store(Arc::new(EmptyStore));
    let (host, diagnostics) = rsi::standard_application_host(composition, arguments.collect())?;
    let running = host
        .start_program(profile.program()?)
        .await
        .map_err(|error| {
            diagnostics
                .take()
                .map_or_else(|| error.to_string(), |error| error.to_string())
        })?;
    let result = running
        .lookup_local::<rsi_application::ApplicationRunContract>()
        .ok_or("application entry required")?
        .run()
        .await;
    let cleanup = running.shutdown().await;
    if !cleanup.is_clean() {
        return Err(format!("unclean application shutdown: {cleanup:?}").into());
    }
    Ok(result?)
}

#[derive(Debug)]
struct EmptyStore;
impl rsi_credentials_local::SecretStore for EmptyStore {
    fn get(
        &self,
        _: &str,
        _: &str,
    ) -> rsi_credentials_protocol::Result<Option<rsi_credentials_protocol::SecretValue>> {
        Ok(None)
    }
    fn set(
        &self,
        _: &str,
        _: &str,
        _: &rsi_credentials_protocol::SecretValue,
    ) -> rsi_credentials_protocol::Result<()> {
        Err(rsi_credentials_protocol::CredentialsError::Store(
            "fixture keyring is disabled".into(),
        ))
    }
    fn unset(&self, _: &str, _: &str) -> rsi_credentials_protocol::Result<bool> {
        Err(rsi_credentials_protocol::CredentialsError::Store(
            "fixture keyring is disabled".into(),
        ))
    }
}
