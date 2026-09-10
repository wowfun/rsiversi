use crate::{RsiError, work::ApplicationWork};
use rsi_api_device_api::DeviceClient;
use rsi_api_protocol::{ApiClient, ApiError, DeviceId, DeviceRecord};
use serde::Serialize;
use std::{ffi::OsString, sync::Arc};

/// Terminal help for the Session-independent device administration application.
pub const HELP: &str = "Devices application:\n  rsi --profile devices register LABEL\n  rsi --profile devices list\n  rsi --profile devices revoke DEVICE_ID\nRegistration prints its one-time token; list prints only non-secret records.\n";

#[derive(Debug)]
pub(crate) enum Command {
    Register(String),
    List,
    Revoke(DeviceId),
}
impl Command {
    pub fn parse(arguments: &[OsString]) -> crate::Result<Self> {
        let arguments = arguments
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .ok_or_else(|| RsiError::Boot("device arguments must be UTF-8".into()))
            })
            .collect::<crate::Result<Vec<_>>>()?;
        match arguments.as_slice() {
            ["list"] => Ok(Self::List),
            ["register", label] => {
                DeviceRecord::validate_label(label)
                    .map_err(|error| RsiError::Boot(error.to_string()))?;
                Ok(Self::Register((*label).into()))
            }
            ["revoke", id] => Ok(Self::Revoke(
                DeviceId::parse(*id).map_err(|error| RsiError::Boot(error.to_string()))?,
            )),
            _ => Err(RsiError::Boot(HELP.into())),
        }
    }
    async fn execute(
        self,
        client: &DeviceClient,
    ) -> rsi_api_protocol::Result<zeroize::Zeroizing<Vec<u8>>> {
        let encoded = match self {
            Self::Register(label) => {
                #[derive(Serialize)]
                struct Receipt<'a> {
                    endpoint_id: &'a rsi_api_protocol::EndpointId,
                    id: &'a DeviceId,
                    label: &'a str,
                    token: &'a str,
                }
                let issued = client.register(&label).await?;
                serde_json::to_vec(&Receipt {
                    endpoint_id: client.endpoint_id(),
                    id: &issued.record.id,
                    label: &issued.record.label,
                    token: issued.token.expose_secret(),
                })
            }
            Self::List => serde_json::to_vec(&client.list().await?),
            Self::Revoke(id) => serde_json::to_vec(&client.revoke(&id).await?),
        }
        .map_err(|_| ApiError::Backend("cannot encode device result".into()))?;
        Ok(zeroize::Zeroizing::new(encoded))
    }
}

pub(crate) async fn run(api: Arc<dyn ApiClient>, command: Command, work: ApplicationWork) -> u8 {
    crate::document::run(
        async move {
            let client = DeviceClient::new(api)?;
            command.execute(&client).await
        },
        work,
        "device output delivery failed; reconcile with list",
    )
    .await
}
