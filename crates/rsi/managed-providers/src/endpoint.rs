use super::{Arc, Deserialize, ManagedProvider, ManagedProviders, Result, Serialize};
use rsi_api_protocol::{ApiRegistrar, ApiRegistration, json_handler};
use rsi_configuration_api::ProvidersOperation;
#[derive(Serialize)]
enum Never {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Replace {
    expected_revision: String,
    deployments: Vec<ManagedProvider>,
}
pub(super) fn register(
    registrar: &dyn ApiRegistrar,
    owner: Arc<ManagedProviders>,
) -> Result<Vec<ApiRegistration>> {
    let service = owner.clone();
    let read = registrar.register(
        ProvidersOperation::Read.spec(),
        json_handler(move |_context, _: Empty| {
            let service = service.clone();
            async move { Ok(Ok::<_, Never>(service.snapshot())) }
        }),
    )?;
    let replace = registrar.register(
        ProvidersOperation::Replace.spec(),
        json_handler(move |context, input: Replace| {
            let service = owner.clone();
            async move {
                service
                    .replace(&context.origin, &input.expected_revision, input.deployments)?
                    .await
                    .map(Ok::<_, Never>)
            }
        }),
    )?;
    Ok(vec![read, replace])
}
