use super::{composition, fixture, host_profile};
use rsi_ai_protocol::LanguageModels;
use rsi_api_protocol::{
    ApiClient, ApiDispatchContract, ConnectionDescriptionContract, DeviceAdministrationContract,
    DeviceAuthenticationContract,
};
use rsi_credentials_protocol::CredentialRef;
use rsi_session_protocol::SessionService;
use rsi_settings_protocol::SettingsAccess;
use rsi_workspace_protocol::WorkspaceRegistry;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_executor_reload_preserves_listener_diagnostics_owner_and_retained_clients() {
    let fixture = fixture("http://127.0.0.1:1");
    let mut daemon = super::DaemonFixture::new(&fixture).await;
    let identity = daemon.running.connection_description().unwrap();
    let source = std::fs::read_to_string(&fixture.profile).unwrap();
    let client = daemon.connection.language_models();
    for maximum_active_turns in [3, 2] {
        let previous = daemon.diagnostics.borrow_and_update().clone();
        std::fs::write(&fixture.profile, format!("{source}\n[[steps]]\nkind = 'patch'\ntarget = 'rsi-agent-executor'\nconfig = {{ executor_id = 'rsi-agent-executor', maximum_active_turns = {maximum_active_turns} }}\n")).unwrap();
        let outcome = daemon.running.reload().await.unwrap();
        assert!(matches!(
            outcome,
            rsi_host::ReloadOutcome::Applied(_) | rsi_host::ReloadOutcome::Unchanged(_)
        ));
        assert!(!daemon.task.is_finished());
        assert_eq!(daemon.running.connection_description().unwrap(), identity);
        let before = previous.snapshot();
        assert_eq!(client.list_models(None, 16).await.unwrap().models.len(), 1);
        assert!(
            daemon
                .diagnostics
                .borrow_and_update()
                .snapshot()
                .accepted_connections
                > 0
        );
        assert!(
            previous.snapshot().accepted_connections > before.accepted_connections,
            "the retained diagnostic must observe new calls on the same listener"
        );
        assert!(
            !daemon.diagnostics.has_changed().unwrap(),
            "executor reload replaced listener diagnostics"
        );
    }
    std::fs::write(
        &fixture.profile,
        "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'invalid'\nplugin = 'unknown.factory'\n",
    )
    .unwrap();
    assert!(daemon.running.reload().await.is_err());
    assert_eq!(client.list_models(None, 16).await.unwrap().models.len(), 1);
    assert!(!daemon.task.is_finished());
    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retained_local_and_uds_model_clients_follow_a_changed_provider_profile() {
    let fixture = fixture("http://127.0.0.1:1");
    let daemon = super::DaemonFixture::new(&fixture).await;
    let clients: [Arc<dyn LanguageModels>; 2] = [
        daemon.running.language_models().unwrap(),
        daemon.connection.language_models(),
    ];
    let identity = daemon.running.connection_description().unwrap();
    for client in &clients {
        assert_eq!(
            client.list_models(None, 16).await.unwrap().models,
            vec![rsi_ai_protocol::ModelRef::new("fixture", "fixture-model").unwrap()]
        );
    }
    let source = std::fs::read_to_string(&fixture.profile).unwrap();
    std::fs::write(
        &fixture.profile,
        source.replace(
            "language_models.fixture-model",
            "language_models.replacement-model",
        ),
    )
    .unwrap();
    daemon.running.reload().await.unwrap();
    assert_eq!(daemon.running.connection_description().unwrap(), identity);
    for client in &clients {
        assert_eq!(
            client.list_models(None, 16).await.unwrap().models,
            vec![rsi_ai_protocol::ModelRef::new("fixture", "replacement-model").unwrap()]
        );
    }
    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_service_profiles_share_an_application_runtime_with_isolated_lifetimes() {
    let runtime = rsi_meta::Runtime::default();
    let first = fixture("http://127.0.0.1:1");
    let second = fixture("http://127.0.0.1:1");
    let one = rsi::RunningRsi::boot_host_profile_in(
        composition(first.paths.clone()),
        &host_profile(&first),
        &runtime.root(),
    )
    .await
    .unwrap();
    let two = rsi::RunningRsi::boot_host_profile_in(
        composition(second.paths.clone()),
        &host_profile(&second),
        &runtime.root(),
    )
    .await
    .unwrap();
    let inspection = one.inspect(rsi_meta::InspectionRequest::default()).unwrap();
    let other = two.inspect(rsi_meta::InspectionRequest::default()).unwrap();
    assert!(inspection.runtime.resources.is_none());
    assert!(other.runtime.resources.is_none());
    assert!(inspection.runtime.total_fibers < runtime.snapshot().fibers.len());
    assert!(inspection.runtime.fibers.iter().all(|fiber| {
        other
            .runtime
            .fibers
            .iter()
            .all(|other| fiber.id != other.id)
    }));
    assert!(!inspection.profile.nodes().is_empty());
    assert_ne!(
        one.connection_description().unwrap().endpoint_id,
        two.connection_description().unwrap().endpoint_id
    );
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_session_protocol::SessionContract>()
            .is_none()
    );
    assert!(
        runtime
            .root()
            .lookup_local::<ConnectionDescriptionContract>()
            .is_none()
    );
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_session_protocol::SessionReadContract>()
            .is_none()
    );
    let service_instances = || {
        runtime.snapshot().fibers.into_iter().filter(|fiber| {
        fiber.state == rsi_meta::FiberState::Active &&
        matches!(&fiber.factory, rsi_meta::FactoryIdentity::Linked { plugin, .. } if plugin.as_str() == "rsi.session")
    }).count()
    };
    assert_eq!(
        service_instances(),
        2,
        "both real Session plugins belong to the supplied Runtime"
    );
    assert!(one.shutdown().await.is_clean());
    assert!(one.session_service().is_err());
    assert_eq!(service_instances(), 1);
    assert!(
        two.session_service()
            .unwrap()
            .list_recent(None, 16)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    two.reload().await.unwrap();
    assert_eq!(
        two.language_models()
            .unwrap()
            .list_models(None, 16)
            .await
            .unwrap()
            .models
            .len(),
        1
    );
    assert!(runtime.shutdown().await.is_clean());
    assert!(two.session_service().is_err());
    assert!(two.shutdown().await.is_clean());
    rsi_service_host::HostOwnerLease::try_acquire(
        rsi_service_host::ServiceHostPaths::from_host_paths(&second.paths).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn rejected_child_profile_leaves_the_parent_graph_and_backend_untouched() {
    let runtime = rsi_meta::Runtime::default();
    let fixture = fixture("http://127.0.0.1:1");
    std::fs::write(
        &fixture.profile,
        "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'bad'\nplugin = 'unregistered.plugin'\n",
    )
    .unwrap();
    let before = runtime.snapshot().fibers.len();
    assert!(
        rsi::RunningRsi::boot_host_profile_in(
            composition(fixture.paths.clone()),
            &host_profile(&fixture),
            &runtime.root(),
        )
        .await
        .is_err()
    );
    assert_eq!(runtime.snapshot().fibers.len(), before);
    let paths = rsi_service_host::ServiceHostPaths::from_host_paths(&fixture.paths).unwrap();
    assert!(!paths.owner_lock().exists());
    assert!(!fixture.paths.state().join("base.sqlite3").exists());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn owner_precedes_storage_and_runtime_identity_does_not_change_launch_preview() {
    let fixture = fixture("http://127.0.0.1:1");
    let candidate = composition(fixture.paths.clone());
    let profile = host_profile(&fixture);
    let preview = candidate.preview_host(&profile).unwrap();
    let paths = rsi_service_host::ServiceHostPaths::from_host_paths(&fixture.paths).unwrap();
    assert!(!paths.owner_lock().exists());
    let lease = Arc::new(rsi_service_host::HostOwnerLease::try_acquire(paths.clone()).unwrap());
    let blocked = candidate
        .clone()
        .build()
        .unwrap()
        .start_file(&fixture.profile)
        .await;
    assert!(blocked.is_err());
    assert!(!fixture.paths.state().join("base.sqlite3").exists());
    assert!(!fixture.paths.state().join("agent").exists());
    let candidate = candidate
        .with_service_owner(
            lease.clone(),
            rsi_api_protocol::HostEpoch::generate().unwrap(),
        )
        .unwrap();
    assert_eq!(candidate.preview_host(&profile).unwrap(), preview);
    let host = candidate
        .build()
        .unwrap()
        .start_file(&fixture.profile)
        .await
        .unwrap();
    assert!(
        host.lookup_local::<ConnectionDescriptionContract>()
            .is_some()
    );
    assert!(host.shutdown().await.is_clean());
    drop(host);
    drop(lease);
    rsi_service_host::HostOwnerLease::try_acquire(paths).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // Observe complete product restart, authentication and API ownership together.
async fn standard_api_plugins_share_durable_identity_and_serve_independent_domains() {
    let fixture = fixture("http://127.0.0.1:1");
    let candidate = composition(fixture.paths.clone());
    let host = candidate
        .clone()
        .build()
        .unwrap()
        .start_file(&fixture.profile)
        .await
        .unwrap();
    let description = host
        .lookup_local::<ConnectionDescriptionContract>()
        .unwrap();
    let registered = host
        .lookup_local::<DeviceAdministrationContract>()
        .unwrap()
        .register("standard API fixture")
        .await
        .unwrap();
    let retained_device = host
        .lookup_local::<DeviceAdministrationContract>()
        .unwrap()
        .register("retained identity fixture")
        .await
        .unwrap();
    let authentication = host.lookup_local::<DeviceAuthenticationContract>().unwrap();
    let authority = authentication.authenticate(&registered.token).unwrap();
    let dispatch = host.lookup_local::<ApiDispatchContract>().unwrap();
    let domains = dispatch
        .operations()
        .into_iter()
        .map(|spec| spec.id.domain().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        domains,
        [
            "connection",
            "configuration",
            "devices",
            "files",
            "inspector",
            "media",
            "models",
            "navigation",
            #[cfg(unix)]
            "native-addons",
            "output",
            "provider-credentials",
            "providers",
            "session",
            "settings",
            "ui",
            "workspace"
        ]
        .map(str::to_owned)
        .into()
    );
    assert!(
        dispatch
            .operations()
            .iter()
            .filter(|spec| matches!(spec.id.domain(), "devices" | "inspector" | "native-addons"))
            .all(|spec| spec.access == rsi_api_protocol::OperationAccess::Local)
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind = listener.local_addr().unwrap();
    let origin = format!("http://{bind}");
    let execution = rsi_meta::Execution::native(tokio::runtime::Handle::current());
    let server = rsi_api_http::HttpServer::from_listener(
        execution.clone(),
        listener,
        rsi_api_http::HttpConfig {
            bind,
            public_origin: origin.clone(),
            tls: None,
            allow_loopback_http: true,
        },
        rsi_api_http::HttpServices {
            dispatch: dispatch.clone(),
            authentication,
            endpoint: description.endpoint_id.clone(),
            epoch: description.host_epoch.clone(),
        },
    )
    .await
    .unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(server.serve(stop.clone()));
    let api = Arc::new(
        rsi_api_http_client::HttpClient::connect(
            execution,
            rsi_api_http_client::HttpClientConfig {
                origin,
                endpoint_id: description.endpoint_id.clone(),
                credential: CredentialRef::new("fixture", "device").unwrap(),
                tls_ca: None,
                allow_loopback_http: true,
            },
            registered.token.clone(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(api.description(), description.as_ref());
    let workspace = rsi_workspace_api::WorkspaceClient::new(api.clone()).unwrap();
    let registered_workspace = workspace.get_or_create(&fixture.workspace).await.unwrap();
    assert_eq!(
        workspace.get(&registered_workspace.id).await.unwrap(),
        registered_workspace
    );
    let settings = rsi_settings_api::SettingsClient::new(api.clone()).unwrap();
    let snapshot = settings.read("rsi.agent").await.unwrap();
    assert_eq!(snapshot.value["default_model"]["model"], "fixture-model");
    let models = rsi_ai_models_api::ModelsClient::new(api.clone()).unwrap();
    assert_eq!(models.list_models(None, 256).await.unwrap().models.len(), 1);
    rsi_media_api::MediaClient::new(api.clone()).unwrap();
    rsi_process_output_api::OutputClient::new(api.clone()).unwrap();
    let sessions = rsi_session_api::SessionClient::new(api.clone()).unwrap();
    assert!(
        sessions
            .list_recent(None, 8)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    std::fs::write(fixture.workspace.join("sample"), b"a\0\xffz").unwrap();
    let files = rsi_session_files::SessionFilesClient::new(api.clone()).unwrap();
    let (target, file) =
        super::files::browse(&sessions, &files, registered_workspace.id, "http").await;
    dropped_ui_binding_releases_profile(&host, &target.session_id).await;
    let mut ui_stream = remote_ui(api.clone(), &target.session_id).await;
    host.lookup_local::<DeviceAdministrationContract>()
        .unwrap()
        .revoke(&registered.record.id)
        .await
        .unwrap();
    assert_eq!(
        rsi_session_files::SessionFiles::read(&files, target, file, 0, 4)
            .await
            .unwrap_err(),
        rsi_session_files::SessionFilesError::Api(rsi_api_protocol::ApiError::Unauthorized)
    );
    let end = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut delivered = 0;
        loop {
            match ui_stream.next().await {
                Ok(Some(_)) => {
                    delivered += 1;
                    assert!(
                        delivered <= 16,
                        "revocation must stop bounded queued deliveries"
                    );
                }
                end => break end,
            }
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(end, Err(rsi_api_protocol::ApiError::Unauthorized)),
        "unexpected UI stream end: {end:?}"
    );
    drop(ui_stream);
    api.close().await;
    stop.cancel();
    task.await.unwrap().unwrap();
    assert!(host.shutdown().await.is_clean());
    assert!(authority.revoked.is_cancelled());
    assert!(dispatch.operations().is_empty());
    let restarted = candidate
        .build()
        .unwrap()
        .start_file(&fixture.profile)
        .await
        .unwrap();
    let next = restarted
        .lookup_local::<ConnectionDescriptionContract>()
        .unwrap();
    assert_eq!(next.endpoint_id, description.endpoint_id);
    assert_ne!(next.host_epoch, description.host_epoch);
    assert!(
        restarted
            .lookup_local::<DeviceAuthenticationContract>()
            .unwrap()
            .authenticate(&registered.token)
            .is_err()
    );
    assert_eq!(
        restarted
            .lookup_local::<DeviceAuthenticationContract>()
            .unwrap()
            .authenticate(&retained_device.token)
            .unwrap()
            .id,
        retained_device.record.id
    );
    assert!(restarted.shutdown().await.is_clean());
}

async fn dropped_ui_binding_releases_profile(
    host: &rsi_host::RunningHost,
    session: &rsi_agent_session_protocol::SessionId,
) {
    let baseline = host
        .inspect(rsi_meta::InspectionRequest::default())
        .unwrap()
        .total_fibers;
    let binder = host
        .lookup_local::<rsi_ui_api::UiTargetBinderContract>()
        .unwrap();
    for _ in 0..24 {
        let binding = binder
            .bind(
                rsi_api_protocol::CallOrigin::Local,
                rsi_ui_api::ExportScope {
                    kind: "session".into(),
                    key: session.to_string(),
                },
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            host.inspect(rsi_meta::InspectionRequest::default())
                .unwrap()
                .total_fibers
                > baseline
        );
        // Also covers a delivered oneshot value dropped before its waiter accepts it.
        drop(binding);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while host
                .inspect(rsi_meta::InspectionRequest::default())
                .unwrap()
                .total_fibers
                != baseline
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropped UI binding retained an orphan Profile");
    }
}

async fn remote_ui(
    api: Arc<dyn ApiClient>,
    session: &rsi_agent_session_protocol::SessionId,
) -> rsi_ui_api::UiObservation {
    use rsi_ui_api::{CatalogRequest, ExportScope, Observe, Selection, UiClient};
    let client = UiClient::new(api).unwrap();
    let scope = ExportScope {
        kind: "session".into(),
        key: session.to_string(),
    };
    let page = client
        .catalog(&CatalogRequest {
            scope: scope.clone(),
            after: None,
            maximum: 64,
        })
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].bundle, "rsi.session.inspection");
    assert!(page.next.is_none());
    let request = Observe {
        application: "http-ui-fixture".into(),
        selections: vec![Selection {
            scope,
            bundle: page.entries[0].bundle.clone(),
            surface: page.entries[0].surface.clone(),
        }],
    };
    let mut stream = client.observe(&request).await.unwrap();
    let item = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .item;
    let view = item.snapshot.model.standard_view.unwrap();
    assert_eq!(view.title, "Session details");
    assert!(view.elements.iter().any(|element| matches!(element, rsi_ui::UiElement::Field { label, value } if label == "Session" && value == session.as_str())));
    assert!(item.ticket.is_some());
    stream
}
