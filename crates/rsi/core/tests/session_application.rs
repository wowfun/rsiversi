#![cfg(target_os = "linux")]

use axum::{Router, body::Body, http::StatusCode, response::Response, routing::post};
use futures_util::StreamExt as _;
use rsi::{
    HostProfileDocument, HostProfileId, ProfileCatalog, ProfileSource, RunningRsi,
    SessionHostConnectionMode, StandardCodingTools, StandardComposition, StandardSessionDaemon,
    connect_or_embed_session_host,
};
use rsi_agent_session_protocol::{SessionFactBody, SessionId, TurnId};
use rsi_credentials_local::SecretStore;
use rsi_credentials_protocol::{CredentialsError, Result as CredentialResult, SecretValue};
use rsi_host::HostPaths;
use rsi_session::{CreateSession, SessionApplication, SessionApplicationError, SubmitText};
use rsi_session_host::{
    HostEpoch, HostOwnerLease, HostOwnerMetadata, HostOwnerMode, SessionHostPaths,
    UdsSessionApplication, UdsSessionServer,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

const KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

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
    running.session_application().unwrap();
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

async fn assert_session_application_contract(
    application: Arc<dyn SessionApplication>,
    workspace: &std::path::Path,
    session: &str,
) {
    let session_id = SessionId::new(session).unwrap();
    let handle = application
        .create(CreateSession {
            cwd: workspace.to_owned(),
            session_id: Some(session_id.clone()),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    let header = handle.header().await.unwrap();
    assert_eq!(header.session_id(), &session_id);
    assert_eq!(
        header.canonical_cwd(),
        std::fs::canonicalize(workspace).unwrap().to_str().unwrap()
    );
    assert!(
        handle
            .history_before(None, 8)
            .await
            .unwrap()
            .facts
            .is_empty()
    );
    assert!(matches!(
        handle.history_before(None, 0).await,
        Err(SessionApplicationError::Invalid(_))
    ));

    let turn_id = TurnId::new(format!("{session}-turn")).unwrap();
    let submission = SubmitText {
        turn_id: turn_id.clone(),
        text: "hello contract".into(),
        model: None,
        sandbox: None,
    };
    let first = handle.submit_text(submission.clone()).await.unwrap();
    let mut observation = handle.subscribe(first.accepted_seq).await.unwrap();
    let retried = handle.submit_text(submission).await.unwrap();
    assert_eq!(first, retried);
    assert!(matches!(
        handle
            .submit_text(SubmitText {
                turn_id: turn_id.clone(),
                text: "changed body".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(SessionApplicationError::Conflict { .. })
    ));

    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let update = observation.next().await.unwrap().unwrap();
            if matches!(
                update,
                rsi_agent_turn_protocol::TurnUpdate::Fact { ref fact, .. }
                    if matches!(fact.body(), SessionFactBody::TurnTerminal { turn_id: observed, .. } if observed == &turn_id)
            ) {
                break;
            }
        }
    })
    .await
    .expect("turn reached a durable terminal Fact");

    let history = handle.history_before(None, 64).await.unwrap();
    assert!(history.durable_seq >= first.accepted_seq);
    assert!(history.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::TurnTerminal { turn_id: observed, .. } if observed == &turn_id
    )));
    let attached = application.attach(&session_id).await.unwrap();
    assert_eq!(attached.header().await.unwrap(), header);
    assert!(
        application
            .list_recent(None, 64)
            .await
            .unwrap()
            .sessions
            .iter()
            .any(|summary| summary.header.session_id() == &session_id)
    );
    assert!(matches!(
        application
            .attach(&SessionId::new(format!("{session}-missing")).unwrap())
            .await,
        Err(SessionApplicationError::NotFound(_))
    ));
    assert!(matches!(
        application
            .create(CreateSession {
                cwd: workspace.to_owned(),
                session_id: Some(session_id),
                agent_preset_id: None,
            })
            .await,
        Err(SessionApplicationError::Invalid(message))
            if message.contains("already exists") && message.contains("durable Store")
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_and_uds_adapters_pass_one_real_kernel_store_contract() {
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let local = Arc::new(running.session_application().unwrap());
    assert_session_application_contract(
        local.clone() as Arc<dyn SessionApplication>,
        &fixture.workspace,
        "local-contract",
    )
    .await;

    let host_paths = SessionHostPaths::from_host_paths_with_runtime(
        &fixture.paths,
        Some(&fixture.temporary.path().join("runtime")),
    )
    .unwrap();
    let epoch = HostEpoch::generate().unwrap();
    let server = UdsSessionServer::bind(&host_paths, local, KEY, epoch.clone()).unwrap();
    let cancellation = CancellationToken::new();
    let server_task = tokio::spawn(server.serve(cancellation.clone()));
    let remote = Arc::new(
        UdsSessionApplication::connect(host_paths.socket(), KEY, epoch)
            .await
            .unwrap(),
    );
    assert_session_application_contract(
        remote as Arc<dyn SessionApplication>,
        &fixture.workspace,
        "uds-contract",
    )
    .await;

    cancellation.cancel();
    server_task.await.unwrap().unwrap();
    assert!(running.shutdown().await.is_clean());
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
            "isolated Session Host selection failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }

    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let profile = host_profile(&fixture);

    let embedded = connect_or_embed_session_host(composition(fixture.paths.clone()), &profile)
        .await
        .unwrap();
    assert_eq!(embedded.mode(), SessionHostConnectionMode::Embedded);
    embedded.shutdown().await.unwrap();

    let daemon = StandardSessionDaemon::start(composition(fixture.paths.clone()), &profile)
        .await
        .unwrap();
    let cancellation = CancellationToken::new();
    let daemon_task = tokio::spawn(daemon.run(cancellation.clone()));
    let remote = connect_or_embed_session_host(composition(fixture.paths.clone()), &profile)
        .await
        .unwrap();
    assert_eq!(remote.mode(), SessionHostConnectionMode::Remote);
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
    let remote = connect_or_embed_session_host(different_environment, &profile)
        .await
        .unwrap();
    assert_eq!(remote.mode(), SessionHostConnectionMode::Remote);
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
    let error = connect_or_embed_session_host(incompatible, &profile)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("different launch identity"));

    cancellation.cancel();
    daemon_task.await.unwrap().unwrap();
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
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let application = Arc::new(running.session_application().unwrap());
    let paths = SessionHostPaths::from_host_paths_with_runtime(
        &fixture.paths,
        Some(&fixture.temporary.path().join("runtime")),
    )
    .unwrap();
    std::fs::create_dir_all(paths.runtime_directory()).unwrap();
    let placeholder = tokio::net::UnixListener::bind(paths.socket()).unwrap();
    let epoch = HostEpoch::generate().unwrap();
    let mut lease = HostOwnerLease::try_acquire(paths.clone()).unwrap();
    lease
        .publish(
            &HostOwnerMetadata::current(
                HostOwnerMode::Daemon,
                launch_key.clone(),
                epoch.clone(),
                Some(paths.socket().to_owned()),
            )
            .unwrap(),
        )
        .unwrap();

    let connecting_profile = profile.clone();
    let connecting =
        tokio::spawn(
            async move { connect_or_embed_session_host(candidate, &connecting_profile).await },
        );
    let (first_stream, _) =
        tokio::time::timeout(std::time::Duration::from_secs(2), placeholder.accept())
            .await
            .expect("application selection did not probe the published endpoint")
            .unwrap();
    drop(first_stream);
    drop(placeholder);
    std::fs::remove_file(paths.socket()).unwrap();

    let server = UdsSessionServer::bind(
        &paths,
        application as Arc<dyn SessionApplication>,
        launch_key,
        epoch,
    )
    .unwrap();
    let cancellation = CancellationToken::new();
    let server_task = tokio::spawn(server.serve(cancellation.clone()));
    let connection = connecting.await.unwrap().unwrap();
    assert_eq!(connection.mode(), SessionHostConnectionMode::Remote);
    connection.shutdown().await.unwrap();

    cancellation.cancel();
    server_task.await.unwrap().unwrap();
    drop(lease);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}
