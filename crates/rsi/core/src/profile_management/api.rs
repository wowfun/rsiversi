use super::{Arc, BoxFuture, Manager, Principal, Reply, wire};
use rsi_api_protocol::{ApiRegistrar, ApiRegistration, Result, json_handler};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn human<I, O>(
    registrar: &dyn ApiRegistrar,
    owner: Arc<Manager>,
    operation: wire::Operation,
    work: fn(Arc<Manager>, Principal, I) -> BoxFuture<'static, Reply<O>>,
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
                let (principal, lease) = owner.human(&context.origin)?;
                owner
                    .run(&principal.clone(), move |owner| {
                        Box::pin(async move {
                            let _lease = lease;
                            work(owner, principal, input).await
                        })
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
    let mut registrations = vec![
        human(
            registrar,
            owner.clone(),
            O::Catalog,
            |owner, principal, input| {
                Box::pin(async move { owner.catalog(principal, input).await })
            },
        )?,
        human(
            registrar,
            owner.clone(),
            O::Preview,
            |owner, principal, input| {
                Box::pin(async move { owner.preview(principal, input).await })
            },
        )?,
        human(
            registrar,
            owner.clone(),
            O::Commit,
            |owner, principal, input| Box::pin(async move { owner.commit(principal, input).await }),
        )?,
    ];
    let service = owner.clone();
    registrations.push(registrar.register(
        O::Grants.spec(),
        json_handler(move |context, _: Empty| {
            let owner = service.clone();
            async move {
                require_local(&context.origin)?;
                owner
                    .run(&Principal::Local, move |owner| {
                        Box::pin(async move { owner.grants() })
                    })?
                    .await
            }
        }),
    )?);
    registrations.push(registrar.register(
        O::SetGrant.spec(),
        json_handler(move |context, input: wire::SetGrant| {
            let owner = owner.clone();
            async move {
                require_local(&context.origin)?;
                owner
                    .run_grant(move |owner| Box::pin(async move { owner.set_grant(input).await }))?
                    .await
            }
        }),
    )?);
    Ok(registrations)
}

fn require_local(origin: &rsi_api_protocol::CallOrigin) -> Result<()> {
    match origin {
        rsi_api_protocol::CallOrigin::Local => Ok(()),
        rsi_api_protocol::CallOrigin::Device(_) => Err(rsi_api_protocol::ApiError::Unauthorized),
    }
}
