use rsi_meta_native::{
    Activation, Message, NativeInstance, NativePlugin, Prepared, ProviderChannel, export_plugin,
};
use rsi_tools_protocol::portable::{self, Definition, Request, Response, Scheduling};
use rsi_tools_protocol::{ToolDefinition, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Default)]
struct Plugin;
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    label: String,
}
impl NativePlugin for Plugin {
    type Prepared = Config;
    type Instance = Instance;
    fn identity(&self) -> Result<String, String> {
        Ok("addon.template".into())
    }
    fn prepare(&self, desired: &Value) -> Result<Prepared<Config>, String> {
        let config: Config = serde_json::from_value(desired.clone()).map_err(|e| e.to_string())?;
        if config.label.is_empty() || config.label.len() > 64 {
            return Err("label must contain 1..64 bytes".into());
        }
        let bytes = u64::try_from(size_of::<Config>() + config.label.capacity())
            .map_err(|e| e.to_string())?;
        Ok(Prepared::new(desired.clone(), config, bytes))
    }
    fn create(&self, config: Config) -> Result<Instance, String> {
        Ok(Instance(config))
    }
}
struct Instance(Config);
impl NativeInstance for Instance {
    fn activate(&mut self, activation: &mut Activation<'_>) -> Result<(), String> {
        activation
            .effects()
            .provide(
                "addon.template.tools",
                portable::CONTRACT,
                u64::from(portable::VERSION),
                b"tools",
            )
            .map_err(|e| e.to_string())?;
        activation.effects().commit().map_err(|e| e.to_string())
    }
    fn serve(&mut self, port: &[u8], channel: &mut ProviderChannel<'_>) -> Result<(), String> {
        if port != b"tools" {
            return Err("unknown port".into());
        }
        let message = channel
            .receive()
            .map_err(|e| e.to_string())?
            .ok_or("missing request")?;
        if !message.capabilities.is_empty() {
            return Err("unexpected capabilities".into());
        }
        let request: Request = portable::decode(&message.bytes).map_err(|e| e.to_string())?;
        let response = match request {
            Request::Describe {} => {
                if channel.receive().map_err(|e| e.to_string())?.is_some() {
                    return Err("extra describe request".into());
                }
                Response::Description {
                    tools: vec![Definition {
                        definition: ToolDefinition::new(
                            "addon_echo",
                            "Echo JSON with the configured label",
                            json!({"type":"object"}),
                        )
                        .map_err(|e| e.to_string())?,
                        timeout_ms: 5_000,
                        scheduling: Scheduling::Exclusive,
                    }],
                }
            }
            Request::Execute { call, .. } if call.name == "addon_echo" => Response::Result {
                result: ToolResult::new(
                    json!({"label":self.0.label,"arguments":call.arguments}),
                    Vec::new(),
                    false,
                )
                .map_err(|e| e.to_string())?,
            },
            Request::Execute { .. } => return Err("unknown tool".into()),
            Request::Confined { .. } => return Err("unexpected request".into()),
        };
        channel
            .send(&Message {
                bytes: portable::encode(&response).map_err(|e| e.to_string())?,
                capabilities: Vec::new(),
            })
            .map_err(|e| e.to_string())
    }
}
export_plugin!(Plugin);
