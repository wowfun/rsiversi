use super::{ApiError, Arc, ConfigurationAccess, Result, Serialize};
use rsi_api_protocol::{ApiRegistrar, ApiRegistration, json_handler};
use rsi_configuration_api::{ConfigurationOperation, PluginStatusRequest, PluginStatusSource};
#[derive(Serialize)]
enum Never {}
/// Registers the narrow read through the actual configuration grant owner.
pub fn register_plugin_status(
    registrar: &dyn ApiRegistrar,
    owner: Arc<ConfigurationAccess>,
    source: Arc<dyn PluginStatusSource>,
) -> Result<ApiRegistration> {
    registrar.register(
        ConfigurationOperation::Plugins.spec(),
        json_handler(move |context, request: PluginStatusRequest| {
            let owner = owner.clone();
            let source = source.clone();
            async move {
                request.validate()?;
                let _lease = owner.admit(&context.origin)?;
                let page = source.plugins(request).map_err(|_| ApiError::Unavailable)?;
                page.validate(request).map_err(|_| ApiError::Unavailable)?;
                Ok(Ok::<_, Never>(page))
            }
        }),
    )
}
