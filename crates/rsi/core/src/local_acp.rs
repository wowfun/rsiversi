use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_service_host::{LocalServiceListener, ServiceOwnerContract};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct ListenerFactory;
#[async_trait]
impl PluginFactory for ListenerFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(failure("ACP listener takes no configuration"));
        }
        Ok(PreparedActivation::new(desired.clone())
            .requiring_local::<ServiceOwnerContract>()
            .requiring_local::<rsi_api_protocol::ConnectionDescriptionContract>()
            .requiring_local::<rsi_session_protocol::SessionContract>()
            .requiring_local::<rsi_session_protocol::SessionDraftControlContract>()
            .requiring_local::<rsi_workspace_protocol::WorkspaceRegistryContract>()
            .requiring_local::<rsi_agent_store_protocol::SessionStoreContract>()
            .requiring_local::<crate::acp_inputs::InputsContract>()
            .requiring_local::<rsi_ai_protocol::LanguageModelsContract>()
            .requiring_local::<rsi_agent_turn_protocol::TurnServiceContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let owner = plan.local::<ServiceOwnerContract>()?;
        let description = plan.local::<rsi_api_protocol::ConnectionDescriptionContract>()?;
        let listener = LocalServiceListener::bind(owner, "acp").map_err(failure)?;
        let hello = rsi_acp_agent::encode_attachment_hello(&rsi_acp_agent::AttachmentHello {
            epoch: description.host_epoch.clone(),
            protocol: rsi_acp_agent::attachment_protocol(),
        })
        .map_err(failure)?;
        let context = plan.context().clone();
        let stop = CancellationToken::new();
        let guard = stop.clone().drop_guard();
        let task = context.runtime().execution().clone().spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let accepted = tokio::select! { biased;
                    () = stop.cancelled() => break,
                    _ = tasks.join_next(), if !tasks.is_empty() => continue,
                    stream = listener.accept() => stream,
                };
                let Ok(mut stream) = accepted else {
                    tokio::select! { () = stop.cancelled() => break, () = tokio::time::sleep(Duration::from_millis(50)) => {} }
                    continue;
                };
                if tasks.len() >= 16 { continue; }
                let context = context.clone();
                let hello = hello.clone();
                let stop = stop.child_token();
                tasks.spawn(async move {
                    let admitted = tokio::time::timeout(Duration::from_secs(3), async {
                        stream.write_all(&hello).await?;
                        stream.flush().await
                    }).await;
                    if !matches!(admitted, Ok(Ok(()))) { return; }
                    let Ok(backend) = crate::acp_owner::backend(&context) else { return; };
                    let (read, write) = tokio::io::split(stream);
                    let peer = rsi_acp::Peer::start(rsi_acp::StreamTransport::new(read, write));
                    let _ = rsi_acp::server::run(peer, backend, stop).await;
                });
            }
            // Server shutdown owns backend cancellation and private attachment cleanup.
            if tokio::time::timeout(Duration::from_secs(35), async {
                while tasks.join_next().await.is_some() {}
            }).await.is_err() {
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                return Err("ACP attachment drain timed out".into());
            }
            Ok(())
        });
        plan.defer(
            "drain resident ACP attachments",
            Box::new(move || {
                Box::pin(async move {
                    drop(guard);
                    task.await
                        .map_err(|_| "ACP listener task failed".to_owned())?
                })
            }),
        )
    }
}

pub(crate) async fn attach(
    plan: ActivationPlan,
    composition: &crate::StandardComposition,
) -> rsi_meta::Result<()> {
    composition
        .host_launcher
        .as_ref()
        .ok_or_else(|| failure("ACP needs a Host launcher"))?
        .ensure_started(composition.paths())
        .await
        .map_err(failure)?;
    let paths = rsi_service_host::ServiceHostPaths::from_host_paths(composition.paths())
        .map_err(failure)?;
    let owner = paths
        .read_metadata()
        .map_err(failure)?
        .ok_or_else(|| failure("ACP Host has no active identity"))?;
    let transport = tokio::time::timeout(Duration::from_secs(3), async {
        let mut stream = rsi_service_host::connect_local_service(&owner, "acp")
            .await
            .map_err(failure)?;
        let mut header = [0; 4];
        stream.read_exact(&mut header).await.map_err(failure)?;
        let length = rsi_acp_agent::decode_attachment_length(header).map_err(failure)?;
        let mut bytes = vec![0; length + 4];
        bytes[..4].copy_from_slice(&header);
        stream.read_exact(&mut bytes[4..]).await.map_err(failure)?;
        let hello = rsi_acp_agent::decode_attachment_hello(&bytes).map_err(failure)?;
        if hello.epoch != owner.host_epoch || hello.protocol != rsi_acp_agent::attachment_protocol()
        {
            return Err(failure(
                "ACP Host generation or wire contract differs; restart the Host",
            ));
        }
        let (read, write) = tokio::io::split(stream);
        Ok::<_, MetaError>(rsi_acp::StreamTransport::new(read, write))
    })
    .await
    .map_err(|_| failure("ACP attachment deadline elapsed"))??;
    let supply = plan
        .context()
        .provide_local::<crate::application_services::AcpAttachmentContract>(Arc::new(
            crate::application_services::AcpAttachment::Remote(transport.clone()),
        ))?;
    plan.defer(
        "close ACP client attachment",
        Box::new(move || {
            Box::pin(async move {
                drop(supply);
                rsi_acp::Transport::close(transport.as_ref())
                    .await
                    .map_err(|_| "ACP attachment close failed".into())
            })
        }),
    )
}
fn failure(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
