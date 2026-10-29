use rsi::{ApplicationProfileId, ProfileCatalog, standard_application_host};
use rsi_host::HostPaths;
use std::collections::BTreeMap;
#[cfg(any(target_os = "linux", windows))]
mod support;

#[cfg(windows)]
#[tokio::test]
async fn windows_application_bounds_discovery_of_an_owner_without_an_endpoint() {
    let temporary = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(
        temporary.path().join("config"),
        temporary.path().join("state"),
        temporary.path().join("cache"),
    )
    .unwrap();
    let owner_paths = rsi_service_host::ServiceHostPaths::from_host_paths(&paths).unwrap();
    let lease = rsi_service_host::HostOwnerLease::try_acquire(owner_paths.clone()).unwrap();
    for plugin in [
        "rsi.application.connection",
        "rsi.application.service",
        "rsi.application.acp-service",
    ] {
        let (host, diagnostics) = standard_application_host(
            rsi_app_catalog::compose(rsi::StandardComposition::new(
                paths.clone(),
                BTreeMap::new(),
                None,
                rsi_app_catalog::metadata(),
            ))
            .unwrap(),
            vec![],
        )
        .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            host.start(rsi_host::Profile::new(vec![rsi_host::ProfileEntry::new(
                "connection",
                plugin,
                serde_json::json!({"host_profile":"standard"}),
            )])),
        )
        .await
        .expect("owner discovery must finish within its bounded startup deadline");
        assert!(result.is_err());
        let diagnostic = format!("{:?}", diagnostics.take());
        assert!(
            diagnostic.contains("owner is active but did not publish a usable endpoint"),
            "{plugin}: result={result:?}; diagnostic={diagnostic}"
        );
        assert!(owner_paths.read_metadata().unwrap().is_none());
        assert!(rsi_service_host::HostOwnerLease::try_acquire(owner_paths.clone()).is_err());
    }
    drop(lease);
    drop(rsi_service_host::HostOwnerLease::try_acquire(owner_paths).unwrap());
}

