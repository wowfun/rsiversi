use crate::{RsiError, work::ApplicationWork};
use rsi_api_protocol::{ApiClient, ApiError};
use rsi_native_addons_api::NativeAddonsClient;
use std::{ffi::OsString, sync::Arc};

/// Explicit staging grammar, without source installation or Host startup.
pub const HELP: &str = "Native addons application (existing local Service Host):\n  rsi --profile addons refresh\n  rsi addon refresh\nPrints a staging receipt; existing Sessions keep their pinned generation.\n";
#[derive(Debug)]
pub(crate) enum Command {
    Refresh,
}
impl Command {
    pub(crate) fn parse(arguments: &[OsString]) -> crate::Result<Self> {
        match arguments {
            [value] if value == "refresh" => Ok(Self::Refresh),
            _ => Err(RsiError::Boot(HELP.into())),
        }
    }
}
pub(crate) async fn run(api: Arc<dyn ApiClient>, command: Command, work: ApplicationWork) -> u8 {
    crate::document::run(
        async move {
            let client = NativeAddonsClient::new(api)?;
            let receipt = match command {
                Command::Refresh => client.refresh().await?,
            }.map_err(|error| ApiError::Backend(error.to_string()))?;
            serde_json::to_vec(&receipt)
                .map(zeroize::Zeroizing::new)
                .map_err(|_| ApiError::Backend("cannot encode native staging receipt".into()))
        },
        work,
        "Native refresh result delivery failed; inspect native state before another explicit attempt",
    ).await
}
