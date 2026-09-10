use rsi::{ApplicationProfileId, ProfileCatalog, standard_application_host};
use rsi_host::HostPaths;
use std::collections::BTreeMap;

#[tokio::test]
async fn invalid_remote_policy_and_device_arguments_prepare_before_any_backend() {
    let temporary = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(
        temporary.path().join("config"),
        temporary.path().join("state"),
        temporary.path().join("cache"),
    )
    .unwrap();
    let (host, diagnostics) = standard_application_host(
        rsi::StandardComposition::new(paths.clone(), BTreeMap::new(), None),
        vec![],
    )
    .unwrap();
    let program = rsi_host::ProfileProgram::from_profile(rsi_host::Profile::new(vec![
        rsi_host::ProfileEntry::new(
            "credentials",
            "rsi.credentials.local",
            serde_json::json!({"service":"unused"}),
        ),
        rsi_host::ProfileEntry::new(
            "connection",
            "rsi.application.http",
            serde_json::json!({
                "origin":"http://0.0.0.0:8787", "endpoint_id":"00000000000000000000000000000000",
                "credential":{"owner":"remote", "slot":"device"}, "allow_loopback_http":true,
            }),
        ),
    ]));
    assert!(host.start_program(program).await.is_err());
    assert!(diagnostics.take().is_some());
    for arguments in [
        vec![],
        vec!["register", ""],
        vec!["revoke", "invalid"],
        vec!["list", "unexpected"],
    ] {
        let (host, diagnostics) = standard_application_host(
            rsi::StandardComposition::new(paths.clone(), BTreeMap::new(), None),
            arguments.into_iter().map(Into::into).collect(),
        )
        .unwrap();
        #[cfg(target_os = "linux")]
        let program = ProfileCatalog::new(paths.clone())
            .application(&ApplicationProfileId::new("devices").unwrap())
            .unwrap()
            .program()
            .unwrap();
        // The standard operator transport is Linux-only. Exercise the same
        // portable application factory directly where that backend is absent.
        #[cfg(not(target_os = "linux"))]
        let program = rsi_host::ProfileProgram::from_profile(rsi_host::Profile::new(vec![
            rsi_host::ProfileEntry::new(
                "application",
                "rsi.application.devices",
                serde_json::Value::Null,
            ),
        ]));
        assert!(host.start_program(program).await.is_err());
        assert!(diagnostics.take().is_some());
    }
    for path in [paths.config(), paths.state(), paths.cache()] {
        assert!(!path.exists(), "preflight created {}", path.display());
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn invalid_serve_transport_policy_is_rejected_before_service_activation() {
    for args in [
        vec![],
        vec![
            "--bind",
            "0.0.0.0:8787",
            "--origin",
            "http://localhost:8787",
            "--dev-http",
        ],
        vec![
            "--bind",
            "127.0.0.1:8787",
            "--origin",
            "http://127.0.0.1:8787",
        ],
        vec![
            "--bind",
            "127.0.0.1:8787",
            "--origin",
            "https://localhost:8787",
            "--tls-key",
            "unused.pem",
        ],
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let paths = HostPaths::new(
            temporary.path().join("config"),
            temporary.path().join("state"),
            temporary.path().join("cache"),
        )
        .unwrap();
        let profile = ProfileCatalog::new(paths.clone())
            .application(&ApplicationProfileId::new("serve").unwrap())
            .unwrap();
        let (host, diagnostics) = standard_application_host(
            rsi::StandardComposition::new(paths.clone(), BTreeMap::new(), None),
            args.into_iter().map(Into::into).collect(),
        )
        .unwrap();
        assert!(
            host.start_program(profile.program().unwrap())
                .await
                .is_err()
        );
        assert!(diagnostics.take().is_some());
        for path in [paths.config(), paths.state(), paths.cache()] {
            assert!(
                !path.exists(),
                "invalid Serve input activated a backend: {}",
                path.display()
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serve_profile_owns_one_runtime_authenticates_http_and_releases_its_listener_and_owner() {
    use rsi_api_protocol::{
        ConnectionDescription, ConnectionDescriptionContract, DeviceAdministrationContract,
    };
    let temporary = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(
        temporary.path().join("config"),
        temporary.path().join("state"),
        temporary.path().join("cache"),
    )
    .unwrap();
    std::fs::create_dir_all(paths.config()).unwrap();
    std::fs::write(
        paths.config().join("settings.json"),
        br#"{"rsi.agent":{"default_model":{"deployment":"fixture","model":"unused"}}}"#,
    )
    .unwrap();
    let profile = ProfileCatalog::new(paths.clone())
        .application(&ApplicationProfileId::new("serve").unwrap())
        .unwrap();
    let (host, diagnostics) = standard_application_host(
        rsi::StandardComposition::new(paths.clone(), BTreeMap::new(), None),
        [
            "--bind",
            "127.0.0.1:0",
            "--origin",
            "http://127.0.0.1",
            "--dev-http",
        ]
        .map(Into::into)
        .to_vec(),
    )
    .unwrap();
    let running = host
        .start_program(profile.program().unwrap())
        .await
        .unwrap_or_else(|error| panic!("{error}; {:?}", diagnostics.take()));
    let count = |id: &str| {
        running.runtime_snapshot().fibers.into_iter().filter(|fiber| fiber.state == rsi_meta::FiberState::Active && matches!(&fiber.factory, rsi_meta::FactoryIdentity::Linked { plugin, .. } if plugin.as_str() == id)).count()
    };
    assert_eq!(count("rsi.session"), 1);
    assert_eq!(count("rsi.api"), 1);
    assert_eq!(count("rsi.serve.http"), 1);
    let description = running
        .lookup_local::<ConnectionDescriptionContract>()
        .unwrap();
    let listener = running
        .lookup_local::<rsi_api_http::HttpListenerContract>()
        .unwrap();
    let address = listener.address();
    let response = http_description(address, None).await;
    assert!(response.starts_with("HTTP/1.1 401"));
    let administration = running
        .lookup_local::<DeviceAdministrationContract>()
        .unwrap();
    let device = administration
        .register("Serve composition fixture")
        .await
        .unwrap();
    let response = http_description(address, Some(device.token.expose_secret())).await;
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "{}",
        response.lines().next().unwrap()
    );
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    assert_eq!(
        serde_json::from_str::<ConnectionDescription>(body).unwrap(),
        *description
    );
    let application = running
        .lookup_local::<rsi_application::ApplicationRunContract>()
        .unwrap();
    drop(application.clone().run());
    assert!(matches!(
        application.clone().run().await,
        Err(rsi_application::ApplicationError::AlreadyStarted)
    ));
    let cleanup = running.shutdown().await;
    assert!(cleanup.is_clean(), "{cleanup:?}");
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
    assert!(matches!(
        application.run().await,
        Err(rsi_application::ApplicationError::ShuttingDown)
    ));
    rsi_service_host::HostOwnerLease::try_acquire(
        rsi_service_host::ServiceHostPaths::from_host_paths(&paths).unwrap(),
    )
    .unwrap();
}

#[cfg(target_os = "linux")]
async fn http_description(address: std::net::SocketAddr, token: Option<&str>) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let body = r#"{"wire_version":1,"expected_endpoint":null}"#;
        let auth = token.map_or_else(String::new, |token| format!("Authorization: Bearer {token}\r\n"));
        let request = format!("POST /api/v1/connection/describe/1 HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Type: application/json\r\nX-Rsi-Wire-Version: 1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.take(64 * 1024).read_to_end(&mut response).await.unwrap();
        String::from_utf8(response).unwrap()
    }).await.unwrap()
}

#[tokio::test]
async fn invalid_application_arguments_fail_before_any_standard_backend_or_asset_materialization() {
    let temporary = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(
        temporary.path().join("config"),
        temporary.path().join("state"),
        temporary.path().join("cache"),
    )
    .unwrap();
    let profile = ProfileCatalog::new(paths.clone())
        .application(&ApplicationProfileId::new("headless").unwrap())
        .unwrap();
    let (host, diagnostics) = standard_application_host(
        rsi::StandardComposition::new(paths.clone(), BTreeMap::new(), None),
        vec!["--unknown".into()],
    )
    .unwrap();
    assert!(
        host.start_program(profile.program().unwrap())
            .await
            .is_err()
    );
    assert!(
        diagnostics
            .take()
            .unwrap()
            .to_string()
            .contains("unknown option")
    );
    for path in [paths.config(), paths.state(), paths.cache()] {
        assert!(
            !path.exists(),
            "application preflight created {}",
            path.display()
        );
    }
}

#[tokio::test]
async fn ordinary_application_source_errors_are_rejected_by_the_shared_compiler_before_activation()
{
    let temporary = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(
        temporary.path().join("config"),
        temporary.path().join("state"),
        temporary.path().join("cache"),
    )
    .unwrap();
    let catalog = ProfileCatalog::new(paths.clone());
    let id = ApplicationProfileId::new("invalid-program").unwrap();
    let path = catalog.application_path(&id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    for source in ["format = 2\n", "format = 1\nunknown = true\n"] {
        std::fs::write(&path, source).unwrap();
        let program = catalog.application(&id).unwrap().program().unwrap();
        let (host, diagnostics) = standard_application_host(
            rsi::StandardComposition::new(paths.clone(), BTreeMap::new(), None),
            Vec::new(),
        )
        .unwrap();
        assert!(host.start_program(program).await.is_err());
        assert!(
            diagnostics.take().is_none(),
            "source failure belongs to the shared Profile compiler"
        );
        assert!(!paths.state().exists());
        assert!(!paths.cache().exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn retained_service_observation_does_not_keep_native_ownership_after_shutdown() {
    let temporary = tempfile::tempdir().unwrap();
    let paths = rsi_host::HostPaths::new(
        temporary.path().join("config"),
        temporary.path().join("state"),
        temporary.path().join("cache"),
    )
    .unwrap();
    std::fs::create_dir_all(paths.config()).unwrap();
    std::fs::write(
        paths.config().join("settings.json"),
        br#"{"rsi.agent":{"default_model":{"deployment":"fixture","model":"unused"}}}"#,
    )
    .unwrap();
    let (host, _) = rsi::standard_application_host(
        rsi::StandardComposition::new(paths.clone(), std::collections::BTreeMap::new(), None),
        vec![],
    )
    .unwrap();
    let running = host
        .start(rsi_host::Profile::new(vec![rsi_host::ProfileEntry::new(
            "service",
            "rsi.application.service",
            serde_json::json!({"host_profile":"standard"}),
        )]))
        .await
        .unwrap();
    let observation = running
        .lookup_local::<rsi_serve::ServingServiceContract>()
        .unwrap();
    let administration = running
        .lookup_local::<rsi_api_protocol::DeviceAdministrationContract>()
        .unwrap();
    let authentication = running
        .lookup_local::<rsi_api_protocol::DeviceAuthenticationContract>()
        .unwrap();
    let dispatch = running
        .lookup_local::<rsi_api_protocol::ApiDispatchContract>()
        .unwrap();
    let registered = administration.register("retained operator").await.unwrap();
    let authenticated = authentication.authenticate(&registered.token).unwrap();
    assert!(running.shutdown().await.is_clean());
    observation.stopped().await.unwrap();
    let owner_paths = rsi_service_host::ServiceHostPaths::from_host_paths(&paths).unwrap();
    let replacement = rsi_service_host::HostOwnerLease::try_acquire(owner_paths).unwrap();
    assert!(observation.reload().await.is_err());
    assert!(authenticated.revoked.is_cancelled());
    assert!(matches!(
        administration.list(),
        Err(rsi_api_protocol::ApiError::Unavailable)
    ));
    assert!(matches!(
        administration.revoke(&registered.record.id).await,
        Err(rsi_api_protocol::ApiError::Unavailable)
    ));
    assert!(matches!(
        administration.register("after shutdown").await,
        Err(rsi_api_protocol::ApiError::Unavailable)
    ));
    assert!(matches!(
        authentication.authenticate(&registered.token),
        Err(rsi_api_protocol::ApiError::Unavailable)
    ));
    assert!(dispatch.operations().is_empty());
    assert!(matches!(
        dispatch.admit(
            &rsi_api_protocol::describe_operation().id,
            rsi_api_protocol::CallOrigin::Local
        ),
        Err(rsi_api_protocol::ApiError::Unavailable)
    ));
    drop(replacement);
}
