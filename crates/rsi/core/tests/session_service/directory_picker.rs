use super::*;
use rsi_api_protocol::{ApiError, ApiOutput, ByteBudget, CallOrigin, OperationSpec};
use rsi_directory_picker_api::Operation;
use serde_json::{Value, json};
async fn wire(
    running: &RunningRsi,
    origin: CallOrigin,
    spec: OperationSpec,
    input: Value,
) -> Result<Value, ApiError> {
    let input = if spec.id.domain() == "directory-picker" {
        json!({"location":{"kind":"local"},"request":input})
    } else {
        input
    };
    wire_at(running, origin, spec, input).await
}
async fn wire_at(
    running: &RunningRsi,
    origin: CallOrigin,
    spec: OperationSpec,
    input: Value,
) -> Result<Value, ApiError> {
    let payload = ByteBudget::default().encode(&input, spec.maximum_request_bytes)?;
    let ApiOutput::Reply(reply) = running
        .api_dispatch()
        .unwrap()
        .admit(&spec.id, origin)?
        .invoke(payload)
        .await?
    else {
        panic!("finite directory API")
    };
    Ok(serde_json::from_slice(reply.json.as_bytes()).unwrap())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn target_picker_checks_use_before_connection_lookup_and_local_grants_do_not_substitute() {
    let fixture = fixture("http://127.0.0.1:1");
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let device = running
        .device_administration()
        .unwrap()
        .register("target-picker")
        .await
        .unwrap();
    let origin = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&device.token)
            .unwrap(),
    );
    let location = json!({"kind":"ssh","target":"b".repeat(32)});
    let status = |request| json!({"location":location,"request":request});
    // The same path exists locally. No connection or missing Use may select it.
    for configuration in [false, true] {
        if configuration {
            wire_at(
                &running,
                CallOrigin::Local,
                rsi_configuration_api::ConfigurationOperation::SetGrant.spec(),
                json!({"device":device.record.id,"expected_revision":"0","granted":true}),
            )
            .await
            .unwrap();
        }
        let observed = wire_at(
            &running,
            origin.clone(),
            Operation::Status.spec(),
            status(Value::Null),
        )
        .await
        .unwrap();
        assert_eq!(observed, json!({"supported":true,"allowed":false}));
        for (operation, request) in [
            (Operation::List, json!({"path":fixture.workspace})),
            (
                Operation::Create,
                json!({"parent":fixture.workspace,"name":"remote-only"}),
            ),
        ] {
            assert!(matches!(
                wire_at(&running, origin.clone(), operation.spec(), status(request)).await,
                Err(ApiError::Unauthorized)
            ));
        }
    }
    let scope = json!({"principal":{"kind":"device","id":device.record.id},"scope":{"kind":"ssh_use","target":"b".repeat(32)}});
    for (revision, granted) in [("0", true), ("1", false)] {
        wire_at(
            &running,
            CallOrigin::Local,
            rsi_configuration_api::leaf::Operation::SetGrant.spec(),
            json!({"expected":revision,"scope":scope,"granted":granted}),
        )
        .await
        .unwrap();
        let observed = wire_at(
            &running,
            origin.clone(),
            Operation::Status.spec(),
            status(Value::Null),
        )
        .await
        .unwrap();
        assert_eq!(observed["allowed"], granted);
        let listed = wire_at(
            &running,
            origin.clone(),
            Operation::List.spec(),
            status(json!({"path":fixture.workspace})),
        )
        .await;
        assert!(if granted {
            matches!(listed, Err(ApiError::Unavailable))
        } else {
            matches!(listed, Err(ApiError::Unauthorized))
        });
    }
    assert!(!fixture.workspace.join("remote-only").exists());
    assert!(running.shutdown().await.is_clean());
}
#[tokio::test]
async fn directory_discovery_and_single_creation_require_actual_configuration_grants() {
    let fixture = fixture("http://127.0.0.1:1");
    let composition = composition(fixture.paths.clone())
        .with_user_home(Some(fixture.workspace.clone()))
        .unwrap();
    let running = RunningRsi::boot_host_profile(composition, &host_profile(&fixture))
        .await
        .unwrap();
    let device = running
        .device_administration()
        .unwrap()
        .register("directory-picker")
        .await
        .unwrap();
    let origin = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&device.token)
            .unwrap(),
    );
    let status = wire(
        &running,
        origin.clone(),
        Operation::Status.spec(),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(status, json!({"supported":cfg!(unix),"allowed":false}));
    assert!(matches!(
        wire(
            &running,
            origin.clone(),
            Operation::List.spec(),
            json!({"path":null})
        )
        .await,
        Err(ApiError::Unauthorized)
    ));
    assert!(matches!(
        wire(
            &running,
            origin.clone(),
            Operation::Create.spec(),
            json!({"parent":fixture.workspace,"name":"ungranted"})
        )
        .await,
        Err(ApiError::Unauthorized)
    ));
    assert!(!fixture.workspace.join("ungranted").exists());
    wire(
        &running,
        CallOrigin::Local,
        rsi_configuration_api::ConfigurationOperation::SetGrant.spec(),
        json!({"device":device.record.id,"expected_revision":"0","granted":true}),
    )
    .await
    .unwrap();
    let listing = wire(
        &running,
        origin.clone(),
        Operation::List.spec(),
        json!({"path":null}),
    )
    .await
    .unwrap();
    assert_eq!(listing["path"], fixture.workspace.to_str().unwrap());
    let created = wire(
        &running,
        origin.clone(),
        Operation::Create.spec(),
        json!({"parent":fixture.workspace,"name":"picked"}),
    )
    .await
    .unwrap();
    assert_eq!(
        created["path"],
        fixture.workspace.join("picked").to_str().unwrap()
    );
    assert!(fixture.workspace.join("picked").is_dir());
    wire(
        &running,
        CallOrigin::Local,
        rsi_configuration_api::ConfigurationOperation::SetGrant.spec(),
        json!({"device":device.record.id,"expected_revision":"1","granted":false}),
    )
    .await
    .unwrap();
    assert!(matches!(
        wire(
            &running,
            origin,
            Operation::List.spec(),
            json!({"path":null})
        )
        .await,
        Err(ApiError::Unauthorized)
    ));
    assert!(running.shutdown().await.is_clean());
}
