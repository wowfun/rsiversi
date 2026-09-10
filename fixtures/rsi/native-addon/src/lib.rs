mod ai;
mod api_probe;
mod ui;
use rsi_meta_native::{
    Activation, Message, NativeInstance, NativePlugin, Prepared, ProviderChannel, export_plugin,
};
use rsi_tools_protocol::portable::{self, Definition, Request, Response, Scheduling};
use rsi_tools_protocol::{ToolDefinition, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const DESCRIPTION: &str = if cfg!(feature = "revision-b") {
    "Native fixture tool revision B"
} else {
    "Native fixture tool"
};

#[derive(Default)]
struct Plugin;
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Independent opt-in fixture ports, not lifecycle states.
struct Config {
    label: String,
    #[serde(default = "enabled")]
    tools: bool,
    #[serde(default)]
    ai: bool,
    #[serde(default)]
    api_probe: bool,
    #[serde(default)]
    ui: bool,
}
fn enabled() -> bool {
    true
}
impl NativePlugin for Plugin {
    type Prepared = Config;
    type Instance = Instance;
    fn identity(&self) -> Result<String, String> {
        Ok("fixture.native-addon".into())
    }
    fn prepare(&self, desired: &Value) -> Result<Prepared<Config>, String> {
        let config: Config =
            serde_json::from_value(desired.clone()).map_err(|error| error.to_string())?;
        if config.label.is_empty() || config.label.len() > 64 {
            return Err("invalid label".into());
        }
        let bytes = u64::try_from(size_of::<Config>() + config.label.capacity())
            .map_err(|error| error.to_string())?;
        Ok(Prepared::new(desired.clone(), config, bytes))
    }
    fn create(&self, config: Config) -> Result<Instance, String> {
        Ok(Instance(config, 0))
    }
}
struct Instance(Config, u32);
impl NativeInstance for Instance {
    fn activate(&mut self, activation: &mut Activation<'_>) -> Result<(), String> {
        if self.0.ui {
            activation
                .effects()
                .provide(
                    "fixture.native.ui",
                    rsi_ui_protocol::portable::CONTRACT,
                    u64::from(rsi_ui_protocol::portable::VERSION),
                    b"ui",
                )
                .map_err(|error| error.to_string())?;
        }
        if self.0.api_probe {
            activation
                .effects()
                .provide(
                    "fixture.native.api-probe",
                    "fixture.api-probe",
                    1,
                    b"api-probe",
                )
                .map_err(|error| error.to_string())?;
        }
        if self.0.tools {
            activation
                .effects()
                .provide(
                    "fixture.native.tools",
                    portable::CONTRACT,
                    u64::from(portable::VERSION),
                    b"tools",
                )
                .map_err(|error| error.to_string())?;
        }
        if self.0.ai {
            activation
                .effects()
                .provide(
                    "fixture.native.ai",
                    rsi_ai_protocol::portable::PROVIDER_CONTRACT,
                    u64::from(rsi_ai_protocol::portable::PROVIDER_VERSION),
                    b"ai",
                )
                .map_err(|error| error.to_string())?;
        }
        activation
            .effects()
            .commit()
            .map_err(|error| error.to_string())
    }
    fn serve(&mut self, port: &[u8], channel: &mut ProviderChannel<'_>) -> Result<(), String> {
        if port == b"ui" {
            let result = ui::serve(channel, &mut self.1);
            if let Err(error) = &result {
                eprintln!("fixture native UI: {error}");
            }
            return result;
        }
        if port == b"api-probe" {
            return api_probe::serve(channel);
        }
        if port == b"ai" {
            return ai::serve(channel, &self.0.label);
        }
        if port != b"tools" {
            return Err("unknown port".into());
        }
        match receive(channel)? {
            Request::Describe {} => {
                if channel
                    .receive()
                    .map_err(|error| error.to_string())?
                    .is_some()
                {
                    return Err("extra describe request".into());
                }
                let tools = ["native_echo", "native_confine"]
                    .into_iter()
                    .map(|name| {
                        Ok(Definition {
                            definition: ToolDefinition::new(
                                name,
                                DESCRIPTION,
                                json!({"type":"object"}),
                            )
                            .map_err(|error| error.to_string())?,
                            timeout_ms: 5_000,
                            scheduling: Scheduling::Exclusive,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                send(channel, &Response::Description { tools })
            }
            Request::Execute { call, policy } => {
                let value = match call.name.as_str() {
                    "native_echo" => {
                        json!({"label":self.0.label,"arguments":call.arguments,"policy":policy})
                    }
                    "native_confine" => {
                        send(
                            channel,
                            &Response::Confine {
                                program: "/bin/echo".into(),
                                arguments: vec!["native".into()],
                            },
                        )?;
                        let Request::Confined { plan } = receive(channel)? else {
                            return Err("missing confined plan".into());
                        };
                        json!({"label":self.0.label,"plan":plan})
                    }
                    _ => return Err("unknown tool".into()),
                };
                let result =
                    ToolResult::new(value, Vec::new(), false).map_err(|error| error.to_string())?;
                send(channel, &Response::Result { result })
            }
            Request::Confined { .. } => Err("unexpected initial message".into()),
        }
    }
}
fn receive(channel: &mut ProviderChannel<'_>) -> Result<Request, String> {
    let message = channel
        .receive()
        .map_err(|error| error.to_string())?
        .ok_or("missing request")?;
    if !message.capabilities.is_empty() {
        return Err("unexpected capabilities".into());
    }
    portable::decode(&message.bytes).map_err(|error| error.to_string())
}
fn send(channel: &mut ProviderChannel<'_>, value: &Response) -> Result<(), String> {
    channel
        .send(&Message {
            bytes: portable::encode(value).map_err(|error| error.to_string())?,
            capabilities: Vec::new(),
        })
        .map_err(|error| error.to_string())
}
export_plugin!(Plugin);
