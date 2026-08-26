use super::*;

#[derive(Debug)]
struct ZeroResponses;

#[async_trait]
impl ServiceEndpoint for ZeroResponses {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        channel.recv().await.expect("unary request");
        Ok(())
    }
}

#[derive(Debug)]
struct TwoResponses;

#[async_trait]
impl ServiceEndpoint for TwoResponses {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        let request = channel.recv().await.expect("unary request");
        channel.send(request.clone()).await?;
        channel.send(request).await
    }
}

async fn invoke_endpoint(endpoint: Arc<dyn ServiceEndpoint>) -> MetaError {
    let runtime = Runtime::default();
    let _provider = support::install_provider(&runtime, "unary-provider", "unary", endpoint).await;
    let (_consumer, capture) =
        support::install_consumer(&runtime, "unary-consumer", vec!["unary"]).await;
    capture.capabilities[0]
        .invoke(Message::new(b"one".as_slice()))
        .await
        .unwrap_err()
}

#[tokio::test]
async fn invoke_rejects_zero_responses() {
    assert_eq!(
        invoke_endpoint(Arc::new(ZeroResponses)).await,
        MetaError::Service("provider ended a unary call without a response".to_owned()),
    );
}

#[tokio::test]
async fn invoke_rejects_a_second_response_before_clean_terminal() {
    assert_eq!(
        invoke_endpoint(Arc::new(TwoResponses)).await,
        MetaError::Service("provider produced more than one unary response".to_owned()),
    );
}
