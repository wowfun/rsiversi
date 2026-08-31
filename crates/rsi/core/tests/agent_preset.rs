use rsi::{AgentPresetManager, AgentPresetSource, AgentPresetTrust};
use rsi_agent_presets::{AgentPresetId, COMPOSITION_FILE};
use rsi_host::HostPaths;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn write_preset(root: &Path, id: &str, composition: &str) {
    let directory = root.join(id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(COMPOSITION_FILE), composition).unwrap();
}

#[tokio::test]
async fn manager_derives_settings_roots_user_root_and_default_override() {
    let temporary = tempfile::tempdir().unwrap();
    let config = temporary.path().join("config");
    let state = temporary.path().join("state");
    let cache = temporary.path().join("cache");
    let system = temporary.path().join("system");
    let configured = temporary.path().join("configured");
    let user = config.join("agent-presets");
    fs::create_dir_all(&config).unwrap();
    write_preset(&system, "standard", "format = 1\n# standard\n");
    write_preset(&configured, "team", "format = 1\n# team\n");
    write_preset(&user, "mine", "format = 1\n# mine\n");
    fs::write(
        config.join("settings.json"),
        serde_json::to_vec_pretty(&json!({
            "unrelated.namespace": { "keep": true },
            "rsi.agent-presets": {
                "roots": [{ "path": configured }]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let paths = HostPaths::new(config.clone(), state, cache).unwrap();

    let manager = AgentPresetManager::open(paths, [system.clone()], false)
        .await
        .unwrap();
    let roster = manager.catalog().roster().await.unwrap();
    assert_eq!(
        roster
            .presets
            .iter()
            .map(|row| (row.id.as_str(), row.source, row.trust))
            .collect::<Vec<_>>(),
        [
            (
                "standard",
                AgentPresetSource::System,
                AgentPresetTrust::System,
            ),
            (
                "team",
                AgentPresetSource::Configured,
                AgentPresetTrust::User,
            ),
            ("mine", AgentPresetSource::User, AgentPresetTrust::User),
        ]
    );
    assert_eq!(
        manager.catalog().default_id().await.unwrap().as_str(),
        "standard"
    );

    manager
        .catalog()
        .set_default(&AgentPresetId::new("team").unwrap())
        .await
        .unwrap();
    assert_eq!(
        manager.catalog().default_id().await.unwrap().as_str(),
        "team"
    );
    let document: serde_json::Value =
        serde_json::from_slice(&fs::read(config.join("settings.json")).unwrap()).unwrap();
    assert_eq!(document["unrelated.namespace"]["keep"], true);
    assert_eq!(document["rsi.agent-presets"]["default"], "team");
    assert_eq!(
        document["rsi.agent-presets"]["roots"][0]["path"],
        configured.to_string_lossy().as_ref()
    );

    manager.catalog().clear_default().await.unwrap();
    assert_eq!(
        manager.catalog().default_id().await.unwrap().as_str(),
        "standard"
    );
    let document: serde_json::Value =
        serde_json::from_slice(&fs::read(config.join("settings.json")).unwrap()).unwrap();
    assert_eq!(document["unrelated.namespace"]["keep"], true);
    assert!(document["rsi.agent-presets"].get("default").is_none());
    assert_eq!(
        document["rsi.agent-presets"]["roots"][0]["path"],
        configured.to_string_lossy().as_ref()
    );
    assert!(manager.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_managers_detect_a_concurrent_default_write_without_clobbering() {
    let temporary = tempfile::tempdir().unwrap();
    let config = temporary.path().join("config");
    let state = temporary.path().join("state");
    let cache = temporary.path().join("cache");
    let system = temporary.path().join("system");
    fs::create_dir_all(&config).unwrap();
    for id in ["standard", "minimal", "review"] {
        write_preset(&system, id, &format!("format = 1\n# {id}\n"));
    }
    let paths = HostPaths::new(config, state, cache).unwrap();
    let first = AgentPresetManager::open(paths.clone(), [system.clone()], false)
        .await
        .unwrap();
    let second = AgentPresetManager::open(paths.clone(), [system.clone()], false)
        .await
        .unwrap();
    let minimal = AgentPresetId::new("minimal").unwrap();
    let review = AgentPresetId::new("review").unwrap();

    let (left, right) = tokio::join!(
        first.catalog().set_default(&minimal),
        second.catalog().set_default(&review),
    );
    assert_eq!(
        [&left, &right]
            .into_iter()
            .filter(|result| result.is_ok())
            .count(),
        1
    );
    let failure = [&left, &right]
        .into_iter()
        .find_map(|result| result.as_ref().err())
        .unwrap()
        .to_string();
    assert!(failure.contains("settings document changed concurrently"));
    assert!(first.shutdown().await.is_clean());
    assert!(second.shutdown().await.is_clean());

    let reopened = AgentPresetManager::open(paths, [system], false)
        .await
        .unwrap();
    let selected = reopened.catalog().default_id().await.unwrap();
    assert!(matches!(selected.as_str(), "minimal" | "review"));
    assert!(reopened.shutdown().await.is_clean());
}

fn binary(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rsi"))
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn built_binary_advertises_the_complete_management_command_tree() {
    let root = binary(&["--help"]);
    assert!(root.status.success());
    let root = String::from_utf8(root.stdout).unwrap();
    assert!(root.contains("rsi agent-preset <COMMAND>"));
    assert!(root.contains("--agent-preset ID"));

    let preset = binary(&["agent-preset", "--help"]);
    assert!(preset.status.success());
    let preset = String::from_utf8(preset.stdout).unwrap();
    for command in ["list", "show", "path", "copy", "delete", "default"] {
        assert!(
            preset.contains(command),
            "missing {command:?} in {preset:?}"
        );
    }

    let default = binary(&["agent-preset", "default", "--help"]);
    assert!(default.status.success());
    let default = String::from_utf8(default.stdout).unwrap();
    for command in ["get", "set", "clear"] {
        assert!(
            default.contains(command),
            "missing {command:?} in {default:?}"
        );
    }

    let invalid = binary(&["agent-preset", "unknown"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert!(
        String::from_utf8(invalid.stderr)
            .unwrap()
            .contains("unknown agent-preset command")
    );

    let wrong_output = binary(&["agent-preset", "list", "--output", "jsonl"]);
    assert_eq!(wrong_output.status.code(), Some(2));
    assert!(wrong_output.stdout.is_empty());
    assert!(
        String::from_utf8(wrong_output.stderr)
            .unwrap()
            .contains("invalid --output mode")
    );

    let positional_copy = binary(&["agent-preset", "copy", "standard", "mine"]);
    assert_eq!(positional_copy.status.code(), Some(2));
    assert!(positional_copy.stdout.is_empty());

    for arguments in [
        vec!["run", "task", "--agent-preset", "Upper"],
        vec![
            "run",
            "task",
            "--resume",
            "session-one",
            "--agent-preset",
            "standard",
        ],
        vec![
            "run",
            "task",
            "--agent-preset",
            "standard",
            "--agent-preset",
            "minimal",
        ],
    ] {
        let output = binary(&arguments);
        assert_eq!(output.status.code(), Some(2), "arguments: {arguments:?}");
        assert!(output.stdout.is_empty(), "arguments: {arguments:?}");
    }
}

#[test]
fn built_binary_rejects_a_non_absolute_configured_root() {
    let temporary = tempfile::tempdir().unwrap();
    let xdg_config = temporary.path().join("xdg-config");
    let config = xdg_config.join("rsi");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("settings.json"),
        serde_json::to_vec_pretty(&json!({
            "rsi.agent-presets": {
                "roots": [{ "path": "~/presets" }]
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_rsi"))
        .args(["agent-preset", "list"])
        .env("XDG_CONFIG_HOME", xdg_config)
        .env("XDG_STATE_HOME", temporary.path().join("xdg-state"))
        .env("XDG_CACHE_HOME", temporary.path().join("xdg-cache"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("roots[].path` must be absolute")
    );
}

struct CliFixture {
    _temporary: tempfile::TempDir,
    xdg_config: std::path::PathBuf,
    xdg_state: std::path::PathBuf,
    xdg_cache: std::path::PathBuf,
    config: std::path::PathBuf,
    configured: std::path::PathBuf,
    workspace: std::path::PathBuf,
}

impl CliFixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let xdg_config = temporary.path().join("xdg-config");
        let xdg_state = temporary.path().join("xdg-state");
        let xdg_cache = temporary.path().join("xdg-cache");
        let config = xdg_config.join("rsi");
        let configured = temporary.path().join("configured");
        let workspace = temporary.path().join("workspace");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir(&workspace).unwrap();
        fs::write(
            config.join("profile.toml"),
            r#"format = 1

[[steps]]
kind = "plugin"
id = "fixture-provider"
plugin = "rsi.ai.provider.openai-compatible"

[steps.config]
deployment = "fixture"
endpoint = "http://127.0.0.1:9"
path = "/v1/chat/completions"
allow_image_input = false
credential = { owner = "rsi.ai.provider.openai-compatible", slot = "default" }

[steps.config.language_models.fixture-model]
context_window_tokens = 128000
default_output_reserve_tokens = 4096
max_output_reserve_tokens = 16384
"#,
        )
        .unwrap();
        write_preset(
            &configured,
            "standard",
            "format = 1\n# standard-composition\n",
        );
        write_preset(
            &configured,
            "minimal",
            "format = 1\n# minimal-composition\n",
        );
        fs::create_dir_all(configured.join("broken")).unwrap();
        fs::write(
            config.join("settings.json"),
            serde_json::to_vec_pretty(&json!({
                "rsi.agent": {
                    "default_model": {
                        "deployment": "fixture",
                        "model": "fixture-model"
                    }
                },
                "rsi.agent-presets": {
                    "roots": [{ "path": configured, "trust": "system" }]
                }
            }))
            .unwrap(),
        )
        .unwrap();
        Self {
            _temporary: temporary,
            xdg_config,
            xdg_state,
            xdg_cache,
            config,
            configured,
            workspace,
        }
    }

    fn command(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_rsi"))
            .args(arguments)
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("XDG_STATE_HOME", &self.xdg_state)
            .env("XDG_CACHE_HOME", &self.xdg_cache)
            .env("RSI_OPENAI_COMPATIBLE_API_KEY", "fixture-secret")
            .output()
            .unwrap()
    }
}

fn json_output(output: Output) -> serde_json::Value {
    let Output {
        status,
        stdout,
        stderr,
    } = output;
    assert!(
        status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(stderr.is_empty());
    serde_json::from_slice(&stdout).unwrap()
}

fn assert_copied_metadata(path: &Path) {
    let copied_metadata = fs::read_to_string(path).unwrap();
    assert!(copied_metadata.contains("name = \"Mine\""));
    assert!(copied_metadata.contains("description = \"CLI source\""));
    assert!(copied_metadata.contains("order = 17"));
}

#[test]
fn built_binary_lists_shows_and_resolves_paths_in_text_and_single_document_json() {
    let fixture = CliFixture::new();

    let listed = json_output(fixture.command(&["agent-preset", "list", "--output", "json"]));
    assert_eq!(listed["version"], 1);
    assert_eq!(listed["type"], "agent_preset_list");
    assert_eq!(listed["authorable"], true);
    let standard = listed["presets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "standard")
        .unwrap();
    assert_eq!(standard.as_object().unwrap().len(), 7);
    assert!(standard["metadata"].is_object());
    assert_eq!(standard["source"], "system");
    assert_eq!(standard["trust"], "system");
    assert_eq!(standard["default"], true);
    assert_eq!(standard["status"], "healthy");
    assert_eq!(standard["reason"], serde_json::Value::Null);
    let minimal = listed["presets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "minimal")
        .unwrap();
    assert_eq!(minimal["source"], "configured");
    assert_eq!(minimal["trust"], "system");
    assert_eq!(minimal["metadata"]["name"], serde_json::Value::Null);
    assert_eq!(minimal["metadata"]["description"], serde_json::Value::Null);

    let text = fixture.command(&["agent-preset", "list"]);
    assert!(text.status.success());
    assert!(text.stderr.is_empty());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.starts_with("DEFAULT\tID\tSOURCE\tTRUST\tHEALTH\tNAME\n"));
    assert!(text.contains("*\tstandard\tsystem\tsystem\thealthy\t"));
    assert!(text.contains("\tminimal\tconfigured\tsystem\thealthy\t"));

    let shown =
        json_output(fixture.command(&["agent-preset", "show", "minimal", "--output", "json"]));
    assert_eq!(shown["type"], "agent_preset");
    assert_eq!(shown["preset"]["source"], "configured");
    assert_eq!(shown["preset"]["status"], "healthy");
    assert_eq!(shown["composition"], "format = 1\n# minimal-composition\n");

    let broken =
        json_output(fixture.command(&["agent-preset", "show", "broken", "--output", "json"]));
    assert_eq!(broken["preset"]["status"], "broken");
    assert!(broken["preset"]["reason"].as_str().is_some());
    assert_eq!(broken["composition"], serde_json::Value::Null);
    let path = fixture.command(&["agent-preset", "path", "broken"]);
    assert!(path.status.success());
    assert_eq!(
        String::from_utf8(path.stdout).unwrap(),
        format!("{}\n", fixture.configured.join("broken").display())
    );
}

#[test]
fn built_binary_does_not_render_metadata_as_extra_rows_or_terminal_controls() {
    let fixture = CliFixture::new();
    fs::write(
        fixture.configured.join("minimal/preset.toml"),
        "name = \"safe\\nforged-row\"\ndescription = \"safe\\u001b[31mred\"\n",
    )
    .unwrap();

    let listed = fixture.command(&["agent-preset", "list"]);
    assert!(listed.status.success());
    assert!(listed.stderr.is_empty());
    let listed = String::from_utf8(listed.stdout).unwrap();
    assert!(!listed.contains("forged-row"));
    assert!(!listed.contains('\u{1b}'));
    assert_eq!(
        listed
            .lines()
            .filter(|line| line.contains("\tminimal\t"))
            .count(),
        1
    );

    let shown = fixture.command(&["agent-preset", "show", "minimal"]);
    assert!(shown.status.success());
    assert!(shown.stderr.is_empty());
    let shown = String::from_utf8(shown.stdout).unwrap();
    assert!(!shown.contains("forged-row"));
    assert!(!shown.contains('\u{1b}'));
    assert!(!shown.lines().any(|line| line.starts_with("name:")));
    assert!(!shown.lines().any(|line| line.starts_with("description:")));
}

#[test]
fn built_binary_never_promotes_a_cache_sibling_to_a_system_preset() {
    let fixture = CliFixture::new();
    let initial = fixture.command(&["agent-preset", "list", "--output", "json"]);
    assert!(initial.status.success());
    let system_cache = fixture.xdg_cache.join("rsi/agent-presets/system");
    let digest_root = fs::read_dir(system_cache)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .expect("digest-addressed standard preset cache");
    write_preset(&digest_root, "forged", "format = 1\n");

    let listed = json_output(fixture.command(&["agent-preset", "list", "--output", "json"]));
    assert!(
        listed["presets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["id"] != "forged"),
        "an unverified cache sibling was promoted to System trust"
    );
}

#[test]
fn built_binary_copies_deletes_and_resolves_defaults_at_run_time() {
    let fixture = CliFixture::new();
    fs::write(
        fixture.configured.join("minimal/preset.toml"),
        "name = \"Minimal\"\ndescription = \"CLI source\"\norder = 17\n",
    )
    .unwrap();

    let copied = json_output(fixture.command(&[
        "agent-preset",
        "copy",
        "--from",
        "minimal",
        "--id",
        "mine",
        "--name",
        "Mine",
        "--output",
        "json",
    ]));
    assert_eq!(copied["action"], "copied");
    assert_eq!(copied["id"], "mine");
    assert_eq!(
        fs::read_to_string(
            fixture
                .config
                .join("agent-presets/mine")
                .join(COMPOSITION_FILE)
        )
        .unwrap(),
        "format = 1\n# minimal-composition\n"
    );
    let listed = json_output(fixture.command(&["agent-preset", "list", "--output", "json"]));
    let mine = listed["presets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "mine")
        .unwrap();
    assert_eq!(mine["source"], "user");
    assert_eq!(mine["trust"], "user");
    assert_eq!(mine["metadata"]["name"], "Mine");
    assert_eq!(mine["metadata"]["description"], "CLI source");
    assert_copied_metadata(
        &fixture
            .config
            .join("agent-presets/mine")
            .join("preset.toml"),
    );

    let selected = json_output(fixture.command(&[
        "agent-preset",
        "default",
        "set",
        "mine",
        "--output",
        "json",
    ]));
    assert_eq!(selected["type"], "agent_preset_default");
    assert_eq!(selected["action"], "set");
    assert_eq!(selected["id"], "mine");
    let default = fixture.command(&["agent-preset", "default", "get"]);
    assert!(default.status.success());
    assert_eq!(default.stdout, b"mine\n");

    let cleared =
        json_output(fixture.command(&["agent-preset", "default", "clear", "--output", "json"]));
    assert_eq!(cleared["action"], "clear");
    assert_eq!(cleared["id"], "standard");
    let default = fixture.command(&["agent-preset", "default", "get"]);
    assert!(default.status.success());
    assert_eq!(default.stdout, b"standard\n");

    let selected = fixture.command(&["agent-preset", "default", "set", "mine"]);
    assert!(selected.status.success());

    let deleted = fixture.command(&["agent-preset", "delete", "mine"]);
    assert!(deleted.status.success());
    assert_eq!(deleted.stdout, b"deleted mine\n");
    assert!(!fixture.config.join("agent-presets/mine").exists());
    let default = fixture.command(&["agent-preset", "default", "get"]);
    assert!(default.status.success());
    assert_eq!(default.stdout, b"standard\n");

    let future_default = json_output(fixture.command(&[
        "agent-preset",
        "default",
        "set",
        "future-agent",
        "--output",
        "json",
    ]));
    assert_eq!(future_default["action"], "set");
    assert_eq!(future_default["id"], "future-agent");
    let default = fixture.command(&["agent-preset", "default", "get"]);
    assert!(default.status.success());
    assert_eq!(default.stdout, b"future-agent\n");

    for preset_arguments in [vec!["--agent-preset", "future-agent"], Vec::<&str>::new()] {
        let mut arguments = vec!["run", "task", "--cwd", fixture.workspace.to_str().unwrap()];
        arguments.extend(preset_arguments);
        let run = fixture.command(&arguments);
        assert_eq!(run.status.code(), Some(2));
        assert!(run.stdout.is_empty());
        let error = String::from_utf8(run.stderr).unwrap();
        assert!(error.contains("future-agent"), "error: {error}");
        assert!(error.contains("unavailable"), "error: {error}");
    }
}
