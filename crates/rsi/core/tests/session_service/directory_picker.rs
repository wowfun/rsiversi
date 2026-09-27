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
