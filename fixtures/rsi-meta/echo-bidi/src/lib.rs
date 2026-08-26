use rsi_meta_plugin::{
    Activation, Message, NativeInstance, NativePlugin, Prepared, ProviderChannel,
    ServiceRequirement, export_plugin,
};
use serde_json::{Value, json};

#[derive(Default)]
struct EchoPlugin;

impl NativePlugin for EchoPlugin {
    type Prepared = EchoConfig;
    type Instance = EchoInstance;

    fn identity(&self) -> Result<String, String> {
        Ok("fixture.native-echo".to_owned())
    }

    fn prepare(&self, desired: &Value) -> Result<Prepared<Self::Prepared>, String> {
        let config = EchoConfig::validate(desired)?;
        config.validation_gate()?;
        let normalized = config.normalized();
        let retained_bytes = config.retained_bytes();
        Ok(
            Prepared::new(normalized, config, retained_bytes).requiring(ServiceRequirement::new(
                "upstream",
                "fixture.upstream",
                1,
            )),
        )
    }

    fn create(&self, config: Self::Prepared) -> Result<Self::Instance, String> {
        config.create_gate()?;
        Ok(EchoInstance {
            prefix: config.prefix,
            delay_ms: config.delay_ms,
            destroy_delay_ms: config.destroy_delay_ms,
            call_entered_path: config.call_entered_path,
            call_release_path: config.call_release_path,
            destroy_entered_path: config.destroy_entered_path,
            destroy_release_path: config.destroy_release_path,
            upstream: None,
        })
    }
}

struct EchoConfig {
    prefix: Vec<u8>,
    delay_ms: u64,
    validate_delay_ms: u64,
    create_delay_ms: u64,
    destroy_delay_ms: u64,
    create_entered_path: Option<String>,
    create_release_path: Option<String>,
    create_completed_path: Option<String>,
    call_entered_path: Option<String>,
    call_release_path: Option<String>,
    destroy_entered_path: Option<String>,
    destroy_release_path: Option<String>,
    validate_entered_path: Option<String>,
    validate_release_path: Option<String>,
}

impl EchoConfig {
    fn validate(value: &Value) -> Result<Self, String> {
        let prefix = value
            .get("prefix")
            .and_then(Value::as_str)
            .filter(|prefix| prefix.len() <= 64)
            .ok_or_else(|| "config.prefix must be a string of at most 64 bytes".to_owned())?
            .as_bytes()
            .to_vec();
        Ok(Self {
            prefix,
            delay_ms: delay(value, "delay_ms")?,
            validate_delay_ms: delay(value, "validate_delay_ms")?,
            create_delay_ms: delay(value, "create_delay_ms")?,
            destroy_delay_ms: delay(value, "destroy_delay_ms")?,
            create_entered_path: optional_path(value, "create_entered_path")?,
            create_release_path: optional_path(value, "create_release_path")?,
            create_completed_path: optional_path(value, "create_completed_path")?,
            call_entered_path: optional_path(value, "call_entered_path")?,
            call_release_path: optional_path(value, "call_release_path")?,
            destroy_entered_path: optional_path(value, "destroy_entered_path")?,
            destroy_release_path: optional_path(value, "destroy_release_path")?,
            validate_entered_path: optional_path(value, "validate_entered_path")?,
            validate_release_path: optional_path(value, "validate_release_path")?,
        })
    }

    fn validation_gate(&self) -> Result<(), String> {
        signal_and_wait(
            self.validate_entered_path.as_deref(),
            self.validate_release_path.as_deref(),
        )?;
        std::thread::sleep(std::time::Duration::from_millis(self.validate_delay_ms));
        Ok(())
    }

