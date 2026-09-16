use super::*;

#[derive(Debug)]
struct Unused;
#[async_trait::async_trait]
impl CredentialsResolve for Unused {
    async fn resolve(
        &self,
        _: &rsi_credentials_protocol::CredentialRef,
    ) -> rsi_credentials_protocol::Result<rsi_credentials_protocol::ResolvedCredential> {
        unreachable!()
    }
}
#[async_trait::async_trait]
impl Sandbox for Unused {
    async fn confine(
        &self,
        _: rsi_sandbox::ProcessRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
        unreachable!()
    }
    async fn workspace_read(
        &self,
        _: rsi_sandbox::WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
        unreachable!()
    }
}
impl DuplexProcess for Unused {
    fn spawn(
        &self,
        _: rsi_process::DuplexProcessSpec,
    ) -> rsi_process::Result<rsi_process::ManagedDuplexProcess> {
        unreachable!()
    }
}
#[tokio::test(start_paused = true)]
async fn configuration_wait_is_bounded_but_retirement_keeps_admission_until_settled() {
    let service = McpService::new(Arc::new(Unused), Arc::new(Unused), Arc::new(Unused));
    let entry = Arc::new(Entry::new(ServerConfig {
        id: "held".into(),
        enabled: true,
        tools: vec![],
        transport: TransportConfig::StreamableHttp {
            url: "http://127.0.0.1/".into(),
            credential: None,
        },
    }));
    let held = entry.refresh.clone().acquire_owned().await.unwrap();
    service
        .entries
        .write()
        .unwrap()
        .insert("held".into(), entry);
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(31),
        service.configure(McpConfig::default()),
    )
    .await;
    assert_eq!(outcome, Ok(Err(McpError::Timeout)));
    assert!(
        service.status().is_empty(),
        "replacement has already applied"
    );
    assert_eq!(
        service.configure(McpConfig::default()).await,
        Err(McpError::Busy)
    );
    drop(held);
    tokio::time::timeout(std::time::Duration::from_secs(1), service.shutdown())
        .await
        .unwrap();
}
