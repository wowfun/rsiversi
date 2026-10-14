struct CliFixture {
    temporary: TempDir,
    workspace: std::path::PathBuf,
    binary: std::path::PathBuf,
}

impl CliFixture {
    fn new(endpoint: &str) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let config = temporary.path().join("config/rsi");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            config.join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "rsi.agent": {
                    "sandbox": if cfg!(windows) { "danger-full-access" } else { "workspace-write" },
                    "require_approval": cfg!(windows),
                    "default_model": {"deployment": "fixture", "model": "fixture-model"}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let host_directory = config.join("host-profiles/fixture");
        let application_directory = config.join("application-profiles/test-headless");
        let cli_directory = config.join("application-profiles/test-cli");
        std::fs::create_dir_all(&host_directory).unwrap();
        std::fs::create_dir_all(&application_directory).unwrap();
        std::fs::create_dir_all(&cli_directory).unwrap();
        std::fs::write(
            host_directory.join("host.profile.toml"),
            format!(
                r#"format = 1

[[steps]]
kind = "plugin"
id = "fixture-provider"
plugin = "rsi.ai.provider.openai-compatible"

[steps.config]
deployment = "fixture"
endpoint = "{endpoint}"
path = "/v1/chat/completions"
allow_image_input = false
credential = {{ owner = "rsi.ai.provider.openai-compatible", slot = "default" }}

[steps.config.language_models.fixture-model]
context_window_tokens = 128000
default_output_reserve_tokens = 4096
max_output_reserve_tokens = 16384
"#
            ),
        )
        .unwrap();
        std::fs::write(
            application_directory.join("application.profile.toml"),
            "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"connection\"\nplugin = \"rsi.application.connection\"\nconfig = { host_profile = \"fixture\" }\n[[steps]]\nkind = \"plugin\"\nid = \"application\"\nplugin = \"rsi.application.headless\"\n",
        )
        .unwrap();
        std::fs::write(
            cli_directory.join("application.profile.toml"),
            "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"connection\"\nplugin = \"rsi.application.connection\"\nconfig = { host_profile = \"fixture\" }\n[[steps]]\nkind = \"plugin\"\nid = \"application\"\nplugin = \"rsi.application.cli\"\n",
        )
        .unwrap();
        Self {
            temporary,
            workspace,
            binary: env!("CARGO_BIN_EXE_rsi").into(),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .current_dir(&self.workspace)
            .env_remove("DEEPSEEK_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("RSI_DEEPSEEK_API_KEY")
            .env_remove("RSI_OPENAI_API_KEY")
            .env("HOME", self.temporary.path().join("home"))
            .env("XDG_CONFIG_HOME", self.temporary.path().join("config"))
            .env("XDG_STATE_HOME", self.temporary.path().join("state"))
            .env("XDG_CACHE_HOME", self.temporary.path().join("cache"))
            .env("XDG_RUNTIME_DIR", self.temporary.path().join("runtime"))
            .env("RSI_OPENAI_COMPATIBLE_API_KEY", "fixture-secret");
        command
    }

    fn tokio_command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .current_dir(&self.workspace)
            .env_remove("DEEPSEEK_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("RSI_DEEPSEEK_API_KEY")
            .env_remove("RSI_OPENAI_API_KEY")
            .env("HOME", self.temporary.path().join("home"))
            .env("XDG_CONFIG_HOME", self.temporary.path().join("config"))
            .env("XDG_STATE_HOME", self.temporary.path().join("state"))
            .env("XDG_CACHE_HOME", self.temporary.path().join("cache"))
            .env("XDG_RUNTIME_DIR", self.temporary.path().join("runtime"))
            .env("RSI_OPENAI_COMPATIBLE_API_KEY", "fixture-secret");
        command
    }

    fn require_approval(&self) {
        std::fs::write(
            self.temporary.path().join("config/rsi/settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "rsi.agent": {
                    "default_model": {"deployment": "fixture", "model": "fixture-model"},
                    "require_approval": true
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn configure_agent_preset_root(&self) {
        let root = self.temporary.path().join("configured-agent-presets");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            self.temporary.path().join("config/rsi/settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "rsi.agent": {
                    "default_model": {"deployment": "fixture", "model": "fixture-model"}
                },
                "rsi.agent-presets": {
                    "default": "standard",
                    "roots": [{"path": root, "trust": "user"}]
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.command().args(arguments).output().unwrap()
    }

    fn assert_success(&self, arguments: &[&str]) -> Output {
        let output = self.run(arguments);
        assert!(
            output.status.success(),
            "{arguments:?} failed\nstdout: {}\nstderr: {}\nowner log: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            self.owner_log()
        );
        output
    }

    #[cfg(not(target_os = "linux"))]
    fn owner_log(&self) -> String {
        "embedded Session service has no daemon log".to_owned()
    }

    #[cfg(target_os = "linux")]
    fn owner_log(&self) -> String {
        use std::io::Read as _;
        let mut log = String::new();
        let path = self
            .temporary
            .path()
            .join("state/rsi/session-host/owner.log");
        let result = std::fs::File::open(path)
            .and_then(|file| file.take(64 * 1024).read_to_string(&mut log));
        result.map_or_else(|error| format!("unavailable: {error}"), |_| log)
    }
}

impl Drop for CliFixture {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        let _ = self.command().args(["host", "stop", "--force"]).output();
    }
}

#[allow(
    clippy::unused_async,
    reason = "Axum Handler requires an asynchronous function."
)]
async fn chat() -> Response {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"hello from daemon\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3}}\n\n",
        "data: [DONE]\n\n"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .unwrap()
}