    fn create_gate(&self) -> Result<(), String> {
        signal_and_wait(
            self.create_entered_path.as_deref(),
            self.create_release_path.as_deref(),
        )?;
        std::thread::sleep(std::time::Duration::from_millis(self.create_delay_ms));
        if let Some(path) = &self.create_completed_path {
            std::fs::write(path, b"completed").map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn normalized(&self) -> Value {
        json!({
            "prefix": String::from_utf8_lossy(&self.prefix),
            "delay_ms": self.delay_ms,
            "validate_delay_ms": 0,
            "create_delay_ms": self.create_delay_ms,
            "destroy_delay_ms": self.destroy_delay_ms,
            "create_entered_path": self.create_entered_path,
            "create_release_path": self.create_release_path,
            "create_completed_path": self.create_completed_path,
            "call_entered_path": self.call_entered_path,
            "call_release_path": self.call_release_path,
            "destroy_entered_path": self.destroy_entered_path,
            "destroy_release_path": self.destroy_release_path,
            "validate_entered_path": self.validate_entered_path,
            "validate_release_path": self.validate_release_path,
        })
    }

    fn retained_bytes(&self) -> u64 {
        let paths = [
            &self.create_entered_path,
            &self.create_release_path,
            &self.create_completed_path,
            &self.call_entered_path,
            &self.call_release_path,
            &self.destroy_entered_path,
            &self.destroy_release_path,
            &self.validate_entered_path,
            &self.validate_release_path,
        ];
        let path_bytes: usize = paths
            .iter()
            .filter_map(|path| path.as_ref())
            .map(String::capacity)
            .sum();
        u64::try_from(size_of::<Self>() + self.prefix.capacity() + path_bytes)
            .expect("bounded echo configuration charge fits u64")
    }
}

struct EchoInstance {
    prefix: Vec<u8>,
    delay_ms: u64,
    destroy_delay_ms: u64,
    call_entered_path: Option<String>,
    call_release_path: Option<String>,
    destroy_entered_path: Option<String>,
    destroy_release_path: Option<String>,
    upstream: Option<rsi_meta_plugin::Capability>,
}

impl NativeInstance for EchoInstance {
    fn activate(&mut self, activation: &mut Activation<'_>) -> Result<(), String> {
        self.upstream = Some(
            activation
                .injection("upstream")
                .ok_or_else(|| "missing upstream injection".to_owned())?
                .try_clone()
                .map_err(|error| error.to_string())?,
        );
        activation
            .effects()
            .defer("echo fixture activation", || Ok(()))
            .map_err(|error| error.to_string())?;
        let published = activation
            .effects()
            .provide("echo", "fixture.echo", 1, b"echo")
            .map_err(|error| error.to_string())?;
        drop(published);
        activation
            .effects()
            .commit()
            .map_err(|error| error.to_string())
    }

    fn serve(&mut self, port: &[u8], channel: &mut ProviderChannel<'_>) -> Result<(), String> {
        if port != b"echo" {
            return Err(format!(
                "unknown native port {}",
                String::from_utf8_lossy(port)
            ));
        }
        signal_and_wait(
            self.call_entered_path.as_deref(),
            self.call_release_path.as_deref(),
        )?;
        std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
        let request = channel
            .receive()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "echo request stream ended before one message".to_owned())?;
        if channel
            .receive()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err("echo accepts exactly one request message".to_owned());
        }
        let upstream = self
            .upstream
            .as_ref()
            .ok_or_else(|| "echo instance is not active".to_owned())?;
        let mut upstream_call = channel
            .host()
            .open(upstream)
            .map_err(|error| error.to_string())?;
        upstream_call
            .send(&request)
            .map_err(|error| error.to_string())?;
        upstream_call
            .finish_requests()
            .map_err(|error| error.to_string())?;
        let mut response = upstream_call
            .receive()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "upstream returned no response".to_owned())?;
        if upstream_call
            .receive()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err("upstream returned more than one response".to_owned());
        }
        upstream_call
            .terminal()
            .map_err(|error| error.to_string())?;
        let mut bytes = self.prefix.clone();
        bytes.append(&mut response.bytes);
        channel
            .send(&Message {
                bytes,
                capabilities: response.capabilities,
            })
            .map_err(|error| error.to_string())
    }
}

impl Drop for EchoInstance {
    fn drop(&mut self) {
        let _ = signal_and_wait(
            self.destroy_entered_path.as_deref(),
            self.destroy_release_path.as_deref(),
        );
        std::thread::sleep(std::time::Duration::from_millis(self.destroy_delay_ms));
    }
}

fn delay(config: &Value, field: &str) -> Result<u64, String> {
    config.get(field).map_or(Ok(0), |value| {
        value
            .as_u64()
            .filter(|delay| *delay <= 5_000)
            .ok_or_else(|| format!("config.{field} must be an integer from 0 through 5000"))
    })
}

