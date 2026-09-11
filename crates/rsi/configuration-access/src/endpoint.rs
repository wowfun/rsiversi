use super::{
    ApiError, Arc, CallOrigin, ConfigurationAccess, Deserialize, DeviceId, Result, Serialize,
};
use rsi_api_protocol::{ApiRegistrar, ApiRegistration, json_handler};
use rsi_configuration_api::ConfigurationOperation;

#[derive(Serialize)]
enum Never {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    device: DeviceId,
    expected_revision: String,
    granted: bool,
}
pub(super) fn register(
    registrar: &dyn ApiRegistrar,
    owner: Arc<ConfigurationAccess>,
) -> Result<Vec<ApiRegistration>> {
    let mut registrations = Vec::new();
    let service = owner.clone();
    registrations.push(registrar.register(
        ConfigurationOperation::Status.spec(),
        json_handler(move |context, _: Empty| {
            let service = service.clone();
            async move {
                Ok(Ok::<_, Never>(
                    serde_json::json!({"allowed":service.allowed(&context.origin)}),
                ))
            }
        }),
    )?);
    let service = owner.clone();
    registrations.push(registrar.register(
        ConfigurationOperation::Grants.spec(),
        json_handler(move |context, _: Empty| {
            let service = service.clone();
            async move {
                if !matches!(context.origin, CallOrigin::Local) {
                    return Err(ApiError::Unauthorized);
                }
                service.snapshot().await.map(Ok::<_, Never>)
            }
        }),
    )?);
    registrations.push(registrar.register(
        ConfigurationOperation::SetGrant.spec(),
        json_handler(move |context, input: Change| {
            let service = owner.clone();
            async move {
                service
                    .set_grant(
                        &context.origin,
                        input.device,
                        &input.expected_revision,
                        input.granted,
                    )?
                    .await
                    .map(Ok::<_, Never>)
            }
        }),
    )?);
    Ok(registrations)
}
