use rsi_meta_plugin::{Host, NativeInstance, NativePlugin, export_plugin};
use serde_json::{Value, json};

#[derive(Default)]
struct EchoPlugin;

impl NativePlugin for EchoPlugin {
    type Instance = EchoInstance;

    fn descriptor(&self) -> Value {
        json!({
            "identity": {
                "kind": "builtin",
                "name": "fixture.native-echo",
                "revision": "1"
            },
            "requires": [{
                "key": "upstream",
                "contract": "fixture.upstream",
                "version": 1
            }],
            "provides": [{
                "key": "echo",
                "contract": "fixture.echo",
                "version": 1
            }]
        })
    }

    fn validate_config(&self, config: Value) -> Result<Value, String> {
        let prefix = config
            .get("prefix")
            .and_then(Value::as_str)
            .ok_or_else(|| "config.prefix must be a string".to_owned())?;
        if prefix.len() > 64 {
            return Err("config.prefix exceeds 64 bytes".to_owned());
        }
        let delay_ms = config.get("delay_ms").map_or(Ok(0), |value| {
            value
                .as_u64()
                .filter(|delay| *delay <= 5_000)
                .ok_or_else(|| "config.delay_ms must be an integer from 0 through 5000".to_owned())
        })?;
        let validate_delay_ms = config.get("validate_delay_ms").map_or(Ok(0), |value| {
            value
                .as_u64()
                .filter(|delay| *delay <= 5_000)
                .ok_or_else(|| {
                    "config.validate_delay_ms must be an integer from 0 through 5000".to_owned()
                })
        })?;
        let destroy_delay_ms = config.get("destroy_delay_ms").map_or(Ok(0), |value| {
            value
                .as_u64()
                .filter(|delay| *delay <= 5_000)
                .ok_or_else(|| {
                    "config.destroy_delay_ms must be an integer from 0 through 5000".to_owned()
                })
        })?;
        let create_delay_ms = config.get("create_delay_ms").map_or(Ok(0), |value| {
            value
                .as_u64()
                .filter(|delay| *delay <= 5_000)
                .ok_or_else(|| {
                    "config.create_delay_ms must be an integer from 0 through 5000".to_owned()
                })
        })?;
        let create_entered_path = optional_path(&config, "create_entered_path")?;
        let create_release_path = optional_path(&config, "create_release_path")?;
        let create_completed_path = optional_path(&config, "create_completed_path")?;
        let call_entered_path = optional_path(&config, "call_entered_path")?;
        let call_release_path = optional_path(&config, "call_release_path")?;
        let destroy_entered_path = optional_path(&config, "destroy_entered_path")?;
        let destroy_release_path = optional_path(&config, "destroy_release_path")?;
        let validate_entered_path = optional_path(&config, "validate_entered_path")?;
        let validate_release_path = optional_path(&config, "validate_release_path")?;
        signal_and_wait(
            validate_entered_path.as_deref(),
            validate_release_path.as_deref(),
        )?;
        std::thread::sleep(std::time::Duration::from_millis(validate_delay_ms));
        Ok(json!({
            "prefix": prefix,
            "delay_ms": delay_ms,
            "validate_delay_ms": 0,
            "create_delay_ms": create_delay_ms,
            "destroy_delay_ms": destroy_delay_ms,
            "create_entered_path": create_entered_path,
            "create_release_path": create_release_path,
            "create_completed_path": create_completed_path,
            "call_entered_path": call_entered_path,
            "call_release_path": call_release_path,
            "destroy_entered_path": destroy_entered_path,
            "destroy_release_path": destroy_release_path
        }))
    }

    fn create(&self, config: Value) -> Result<Self::Instance, String> {
        signal_and_wait(
            config["create_entered_path"].as_str(),
            config["create_release_path"].as_str(),
        )?;
        std::thread::sleep(std::time::Duration::from_millis(
            config["create_delay_ms"].as_u64().unwrap_or_default(),
        ));
        if let Some(path) = config["create_completed_path"].as_str() {
            std::fs::write(path, b"completed").map_err(|error| error.to_string())?;
        }
        Ok(EchoInstance {
            prefix: config["prefix"]
                .as_str()
                .unwrap_or_default()
                .as_bytes()
                .to_vec(),
            delay_ms: config["delay_ms"].as_u64().unwrap_or_default(),
            destroy_delay_ms: config["destroy_delay_ms"].as_u64().unwrap_or_default(),
            call_entered_path: config["call_entered_path"].as_str().map(str::to_owned),
            call_release_path: config["call_release_path"].as_str().map(str::to_owned),
            destroy_entered_path: config["destroy_entered_path"].as_str().map(str::to_owned),
            destroy_release_path: config["destroy_release_path"].as_str().map(str::to_owned),
        })
    }
}

fn optional_path(config: &Value, field: &str) -> Result<Option<String>, String> {
    config.get(field).map_or(Ok(None), |value| {
        value
            .as_str()
            .filter(|path| !path.is_empty() && path.len() <= 4096)
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

struct EchoInstance {
    prefix: Vec<u8>,
    delay_ms: u64,
    destroy_delay_ms: u64,
    call_entered_path: Option<String>,
    call_release_path: Option<String>,
    destroy_entered_path: Option<String>,
    destroy_release_path: Option<String>,
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

impl NativeInstance for EchoInstance {
    fn call(&mut self, host: &Host<'_>, service: &str, request: &[u8]) -> Result<Vec<u8>, String> {
        if service != "echo" {
            return Err(format!("unknown service {service}"));
        }
        signal_and_wait(
            self.call_entered_path.as_deref(),
            self.call_release_path.as_deref(),
        )?;
        std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
        let upstream = host.call("upstream", request)?;
        let mut response = self.prefix.clone();
        response.extend(upstream);
        Ok(response)
    }
}

export_plugin!(EchoPlugin);