fn optional_path(config: &Value, field: &str) -> Result<Option<String>, String> {
    config.get(field).map_or(Ok(None), |value| {
        value
            .as_str()
            .filter(|path| !path.is_empty() && path.len() <= 4_096)
            .map(|path| Some(path.to_owned()))
            .ok_or_else(|| format!("config.{field} must be a nonempty path string"))
    })
}

fn signal_and_wait(entered: Option<&str>, release: Option<&str>) -> Result<(), String> {
    if let Some(path) = entered {
        std::fs::write(path, b"entered").map_err(|error| error.to_string())?;
    }
    if let Some(path) = release {
        while !std::path::Path::new(path).exists() {
            std::thread::yield_now();
        }
    }
    Ok(())
}

export_plugin!(EchoPlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use core::ffi::c_void;
    use rsi_meta_plugin::{
        ABI_MAJOR, ABI_MINOR, BasicOutput, CapInput, EmptyInput, FrameHeader, HostTable,
        PLUGIN_DESTROY_FACTORY, PLUGIN_FINALIZE, PluginTable, STATUS_OK, STATUS_UNSUPPORTED,
        TableHeader,
    };

    unsafe extern "C" fn host_exchange(
        _: *mut c_void,
        _: u32,
        _: *const c_void,
        _: u32,
        _: *mut c_void,
        _: u32,
    ) -> u32 {
        STATUS_UNSUPPORTED
    }

    #[test]
    fn fixture_preparation_declares_only_the_actual_injection() {
        let prepared = EchoPlugin
            .prepare(&json!({ "prefix": "v2:" }))
            .expect("valid fixture config");
        assert_eq!(prepared.normalized_config()["prefix"], "v2:");
        assert_eq!(prepared.requirements().len(), 1);
        assert_eq!(prepared.requirements()[0].key(), "upstream");
        assert!(prepared.retained_bytes() >= u64::try_from(size_of::<EchoConfig>()).unwrap());
        assert!(EchoPlugin.prepare(&json!({ "prefix": 7 })).is_err());
    }

    #[test]
    fn fixture_exports_one_compatible_v2_table() {
        let host = HostTable {
            header: TableHeader::new(ABI_MINOR, HostTable::STRUCT_SIZE),
            issuer: 4_242,
            state: core::ptr::dangling_mut(),
            exchange: Some(host_exchange),
        };
        let mut plugin = PluginTable::EMPTY;
        // SAFETY: The generated entry borrows two complete aligned tables.
        assert_eq!(
            unsafe {
                super::rsi_meta_plugin_entry_v2(
                    &raw const host,
                    &raw mut plugin,
                    PluginTable::STRUCT_SIZE,
                )
            },
            STATUS_OK
        );
        assert_eq!(plugin.header.abi_major, ABI_MAJOR);
        assert_eq!(plugin.header.abi_minor, ABI_MINOR);
        assert!(plugin.is_compatible_for_host(ABI_MINOR));

        let input = CapInput {
            header: FrameHeader::new(u32::try_from(size_of::<CapInput>()).unwrap()),
            capability: plugin.factory,
        };
        let mut output: BasicOutput = unsafe { core::mem::zeroed() };
        assert_eq!(
            call(plugin, PLUGIN_DESTROY_FACTORY, &input, &mut output),
            STATUS_OK
        );
        assert_eq!(
            call(
                plugin,
                PLUGIN_FINALIZE,
                &EmptyInput {
                    header: FrameHeader::new(u32::try_from(size_of::<EmptyInput>()).unwrap(),),
                },
                &mut output,
            ),
            STATUS_OK
        );
    }

    fn call<I, O>(plugin: PluginTable, opcode: u32, input: &I, output: &mut O) -> u32 {
        // SAFETY: Entry validated the exchange and both typed frames remain live.
        unsafe {
            plugin.exchange.expect("fixture exchange")(
                plugin.state,
                opcode,
                std::ptr::from_ref(input).cast(),
                u32::try_from(size_of::<I>()).unwrap(),
                std::ptr::from_mut(output).cast(),
                u32::try_from(size_of::<O>()).unwrap(),
            )
        }
    }
}
