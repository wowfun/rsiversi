#![cfg(target_os = "linux")]

use axum::{
    Json, Router, body::Body, extract::State, http::StatusCode, response::Response, routing::post,
};
use futures_util::StreamExt as _;
use rsi::{
    HostProfileDocument, HostProfileId, ProfileCatalog, ProfileSource, RunningRsi,
    ServiceHostConnectionMode, StandardCodingTools, StandardComposition, StandardServiceDaemon,
    connect_or_embed_service_host,
};
use rsi_agent_session_protocol::{MessageId, SessionFactBody, SessionId, TurnId, WorkspaceTrust};
use rsi_agent_turn_protocol::{MessageReceipt, ObservationCursor, SessionObservation};
use rsi_api_protocol::HostEpoch;
use rsi_credentials_local::SecretStore;
use rsi_credentials_protocol::{CredentialsError, Result as CredentialResult, SecretValue};
use rsi_host::HostPaths;
use rsi_service_host::{HostOwnerLease, HostOwnerMetadata, HostOwnerMode, ServiceHostPaths};
use rsi_session_protocol::{
    CreateSession, SessionError, SessionInput, SessionService, SubmitInput,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

const KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[path = "session_service/plan_policy.rs"]
mod plan_policy;
#[path = "session_service/session_api.rs"]
mod session_api;
#[path = "session_service/standard_api.rs"]
mod standard_api;

#[derive(Debug)]
struct EmptySecretStore;

impl SecretStore for EmptySecretStore {
    fn get(&self, _service: &str, _account: &str) -> CredentialResult<Option<SecretValue>> {
        Ok(None)
    }

    fn set(&self, _service: &str, _account: &str, _secret: &SecretValue) -> CredentialResult<()> {
        Err(CredentialsError::Store("read-only test store".into()))
    }

    fn unset(&self, _service: &str, _account: &str) -> CredentialResult<bool> {
        Err(CredentialsError::Store("read-only test store".into()))
    }
}

struct Fixture {
    temporary: TempDir,
    paths: HostPaths,
    profile: std::path::PathBuf,
    workspace: std::path::PathBuf,
}

fn fixture(endpoint: &str) -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let config = temporary.path().join("config");
    let state = temporary.path().join("state");
    let cache = temporary.path().join("cache");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        config.join("settings.json"),
        serde_json::to_vec(&serde_json::json!({
            "rsi.agent": {
                "default_model": {"deployment": "fixture", "model": "fixture-model"}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let profile_directory = config.join("host-profiles/fixture");
    std::fs::create_dir_all(&profile_directory).unwrap();
    let profile = profile_directory.join("host.profile.toml");
    std::fs::write(
        &profile,
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
    Fixture {
        paths: HostPaths::new(config, state, cache).unwrap(),
        profile,
        workspace,
        temporary,
    }
}

fn composition(paths: HostPaths) -> StandardComposition {
    let coding = StandardCodingTools::new(
        std::fs::canonicalize("/bin/bash").unwrap(),
        std::env::current_exe().unwrap().canonicalize().unwrap(),
        vec![("PATH".into(), "/usr/bin:/bin".into())],
    )
    .unwrap();
    StandardComposition::new(
        paths,
        BTreeMap::from([(
            "RSI_OPENAI_COMPATIBLE_API_KEY".into(),
            SecretValue::new("fixture-secret").unwrap(),
        )]),
        Some(coding),
    )
    .with_credential_store(Arc::new(EmptySecretStore))
}

fn host_profile(fixture: &Fixture) -> HostProfileDocument {
    HostProfileDocument {
        id: HostProfileId::new("fixture").unwrap(),
        source: ProfileSource::User,
        path: Some(fixture.profile.clone()),
        contents: std::fs::read(&fixture.profile).unwrap(),
    }
}

struct DaemonFixture {
    running: Arc<RunningRsi>,
    diagnostics: tokio::sync::watch::Receiver<rsi_service_host::ServiceHostDiagnostics>,
    connection: rsi::ServiceHostConnection,
    client_runtime: rsi_meta::Runtime,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<rsi::Result<()>>,
}
impl DaemonFixture {
    async fn new(fixture: &Fixture) -> Self {
        let profile = host_profile(fixture);
        let owner =
            HostOwnerLease::try_acquire(ServiceHostPaths::from_host_paths(&fixture.paths).unwrap())
                .unwrap();
        let daemon =
            StandardServiceDaemon::start(composition(fixture.paths.clone()), &profile, owner)
                .await
                .unwrap();
        let running = daemon.running();
        let diagnostics = daemon.diagnostics();
        let stop = CancellationToken::new();
        let task = tokio::spawn(daemon.run(stop.clone()));
        let client_runtime = rsi_meta::Runtime::default();
        let connection = connect_or_embed_service_host(
            &client_runtime.root(),
            composition(fixture.paths.clone()),
            &profile,
        )
        .await
        .unwrap();
        assert_eq!(connection.mode(), ServiceHostConnectionMode::Remote);
        Self {
            running,
            diagnostics,
            connection,
            client_runtime,
            stop,
            task,
        }
    }
    async fn shutdown(self) {
        self.connection.shutdown().await.unwrap();
        assert!(self.client_runtime.shutdown().await.is_clean());
        self.stop.cancel();
        self.task.await.unwrap().unwrap();
        assert!(self.running.shutdown().await.is_clean());
    }
}

async fn chat() -> Response {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .unwrap()
}

async fn provider() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/chat/completions", post(chat)),
        )
        .await
        .unwrap();
    });
    (format!("http://{address}"), task)
}

async fn capturing_chat(
    State(requests): State<Arc<Mutex<Vec<serde_json::Value>>>>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    requests.lock().unwrap().push(request);
    chat().await
}

async fn capturing_provider() -> (
    String,
    Arc<Mutex<Vec<serde_json::Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let task = tokio::spawn({
        let requests = Arc::clone(&requests);
        async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/chat/completions", post(capturing_chat))
                    .with_state(requests),
            )
            .await
            .unwrap();
        }
    });
    (format!("http://{address}"), requests, task)
}

