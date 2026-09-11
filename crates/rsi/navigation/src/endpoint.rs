use super::{
    Arc, Deserialize, Navigation, NavigationCursor, NavigationFilter, Result, Serialize, SessionId,
    SessionMetadata,
};
use rsi_api_protocol::{ApiRegistrar, ApiRegistration, json_handler};
use rsi_navigation_api::NavigationOperation;
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
        json_handler(move |_context, input: Query| {
            let service = service.clone();
            async move {
                service
                    .query(input.filter, input.after)?
                    .await
                    .map(Ok::<_, Never>)
            }
        }),
    )?;
    let replace = registrar.register(
        NavigationOperation::Replace.spec(),
        json_handler(move |_context, input: Replace| {
            let service = owner.clone();
            async move {
                service
                    .replace(input.session, &input.expected_revision, input.metadata)?
                    .await
                    .map(Ok::<_, Never>)
            }
        }),
    )?;
    Ok(vec![query, replace])
}
