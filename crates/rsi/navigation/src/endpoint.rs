use super::{
    Arc, Deserialize, Navigation, NavigationCursor, NavigationFilter, Result, Serialize, SessionId,
    SessionMetadata,
};
use rsi_api_protocol::{ApiRegistrar, ApiRegistration, json_handler};
use rsi_navigation_api::{NavigationOperation, OrderScope, SummaryRequest};
#[derive(Serialize)]
enum Never {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    filter: NavigationFilter,
    after: Option<NavigationCursor>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Replace {
    session: SessionId,
    expected_revision: String,
    metadata: SessionMetadata,
}
pub(super) fn register(
    registrar: &dyn ApiRegistrar,
    owner: Arc<Navigation>,
) -> Result<Vec<ApiRegistration>> {
    let service = owner.clone();
    let query = registrar.register(
        NavigationOperation::Query.spec(),
        json_handler(move |context, input: Query| {
            let service = service.clone();
            async move {
                service
                    .query(&context.origin, input.filter, input.after)?
                    .await
                    .map(Ok::<_, Never>)
            }
        }),
    )?;
    let pinned_owner = owner.clone();
    let pinned = registrar.register(
        NavigationOperation::Pinned.spec(),
        json_handler(move |context, filter: NavigationFilter| {
            let owner = pinned_owner.clone();
            async move {
                owner
                    .pinned(context.origin, filter)?
                    .await
                    .map(Ok::<_, Never>)
            }
        }),
    )?;
    let seed_owner = owner.clone();
    let seed = registrar.register(
        NavigationOperation::OrderSeed.spec(),
        json_handler(move |context, scope: OrderScope| {
            let owner = seed_owner.clone();
            async move {
                owner
                    .order_seed(&context.origin, scope)?
                    .await
                    .map(Ok::<_, Never>)
            }
        }),
    )?;
    let summaries_owner = owner.clone();
    let summaries = registrar.register(
        NavigationOperation::Summaries.spec(),
        json_handler(move |context, request: SummaryRequest| {
            let owner = summaries_owner.clone();
            async move {
                owner
                    .summaries(&context.origin, request)?
                    .await
                    .map(Ok::<_, Never>)
            }
        }),
    )?;
    let replace = registrar.register(
        NavigationOperation::Replace.spec(),
        json_handler(move |context, input: Replace| {
            let service = owner.clone();
            async move {
                service
                    .replace(
                        context.origin,
                        input.session,
                        &input.expected_revision,
                        input.metadata,
                    )?
                    .await
                    .map(Ok::<_, Never>)
            }
        }),
    )?;
    Ok(vec![query, pinned, seed, summaries, replace])
}
