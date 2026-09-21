use crate::{
    State,
    config::{EndpointConfig, Environment, Launch},
};
use rsi_acp::{Peer, ProcessTransport};
use rsi_acp_protocol::{
    schema,
    service::{Error, Result},
};
use rsi_process::DuplexProcessSpec;
use rsi_sandbox::{ProcessRequest, ProcessStdio};
use serde_json::json;
use std::{collections::BTreeMap, path::Path};

async fn environment(state: &State, launch: &Launch) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    let mut bytes = 0_usize;
    for (key, value) in &launch.environment {
        let value = match value {
            Environment::Literal { value } => value.clone(),
            Environment::Credential { reference } => state
                .credentials
                .resolve(reference)
                .await
                .map_err(|_| Error::Launch)?
                .secret
                .expose_secret()
                .to_owned(),
        };
        bytes = bytes.saturating_add(key.len()).saturating_add(value.len());
        if bytes > 65536 || value.contains('\0') {
            return Err(Error::Launch);
        }
        values.insert(key.clone(), value);
    }
    Ok(values)
}

pub(super) async fn open(
    state: &State,
    endpoint: &EndpointConfig,
    cwd: &Path,
) -> Result<(Peer, Vec<schema::McpServer>)> {
    let mut mcp_servers = Vec::new();
    for server in &endpoint.mcp_servers {
        let values = environment(state, &server.launch).await?;
        mcp_servers.push(json!({"name":server.name,"command":server.launch.program,"args":server.launch.arguments,"env":values.into_iter().map(|(name,value)| json!({"name":name,"value":value})).collect::<Vec<_>>()}));
    }
    let setup = json!({"cwd":cwd,"mcpServers":mcp_servers});
    rsi_acp_protocol::validate_session_setup(&setup, false).map_err(|_| Error::Launch)?;
    let mcp_servers =
        serde_json::from_value(setup["mcpServers"].clone()).map_err(|_| Error::Launch)?;
    let confined = state
        .sandbox
        .confine(ProcessRequest {
            stdio: ProcessStdio::Pipes,
            mode: endpoint.sandbox,
            program: endpoint.launch.program.clone(),
            arguments: endpoint.launch.arguments.clone(),
            cwd: cwd.to_owned(),
            workspace: cwd.to_owned(),
        })
        .await
        .map_err(|_| Error::Launch)?;
    let environment = environment(state, &endpoint.launch)
        .await?
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    let process = state
        .processes
        .spawn(DuplexProcessSpec {
            process: confined,
            environment,
            stdout_buffer_bytes: 64 * 1024,
            stderr_max_bytes: 16 * 1024,
            termination_grace_ms: 1000,
        })
        .map_err(|_| Error::Launch)?;
    Ok((Peer::start(ProcessTransport::new(process)), mcp_servers))
}
