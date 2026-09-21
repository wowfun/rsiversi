use rsi_acp_protocol::{
    observation::ConversationId,
    service::{Error, Result},
};
use rsi_credentials_protocol::CredentialRef;
use rsi_sandbox::SandboxMode;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    pub directory: PathBuf,
    #[serde(default)]
    pub endpoints: Vec<EndpointConfig>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EndpointConfig {
    pub id: String,
    #[serde(default)]
    pub enabled: bool,
    pub cwd: PathBuf,
    pub sandbox: SandboxMode,
    pub launch: Launch,
    #[serde(default)]
    pub mcp_servers: Vec<McpConfig>,
    #[serde(default)]
    pub session_options: Vec<rsi_acp_protocol::configuration::ConfigSelection>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpConfig {
    pub name: String,
    pub launch: Launch,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Launch {
    pub program: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, Environment>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Environment {
    Literal { value: String },
    Credential { reference: CredentialRef },
}
fn path(value: &Path) -> bool {
    value.is_absolute()
        && value.as_os_str().len() <= 4096
        && !value.as_os_str().as_encoded_bytes().contains(&0)
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        if !path(&self.directory) || self.endpoints.len() > 64 {
            return Err(Error::Input);
        }
        let mut ids = BTreeSet::new();
        for endpoint in &self.endpoints {
            ConversationId::new(&endpoint.id).map_err(|_| Error::Input)?;
            if endpoint.id.len() > 64
                || !ids.insert(&endpoint.id)
                || !path(&endpoint.cwd)
                || endpoint.mcp_servers.len() > 8
            {
                return Err(Error::Input);
            }
            endpoint.launch.validate()?;
            rsi_acp_protocol::configuration::validate(&endpoint.session_options)
                .map_err(|_| Error::Input)?;
            let mut names = BTreeSet::new();
            for server in &endpoint.mcp_servers {
                if server.name.is_empty()
                    || server.name.len() > 128
                    || server.name.contains('\0')
                    || !names.insert(&server.name)
                {
                    return Err(Error::Input);
                }
                server.launch.validate()?;
            }
        }
        if serde_json::to_vec(self).map_err(|_| Error::Input)?.len() > 256 * 1024 {
            return Err(Error::Input);
        }
        Ok(())
    }
}
impl Launch {
    fn validate(&self) -> Result<()> {
        if !path(&self.program)
            || self.arguments.len() > 256
            || self.arguments.iter().any(|text| text.contains('\0'))
            || self.arguments.iter().map(String::len).sum::<usize>() > 65536
            || self.environment.len() > 64
        {
            return Err(Error::Input);
        }
        let mut bytes = 0;
        for (key, value) in &self.environment {
            if key.is_empty() || key.len() > 256 || key.contains(['=', '\0']) {
                return Err(Error::Input);
            }
            bytes += key.len();
            match value {
                Environment::Literal { value } => {
                    if value.contains('\0') {
                        return Err(Error::Input);
                    }
                    bytes += value.len();
                }
                Environment::Credential { reference } => {
                    reference.validate().map_err(|_| Error::Input)?;
                    if reference.owner.as_str() != "rsi.acp" {
                        return Err(Error::Input);
                    }
                }
            }
        }
        if bytes > 65536 {
            return Err(Error::Input);
        }
        Ok(())
    }
}
