use super::{Arc, BoxFuture, CallOrigin, Deserialize, Manager, Reply, Serialize, wire};
use rsi_api_protocol::{ApiRegistrar, ApiRegistration, Result, json_handler};
use serde::de::DeserializeOwned;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
fn endpoint<I, O>(
    registrar: &dyn ApiRegistrar,
    owner: Arc<Manager>,
    operation: wire::Operation,
    work: fn(Arc<Manager>, CallOrigin, I) -> BoxFuture<'static, Reply<O>>,
) -> Result<ApiRegistration>
where
    I: DeserializeOwned + Send + 'static,
    O: Serialize + Send + 'static,
{
    registrar.register(
        operation.spec(),
        json_handler(move |context, input: I| {
            let owner = owner.clone();
            async move {
                owner
                    .run(context.origin, move |owner, origin| {
                        work(owner, origin, input)
                    })?
                    .await
            }
        }),
    )
}
pub(super) fn register(
    registrar: &dyn ApiRegistrar,
    owner: Arc<Manager>,
) -> Result<Vec<ApiRegistration>> {
    use wire::Operation as O;
    Ok(vec![
        endpoint(
            registrar,
            owner.clone(),
            O::Catalog,
            |owner, origin, _: Empty| Box::pin(async move { owner.catalog(&origin) }),
        )?,
        endpoint(
            registrar,
            owner.clone(),
            O::PutCandidate,
            |owner, origin, input| Box::pin(async move { owner.put(origin, input).await }),
        )?,
        endpoint(
            registrar,
            owner.clone(),
            O::ConfirmTrust,
            |owner, origin, input| Box::pin(async move { owner.trust(origin, input).await }),
        )?,
        endpoint(
            registrar,
            owner.clone(),
            O::Connect,
            |owner, origin, input| Box::pin(async move { owner.connect(origin, input).await }),
        )?,
        endpoint(
            registrar,
            owner.clone(),
            O::ResolveDirectory,
            |owner, origin, input| {
                Box::pin(async move { owner.resolve_directory(origin, input).await })
            },
        )?,
        endpoint(registrar, owner, O::Disconnect, |owner, origin, input| {
            Box::pin(async move { owner.disconnect(origin, input).await })
        })?,
    ])
}