#[cfg(any(target_os = "linux", windows))]
#[tokio::test]
async fn embedded_application_connection_starts_without_optional_process_capabilities() {
    let temporary = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(
        temporary.path().join("config"),
        temporary.path().join("state"),
        temporary.path().join("cache"),
    )
    .unwrap();
    let (host, diagnostics) = standard_application_host(
        rsi_app_catalog::compose(rsi::StandardComposition::new(
            paths.clone(),
            BTreeMap::new(),
            None,
            rsi_app_catalog::metadata(),
        ))
        .unwrap(),
        vec![],
    )
    .unwrap();
    let running = host
        .start(rsi_host::Profile::new(vec![rsi_host::ProfileEntry::new(
            "connection",
            "rsi.application.connection",
            serde_json::json!({"host_profile":"standard"}),
        )]))
        .await
        .unwrap_or_else(|error| panic!("{error}; {:?}", diagnostics.take()));
    let sessions = running
        .lookup_local::<rsi_session_protocol::SessionContract>()
        .unwrap();
    assert!(
        sessions
            .list_recent(None, 8)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    drop(sessions);
    assert!(running.shutdown().await.is_clean());
    let owner_paths = rsi_service_host::ServiceHostPaths::from_host_paths(&paths).unwrap();
    let replacement = rsi_service_host::HostOwnerLease::try_acquire(owner_paths).unwrap();
    drop(replacement);
}

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
        rsi_app_catalog::compose(rsi::StandardComposition::new(
            paths.clone(),
            BTreeMap::new(),
            None,
            rsi_app_catalog::metadata(),
        ))
        .unwrap(),
        vec![],
    )
    .unwrap();
    let program = rsi_host::ProfileProgram::from_profile(rsi_host::Profile::new(vec![
        rsi_host::ProfileEntry::new(
            "credentials",
            "rsi.credentials.local",
            serde_json::json!({}),
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
            rsi_app_catalog::compose(rsi::StandardComposition::new(
                paths.clone(),
                BTreeMap::new(),
                None,
                rsi_app_catalog::metadata(),
            ))
            .unwrap(),
            arguments.into_iter().map(Into::into).collect(),
        )
        .unwrap();
        #[cfg(any(target_os = "linux", windows))]
        let program = ProfileCatalog::new(paths.clone(), rsi_app_catalog::metadata())
            .application(&ApplicationProfileId::new("devices").unwrap())
            .unwrap()
            .program()
            .unwrap();
        // Other platforms lack the standard operator transport. Exercise the same
        // portable application factory directly where that backend is absent.
        #[cfg(not(any(target_os = "linux", windows)))]
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

#[cfg(any(target_os = "linux", windows))]
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
        let profile = ProfileCatalog::new(paths.clone(), rsi_app_catalog::metadata())
            .application(&ApplicationProfileId::new("serve").unwrap())
            .unwrap();
        let (host, diagnostics) = standard_application_host(
            rsi_app_catalog::compose(rsi::StandardComposition::new(
                paths.clone(),
                BTreeMap::new(),
                None,
                rsi_app_catalog::metadata(),
            ))
            .unwrap(),
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

#[cfg(any(target_os = "linux", windows))]
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
    let (host, program) = support::embedded_serve(
        rsi::StandardComposition::new(
            paths.clone(),
            BTreeMap::new(),
            None,
            rsi_app_catalog::metadata(),
        ),
        [
            "--bind",
            "127.0.0.1:0",
            "--origin",
            "http://127.0.0.1",
            "--dev-http",
        ]
        .map(Into::into)
        .to_vec(),
    );
    let running = host.start_program(program).await.unwrap();
    let count = |id: &str| {
        running.runtime_snapshot().fibers.into_iter().filter(|fiber| fiber.state == rsi_meta::FiberState::Active && matches!(&fiber.factory, rsi_meta::FactoryIdentity::Linked { plugin, .. } if plugin.as_str() == id)).count()
    };
    assert_eq!(count("rsi.session"), 1);
    assert_eq!(count("rsi.api"), 1);
    assert_eq!(count("rsi.serve.http"), 1);
    let conflict = rsi_service_host::HostOwnerLease::try_acquire(
        rsi_service_host::ServiceHostPaths::from_host_paths(&paths).unwrap(),
    )
    .unwrap_err();
    assert_eq!(conflict, rsi_service_host::ServiceHostError::OwnerActive);
    #[cfg(windows)]
    assert!(
        conflict
            .to_string()
            .contains("stop the other Web, TUI, ACP")
    );
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

#[cfg(any(target_os = "linux", windows))]
async fn http_description(address: std::net::SocketAddr, token: Option<&str>) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let body = r#"{"wire_version":2,"expected_endpoint":null}"#;
        let auth = token.map_or_else(String::new, |token| format!("Authorization: Bearer {token}\r\n"));
        let request = format!("POST /api/v2/rsi-api-protocol/4/connection/describe/1/read HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Type: application/json\r\nX-Rsi-Wire-Version: 2\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
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
    let profile = ProfileCatalog::new(paths.clone(), rsi_app_catalog::metadata())
        .application(&ApplicationProfileId::new("headless").unwrap())
        .unwrap();
    let (host, diagnostics) = standard_application_host(
        rsi_app_catalog::compose(rsi::StandardComposition::new(
            paths.clone(),
            BTreeMap::new(),
            None,
            rsi_app_catalog::metadata(),
        ))
        .unwrap(),
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
    let catalog = ProfileCatalog::new(paths.clone(), rsi_app_catalog::metadata());
    let id = ApplicationProfileId::new("invalid-program").unwrap();
    let path = catalog.application_path(&id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    for source in ["format = 2\n", "format = 1\nunknown = true\n"] {
        std::fs::write(&path, source).unwrap();
        let program = catalog.application(&id).unwrap().program().unwrap();
        let (host, diagnostics) = standard_application_host(
            rsi_app_catalog::compose(rsi::StandardComposition::new(
                paths.clone(),
                BTreeMap::new(),
                None,
                rsi_app_catalog::metadata(),
            ))
            .unwrap(),
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
#[cfg(any(target_os = "linux", windows))]
#[tokio::test]
async fn retained_service_observation_does_not_keep_native_ownership_after_shutdown() {
    let temporary = tempfile::tempdir().unwrap();
    let paths = rsi_host::HostPaths::new(
        temporary.path().join("config"),
        temporary.path().join("state"),
        temporary.path().join("cache"),
    )
    .unwrap();
    let (host, _) = rsi::standard_application_host(
        rsi_app_catalog::compose(rsi::StandardComposition::new(
            paths.clone(),
            std::collections::BTreeMap::new(),
            None,
            rsi_app_catalog::metadata(),
        ))
        .unwrap(),
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
        .lookup_local::<rsi::application_services::ServingServiceContract>()
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
    assert!(matches!(
        dispatch.operations(),
        Err(rsi_api_protocol::ApiError::Unavailable)
    ));
    assert!(matches!(
        dispatch.admit(
            &rsi_api_protocol::describe_operation().call(),
            rsi_api_protocol::CallOrigin::Local
        ),
        Err(rsi_api_protocol::ApiError::Unavailable)
    ));
    drop(replacement);
}

#[tokio::test]
async fn catalog_admits_independent_web_bundle_and_rejects_bootstrap_tampering() {
    for corrupt in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let assets = temporary.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        for file in rsi_web_assets::BOOTSTRAP_FILES {
            std::fs::write(assets.join(file), file.as_bytes()).unwrap();
        }
        std::fs::write(
            assets.join("rsi-build.json"),
            rsi_web_assets::pairing_receipt(
                &assets,
                &"b".repeat(64),
                rsi_api_protocol::ProtocolDescriptor {
                    name: "rsi.desktop.bridge".into(),
                    version: 5,
                },
            )
            .unwrap(),
        )
        .unwrap();
        if corrupt {
            std::fs::write(assets.join("worker.js"), b"changed after receipt").unwrap();
        }
        let paths = HostPaths::new(
            temporary.path().join("config"),
            temporary.path().join("state"),
            temporary.path().join("cache"),
        )
        .unwrap();
        let (host, _) = standard_application_host(
            rsi_app_catalog::compose(rsi::StandardComposition::new(
                paths,
                BTreeMap::new(),
                None,
                rsi_app_catalog::metadata(),
            ))
            .unwrap(),
            vec![],
        )
        .unwrap();
        let result = host
            .start(rsi_host::Profile::new(vec![rsi_host::ProfileEntry::new(
                "assets",
                "rsi.web.assets",
                serde_json::json!({"directory": assets, "files": rsi_web_assets::BOOTSTRAP_FILES}),
            )]))
            .await;
        if corrupt {
            assert!(
                result.is_err(),
                "catalog must reject changed bootstrap bytes"
            );
        } else {
            let running = result.expect("catalog must accept an independent bundle family");
            assert!(
                running
                    .lookup_local::<rsi_api_http::HttpAssetsContract>()
                    .is_some()
            );
            assert!(running.shutdown().await.is_clean());
        }
    }
}
