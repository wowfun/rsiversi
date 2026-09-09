use crate::{RsiError, work::ApplicationWork};
use rsi_api_protocol::{ApiClient, ApiError};
use rsi_inspector::{InspectorClient, PageRequest, RuntimeRequest};
use std::{ffi::OsString, sync::Arc};

/// Finite local operator inspection grammar.
pub const HELP: &str = "Inspector application (existing local Service Host):\n  rsi --profile inspector runtime [AFTER_FIBER]\n  rsi --profile inspector profile [OFFSET]\n  rsi --profile inspector factories [OFFSET]\n  rsi --profile inspector native\nPrints one bounded JSON page; use its next_after or next_offset for another page.\n";
#[derive(Debug)]
pub(crate) enum Command {
    Runtime(RuntimeRequest),
    Profile(PageRequest),
    Factories(PageRequest),
    Native,
}
impl Command {
    pub fn parse(arguments: &[OsString]) -> crate::Result<Self> {
        let invalid = || RsiError::Boot(HELP.into());
        let values = arguments
            .iter()
            .map(|value| value.to_str().ok_or_else(invalid))
            .collect::<crate::Result<Vec<_>>>()?;
        match values.as_slice() {
            ["runtime", tail @ ..] if tail.len() <= 1 => {
                let request = RuntimeRequest {
                    after: tail.first().map(|value| (*value).into()),
                    ..Default::default()
                };
                request.validate().map_err(|_| invalid())?;
                Ok(Self::Runtime(request))
            }
            [kind @ ("profile" | "factories"), tail @ ..] if tail.len() <= 1 => {
                let offset = tail
                    .first()
                    .map(|value| {
                        let number: usize = value.parse().map_err(|_| invalid())?;
                        if *value != number.to_string() {
                            return Err(invalid());
                        }
                        Ok(number)
                    })
                    .transpose()?
                    .unwrap_or(0);
                let request = PageRequest {
                    offset,
                    ..Default::default()
                };
                request.validate().map_err(|_| invalid())?;
                Ok(if *kind == "profile" {
                    Self::Profile(request)
                } else {
                    Self::Factories(request)
                })
            }
            ["native"] => Ok(Self::Native),
            _ => Err(invalid()),
        }
    }
}
pub(crate) async fn run(api: Arc<dyn ApiClient>, command: Command, work: ApplicationWork) -> u8 {
    crate::document::run(
        async move {
            let client = InspectorClient::new(api)?;
            let document = match command {
                Command::Runtime(request) => client.runtime(&request).await?,
                Command::Profile(request) => client.profile(&request).await?,
                Command::Factories(request) => client.factories(&request).await?,
                Command::Native => client.native().await?,
            };
            serde_json::to_vec(&document)
                .map(zeroize::Zeroizing::new)
                .map_err(|_| ApiError::Backend("cannot encode Inspector result".into()))
        },
        work,
        "Inspector output delivery failed; repeat the read",
    )
    .await
}
