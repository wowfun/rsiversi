use crate::{
    FrozenServer, ServerConfig, TransportConfig,
    discovery::discover,
    error::{McpError, Result},
    transport::Connection,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

/// Private verified connection, absent from the shared service manifest.
#[derive(Debug)]
pub struct PrivateMcp {
    connection: Arc<Connection>,
    catalog: Arc<FrozenServer>,
}
impl PrivateMcp {
    /// Attaches a caller-confined process and freezes a complete finite catalog.
    pub async fn attach(
        process: rsi_process::ManagedDuplexProcess,
        id: &str,
        selected: Vec<String>,
    ) -> Result<Self> {
        let config = ServerConfig {
            id: id.into(),
            enabled: true,
            tools: selected,
            resource_templates: false,
            transport: TransportConfig::Stdio {
                program: "/private/managed".into(),
                arguments: vec![],
                cwd: "/private".into(),
                environment: BTreeMap::new(),
            },
        };
        config.validate().map_err(|_| McpError::Protocol)?;
        let connection = Connection::attach(process);
        let catalog = match discover(&connection, &config, true, false).await {
            Ok(c) => c,
            Err(e) => {
                let _ = connection.shutdown().await;
                return Err(e);
            }
        };
        connection.seal_bootstrap();
        Ok(Self {
            connection,
            catalog,
        })
    }
    /// The immutable verified catalog; annotations grant no permission.
    pub fn catalog(&self) -> &FrozenServer {
        &self.catalog
    }
    /// Calls exactly one selected raw name. Unknown outcomes invalidate the epoch.
    pub async fn call(&self, name: &str, arguments: Value) -> Result<Value> {
        if !self
            .catalog
            .tools
            .iter()
            .any(|tool| tool.selected && tool.definition.name == name)
        {
            return Err(McpError::NotFound);
        }
        self.connection
            .request("tools/call", json!({"name":name,"arguments":arguments}))
            .await
    }
    /// Stops admission and waits for transport/process settlement.
    pub async fn close(&self) -> Result<()> {
        self.connection.shutdown().await
    }
}
impl Drop for PrivateMcp {
    fn drop(&mut self) {
        self.connection.close();
    }
}