async fn observe_message_claim(
    handle: &Arc<dyn rsi_session_protocol::SessionHandle>,
    receipt: &MessageReceipt,
) -> (TurnId, u64) {
    rsi_session_testkit::observe_message_claim(
        handle,
        receipt,
        &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
    )
    .await
}

async fn run_message_to_terminal(
    handle: &Arc<dyn rsi_session_protocol::SessionHandle>,
    message_id: &str,
) {
    let message_id = MessageId::new(message_id).unwrap();
    let receipt = handle
        .submit(SubmitInput {
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            message_id: message_id.clone(),
            content: vec![SessionInput::Text {
                text: "inspect workspace context".into(),
            }],
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let (turn_id, entered_fact_seq) = observe_message_claim(handle, &receipt).await;
    let mut observation = handle
        .observe(ObservationCursor {
            control_seq: receipt.accepted_control_seq,
            fact_seq: entered_fact_seq,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if matches!(
                observation.next().await.unwrap().unwrap(),
                SessionObservation::Fact { fact, .. }
                    if matches!(fact.body(), SessionFactBody::TurnTerminal { turn_id: observed, .. } if observed == &turn_id)
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn built_in_standard_host_profile_boots_the_real_product_composition() {
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let profile = ProfileCatalog::new(fixture.paths.clone())
        .host(&HostProfileId::new("standard").unwrap())
        .unwrap();
    assert_eq!(profile.source, ProfileSource::Builtin);
    let running = RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &profile)
        .await
        .unwrap();
    let first = running.session_service().unwrap();
    let second = running.session_service().unwrap();
    assert!(
        Arc::ptr_eq(&first, &second),
        "clients share one plugin-owned Session generation"
    );
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standard_plan_commands_reach_the_actual_projection_and_provider_context() {
    use futures_util::StreamExt as _;
    use rsi_agent_session_protocol::{CommandArguments, DomainRequestId, SessionCommandInvocation};
    let (endpoint, requests, provider) = capturing_provider().await;
    let fixture = fixture(&endpoint);
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let application = running.session_service().unwrap();
    let handle = application
        .create(CreateSession {
            workspace_id: running
                .workspace_registry()
                .unwrap()
                .get_or_create(&fixture.workspace)
                .await
                .unwrap()
                .id,
            session_id: SessionId::new("standard-plan-mode").unwrap(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    for (id, argument, enabled) in [("plan-on", "on", true), ("plan-off", "off", false)] {
        let catalog = handle.commands().await.unwrap();
        let command = catalog
            .commands()
            .iter()
            .find(|entry| entry.name() == "plan")
            .unwrap();
        let request_id = DomainRequestId::new(id).unwrap();
        let receipt = handle
            .execute_command(SessionCommandInvocation {
                command: command.id().clone(),
                request_id: request_id.clone(),
                expected_revision: catalog.revision(),
                arguments: CommandArguments::new(argument.into()).unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(
            handle.command_status(&request_id).await.unwrap(),
            Some(receipt)
        );
        let mut views = handle.observe_projections().await.unwrap();
        let view = views.next().await.unwrap().unwrap();
        assert_eq!(
            view.snapshot()
                .entries()
                .iter()
                .find(|entry| entry.producer().as_str() == "rsi.plan-policy.view")
                .unwrap()
                .view()
                .unwrap()
                .value()["enabled"],
            enabled
        );
        drop(views);
        run_message_to_terminal(&handle, id).await;
        let requests = requests.lock().unwrap();
        let latest = requests.last().unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .map(serde_json::Value::to_string)
            .find(|message| message.contains("Plan mode is "))
            .unwrap();
        assert!(latest.contains(if enabled {
            "Plan mode is enabled."
        } else {
            "Plan mode is disabled."
        }));
    }
    assert_eq!(requests.lock().unwrap().len(), 2);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standard_product_applies_project_instructions_only_with_durable_workspace_trust() {
    let (endpoint, requests, provider) = capturing_provider().await;
    let fixture = fixture(&endpoint);
    std::fs::create_dir(fixture.workspace.join(".git")).unwrap();
    std::fs::write(
        fixture.workspace.join("AGENTS.md"),
        "TRUSTED_PROJECT_INSTRUCTION_MARKER",
    )
    .unwrap();
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let application = running.session_service().unwrap();
    let trusted = application
        .create(CreateSession {
            workspace_id: running
                .workspace_registry()
                .unwrap()
                .get_or_create(&fixture.workspace)
                .await
                .unwrap()
                .id,
            session_id: SessionId::new("trusted-workspace-context").unwrap(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Trusted,
        })
        .await
        .unwrap();
    run_message_to_terminal(&trusted, "trusted-workspace-message").await;
    let untrusted = application
        .create(CreateSession {
            workspace_id: running
                .workspace_registry()
                .unwrap()
                .get_or_create(&fixture.workspace)
                .await
                .unwrap()
                .id,
            session_id: SessionId::new("untrusted-workspace-context").unwrap(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    run_message_to_terminal(&untrusted, "untrusted-workspace-message").await;

    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0]
                .to_string()
                .contains("TRUSTED_PROJECT_INSTRUCTION_MARKER")
        );
        assert!(
            !requests[1]
                .to_string()
                .contains("TRUSTED_PROJECT_INSTRUCTION_MARKER")
        );
    }
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

async fn assert_session_service_contract(
    application: Arc<dyn SessionService>,
    model_catalog: Arc<dyn rsi_ai_protocol::LanguageModels>,
    registry: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    workspace: &std::path::Path,
    session: &str,
) {
    let models = model_catalog.list_models(None, 1).await.unwrap();
    assert_eq!(
        models.models,
        vec![rsi_ai_protocol::ModelRef::new("fixture", "fixture-model").unwrap()]
    );
    assert!(!models.has_more);
    assert!(
        model_catalog
            .list_models(models.models.last(), 1)
            .await
            .unwrap()
            .models
            .is_empty()
    );
    assert!(model_catalog.list_models(None, 0).await.is_err());
    assert!(model_catalog.list_models(None, 257).await.is_err());
    rsi_session_testkit::assert_session_contract(
        application,
        CreateSession {
            workspace_id: registry.get_or_create(workspace).await.unwrap().id,
            session_id: SessionId::new(session).unwrap(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        },
        workspace.canonicalize().unwrap().to_str().unwrap(),
        &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_and_uds_adapters_pass_one_real_kernel_store_contract() {
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let daemon = DaemonFixture::new(&fixture).await;
    let running = &daemon.running;
    let local = running.session_service().unwrap();
    assert_session_service_contract(
        local.clone() as Arc<dyn SessionService>,
        running.language_models().unwrap(),
        running.workspace_registry().unwrap(),
        &fixture.workspace,
        "local-contract",
    )
    .await;

    let remote = daemon.connection.session_service();
    let shared = CreateSession {
        workspace_id: running
            .workspace_registry()
            .unwrap()
            .get_or_create(&fixture.workspace)
            .await
            .unwrap()
            .id,
        session_id: SessionId::new("shared-draft").unwrap(),
        agent_preset_id: None,
        workspace_trust: WorkspaceTrust::Untrusted,
    };
    let (in_process, over_socket) =
        tokio::join!(local.create(shared.clone()), remote.create(shared.clone()));
    let in_process = in_process.unwrap();
    assert_eq!(
        in_process.header().await.unwrap(),
        over_socket.unwrap().header().await.unwrap()
    );
    let mut conflict = shared;
    conflict.workspace_trust = WorkspaceTrust::Trusted;
    assert!(matches!(
        remote.create(conflict).await,
        Err(SessionError::DraftConflict { .. })
    ));
    assert_session_service_contract(
        remote.clone() as Arc<dyn SessionService>,
        daemon.connection.language_models(),
        daemon.connection.workspace_registry(),
        &fixture.workspace,
        "uds-contract",
    )
    .await;

    daemon.shutdown().await;
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn application_selection_uses_a_compatible_daemon_and_embeds_only_without_an_owner() {
    const ISOLATED_RUNTIME_CHILD: &str = "RSI_SESSION_APPLICATION_RUNTIME_CHILD";
    if std::env::var_os(ISOLATED_RUNTIME_CHILD).is_none() {
        let runtime = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env(ISOLATED_RUNTIME_CHILD, "1")
            .env("XDG_RUNTIME_DIR", runtime.path())
            .args([
                "--exact",
                "application_selection_uses_a_compatible_daemon_and_embeds_only_without_an_owner",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated Service Host selection failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }

    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let profile = host_profile(&fixture);

    let client_runtime = rsi_meta::Runtime::default();
    let embedded = connect_or_embed_service_host(
        &client_runtime.root(),
        composition(fixture.paths.clone()),
        &profile,
    )
    .await
    .unwrap();
    assert_eq!(embedded.mode(), ServiceHostConnectionMode::Embedded);
    embedded.shutdown().await.unwrap();

    let owner = rsi_service_host::HostOwnerLease::try_acquire(
        rsi_service_host::ServiceHostPaths::from_host_paths(&fixture.paths).unwrap(),
    )
    .unwrap();
    let daemon = StandardServiceDaemon::start(composition(fixture.paths.clone()), &profile, owner)
        .await
        .unwrap();
    let cancellation = CancellationToken::new();
    let daemon_task = tokio::spawn(daemon.run(cancellation.clone()));
    let remote = connect_or_embed_service_host(
        &client_runtime.root(),
        composition(fixture.paths.clone()),
        &profile,
    )
    .await
    .unwrap();
    assert_eq!(remote.mode(), ServiceHostConnectionMode::Remote);
    remote.shutdown().await.unwrap();

    let different_environment = StandardComposition::new(
        fixture.paths.clone(),
        BTreeMap::from([(
            "RSI_OPENAI_COMPATIBLE_API_KEY".into(),
            SecretValue::new("fixture-secret").unwrap(),
        )]),
        Some(
            StandardCodingTools::new(
                std::fs::canonicalize("/bin/bash").unwrap(),
                std::env::current_exe().unwrap().canonicalize().unwrap(),
                vec![("PATH".into(), "/different".into())],
            )
            .unwrap(),
        ),
    )
    .with_credential_store(Arc::new(EmptySecretStore));
    let remote =
        connect_or_embed_service_host(&client_runtime.root(), different_environment, &profile)
            .await
            .unwrap();
    assert_eq!(remote.mode(), ServiceHostConnectionMode::Remote);
    remote.shutdown().await.unwrap();

    let incompatible = StandardComposition::new(
        fixture.paths.clone(),
        BTreeMap::from([(
            "RSI_OPENAI_COMPATIBLE_API_KEY".into(),
            SecretValue::new("fixture-secret").unwrap(),
        )]),
        None,
    )
    .with_credential_store(Arc::new(EmptySecretStore));
    let error = connect_or_embed_service_host(&client_runtime.root(), incompatible, &profile)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("different launch identity"));

    cancellation.cancel();
    daemon_task.await.unwrap().unwrap();
    assert!(client_runtime.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn application_selection_retries_a_live_daemon_until_its_endpoint_recovers() {
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let profile = host_profile(&fixture);
    let candidate = composition(fixture.paths.clone());
    let launch_key = candidate
        .preview_host(&profile)
        .unwrap()
        .launch_key
        .as_str()
        .to_owned();
    let paths = ServiceHostPaths::from_host_paths_with_runtime(
        &fixture.paths,
        Some(&fixture.temporary.path().join("runtime")),
    )
    .unwrap();
    std::fs::create_dir_all(paths.runtime_directory()).unwrap();
    let placeholder = tokio::net::UnixListener::bind(paths.socket()).unwrap();
    let epoch = HostEpoch::generate().unwrap();
    let lease = Arc::new(HostOwnerLease::try_acquire(paths.clone()).unwrap());
    let running = RunningRsi::boot(
        composition(fixture.paths.clone())
            .with_service_owner(lease.clone(), epoch.clone())
            .unwrap(),
        &fixture.profile,
    )
    .await
    .unwrap();
    lease
        .publish(
            &HostOwnerMetadata::current(
                HostOwnerMode::Daemon,
                launch_key.clone(),
                epoch.clone(),
                running
                    .connection_description()
                    .unwrap()
                    .endpoint_id
                    .clone(),
                Some(paths.socket().to_owned()),
            )
            .unwrap(),
        )
        .unwrap();

    let client_runtime = rsi_meta::Runtime::default();
    let parent = client_runtime.root();
    let connecting_profile = profile.clone();
    let connecting = tokio::spawn(async move {
        connect_or_embed_service_host(&parent, candidate, &connecting_profile).await
    });
    let (first_stream, _) =
        tokio::time::timeout(std::time::Duration::from_secs(2), placeholder.accept())
            .await
            .expect("application selection did not probe the published endpoint")
            .unwrap();
    drop(first_stream);
    drop(placeholder);
    std::fs::remove_file(paths.socket()).unwrap();

    let service = rsi_api_http::LocalHttpService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        running.api_dispatch().unwrap(),
        &running.connection_description().unwrap(),
        rsi_service_host::local_compatibility_key(&launch_key).unwrap(),
    )
    .unwrap();
    let server = rsi_service_host::LocalApiServer::bind(lease.clone(), service).unwrap();
    let cancellation = CancellationToken::new();
    let server_task = tokio::spawn(server.serve(cancellation.clone()));
    let connection = connecting.await.unwrap().unwrap();
    assert_eq!(connection.mode(), ServiceHostConnectionMode::Remote);
    connection.shutdown().await.unwrap();
    assert!(client_runtime.shutdown().await.is_clean());

    cancellation.cancel();
    server_task.await.unwrap().unwrap();
    drop(lease);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

async fn assert_workspace_domain(registry: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>) {
    use rsi_workspace_protocol::{WorkspaceError, WorkspaceStatus};
    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    let marker = first_dir.path().join("keep.txt");
    std::fs::write(&marker, "user-owned").unwrap();
    let first = registry.get_or_create(first_dir.path()).await.unwrap();
    let second = registry.get_or_create(second_dir.path()).await.unwrap();
    assert_eq!(registry.get(&first.id).await.unwrap(), first);
    assert_eq!(
        registry
            .get_or_create(&first_dir.path().join("."))
            .await
            .unwrap(),
        first
    );
    let page = registry.list(None, 1).await.unwrap();
    assert_eq!(page.records, vec![first.clone()]);
    let cursor = page.next.unwrap();
    assert!(registry.delete_registration(&first.id).await.unwrap());
    assert!(!registry.delete_registration(&first.id).await.unwrap());
    assert!(
        matches!(registry.get(&first.id).await, Err(WorkspaceError::Unknown(id)) if id == first.id)
    );
    let next = registry.list(Some(cursor), 1).await.unwrap();
    assert_eq!(next.records, vec![second.clone()]);
    assert!(next.next.is_none());
    second_dir.close().unwrap();
    assert_eq!(registry.get(&second.id).await.unwrap(), second);
    assert_eq!(
        registry.status(&second.id).await.unwrap(),
        WorkspaceStatus::MissingDirectory
    );
    assert_eq!(std::fs::read(&marker).unwrap(), b"user-owned");
    assert!(registry.list(None, 0).await.is_err());
    assert!(registry.list(None, 257).await.is_err());
    assert!(registry.delete_registration(&second.id).await.unwrap());
    assert!(registry.list(None, 256).await.unwrap().records.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_and_uds_workspace_operations_are_independent_of_session_creation() {
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let daemon = DaemonFixture::new(&fixture).await;
    let running = &daemon.running;
    let sessions = running.session_service().unwrap();
    assert_workspace_domain(running.workspace_registry().unwrap()).await;
    let remote = daemon.connection.workspace_registry();
    assert_workspace_domain(remote).await;
    assert!(
        sessions
            .list_recent(None, 256)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    daemon.shutdown().await;
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_media_upload_survives_message_rejection_and_host_restart() {
    use base64::Engine as _;
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let daemon = DaemonFixture::new(&fixture).await;
    let running = &daemon.running;
    let remote = daemon.connection.media_service();
    let sessions = daemon.connection.session_service();
    let source: bytes::Bytes = bytes::Bytes::from(base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=").unwrap());
    let reference = remote.import_image(source.clone()).await.unwrap();
    assert_eq!(remote.import_image(source).await.unwrap(), reference);
    let canonical = remote.read(&reference).await.unwrap();
    assert_eq!((reference.width, reference.height), (1, 1));
    assert!(
        sessions
            .list_recent(None, 16)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    let handle = sessions
        .create(CreateSession {
            workspace_id: running
                .workspace_registry()
                .unwrap()
                .get_or_create(&fixture.workspace)
                .await
                .unwrap()
                .id,
            session_id: SessionId::new("failed-media-message").unwrap(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    let result = handle
        .submit(SubmitInput {
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            message_id: MessageId::new("rejected-media-message").unwrap(),
            content: vec![SessionInput::Image {
                media: reference.clone(),
            }],
            model: Some(rsi_ai_protocol::ModelRef::new("missing", "route").unwrap()),
            sandbox: None,
        })
        .await;
    assert!(matches!(result, Err(SessionError::Invalid(_))));
    assert_eq!(
        remote.read(&reference).await.unwrap().bytes,
        canonical.bytes
    );
    assert!(
        sessions
            .list_recent(None, 16)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    daemon.shutdown().await;
    let restarted = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    assert_eq!(
        restarted
            .media_service()
            .unwrap()
            .read(&reference)
            .await
            .unwrap()
            .bytes,
        canonical.bytes
    );
    assert!(restarted.shutdown().await.is_clean());
    provider.abort();
}
