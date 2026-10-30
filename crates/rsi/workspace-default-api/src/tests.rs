use super::*;
use rsi_api_protocol::{
    ApiOutput, ByteBudget, ConnectionDescription, OperationCatalog, RetainedBytes,
};

#[derive(Debug)]
struct FailedDirectory(String);
#[async_trait]
impl ApiClient for FailedDirectory {
    fn description(&self) -> &ConnectionDescription {
        unreachable!("no discovery")
    }
    async fn operations(&self) -> rsi_api_protocol::Result<OperationCatalog> {
        unreachable!("no discovery")
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(
        &self,
        _: &OperationSpec,
        _: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        Err(ApiError::Domain(
            ByteBudget::default().encode(&self.0, 128 * 1024).unwrap(),
        ))
    }
}

#[tokio::test]
async fn environmental_failure_is_a_bounded_backend_diagnostic_not_invalid_input() {
    for message in [
        "Directory is temporarily unavailable".into(),
        "界".repeat(513),
    ] {
        let result = Client::new(Arc::new(FailedDirectory(message)))
            .initialize()
            .await;
        let Err(ApiError::Backend(message)) = result else {
            panic!("{result:?}");
        };
        assert!(!message.is_empty() && message.len() <= 512);
    }
}
