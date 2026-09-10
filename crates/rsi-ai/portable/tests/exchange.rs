mod common;
use async_trait::async_trait;
use futures_util::StreamExt as _;
use rsi_ai_portable::PortableProviderFactory;
use rsi_ai_protocol::{
    AiError, ContentDelta, ContentStart, DispatchStatus, ErrorKind, ErrorPhase, FinishReason,
    ImageToolResultCapability, LanguageAssembler, LanguageCallContract, LanguageEvent,
    LanguageProfile, LanguageRequest, LanguageSettings, Message as AiMessage, MessageContent,
    ModelRef, ToolDialect,
    portable::{
        self, ControlRequest, ControlResponse, Decoder, Description, ImageModel, Kind,
        LanguageFeature, LanguageModel,
    },
};
use rsi_api_protocol::ByteBudget;
use rsi_meta::{
    ActivationPlan, ConfigValue, ContractVersion, Message, PluginFactory, PreparedActivation,
    ProviderChannel, Runtime, ServiceEndpoint,
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug)]
enum Behavior {
    Echo,
    AlterSnapshot,
    PrepareCredential,
    OversizedState,
    DependencyFlood,
    ExtraPrepare,
    Missing,
    ExtraTerminal,
    Cancel,
    Media,
    UnauthorizedMedia,
    BadMediaPage,
    FailureText,
    NoModels,
    DuplicateModels,
    WrongStartPhase,
}
#[derive(Debug)]
struct Endpoint {
    behavior: Behavior,
    prepares: AtomicUsize,
    starts: AtomicUsize,
    entered: Semaphore,
    ended: Semaphore,
    frozen: Mutex<Option<Value>>,
}
impl Endpoint {
    fn description(&self) -> Description {
        let model = LanguageModel {
            model: "native-text".into(),
            profile: LanguageProfile::new(
                8192,
                512,
                1024,
                ToolDialect::Responses,
                false,
                ImageToolResultCapability::No,
                vec![],
            )
            .unwrap(),
            features: vec![LanguageFeature::InputImages],
            request_extensions: vec![],
        };
        let language = match self.behavior {
            Behavior::NoModels => vec![],
            Behavior::DuplicateModels => vec![model.clone(), model],
            _ => vec![model],
        };
        Description {
            language,
            image: vec![ImageModel {
                model: "native-image".into(),
                maximum_count: 1,
                features: vec![],
            }],
        }
    }
}
async fn receive(channel: &mut ProviderChannel<'_>) -> portable::Packet {
    let mut decoder = Decoder::new(ByteBudget::default());
    loop {
        let message = channel.recv().await.unwrap();
        assert!(message.capabilities().is_empty());
        if let Some(packet) = decoder.push(message.as_bytes()).unwrap() {
            return packet;
        }
    }
}
async fn send(channel: &ProviderChannel<'_>, response: &ControlResponse) -> rsi_meta::Result<()> {
    let bytes = serde_json::to_vec(response).unwrap();
    for frame in portable::frames(Kind::Json, &bytes).unwrap() {
        channel.send(Message::new(frame)).await?;
    }
    Ok(())
}
#[async_trait]
impl ServiceEndpoint for Endpoint {
    #[allow(clippy::too_many_lines)] // One deterministic peer script exposes wrong-phase and terminal faults.
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> rsi_meta::Result<()> {
        let packet = receive(&mut channel).await;
        assert_eq!(packet.kind, Kind::Json);
        let request: ControlRequest = portable::decode_control(packet.bytes.as_bytes()).unwrap();
        assert!(
            !String::from_utf8_lossy(packet.bytes.as_bytes()).contains("native-test-credential")
        );
        drop(packet);
        match request {
            ControlRequest::Describe {} => {
                assert!(channel.recv().await.is_none());
                send(
                    &channel,
                    &ControlResponse::Description {
                        description: Box::new(self.description()),
                    },
                )
                .await?;
            }
            ControlRequest::PrepareLanguage { input } => {
                self.prepares.fetch_add(1, Ordering::SeqCst);
                assert!(channel.recv().await.is_none());
                *self.frozen.lock().unwrap() = Some(serde_json::to_value(&input).unwrap());
                let mut snapshot = input.snapshot;
                if matches!(self.behavior, Behavior::AlterSnapshot) {
                    snapshot.call_id = "changed".into();
                }
                if matches!(self.behavior, Behavior::PrepareCredential) {
                    send(&channel, &ControlResponse::Credential {}).await?;
                    return Ok(());
                }
                let response = ControlResponse::Prepared {
                    snapshot: Box::new(snapshot),
                    state: if matches!(self.behavior, Behavior::OversizedState) {
                        json!("x".repeat(portable::MAXIMUM_PREPARED_STATE_BYTES))
                    } else {
                        json!({"prepared":true})
                    },
                };
                send(&channel, &response).await?;
                if matches!(self.behavior, Behavior::ExtraPrepare) {
                    send(&channel, &response).await?;
                }
            }
            ControlRequest::StartLanguage { input, state } => {
                self.starts.fetch_add(1, Ordering::SeqCst);
                self.entered.add_permits(1);
                assert_eq!(state, json!({"prepared":true}));
                assert_eq!(
                    Some(serde_json::to_value(&input).unwrap()),
                    *self.frozen.lock().unwrap()
                );
                match self.behavior {
                    Behavior::Cancel => {
                        channel.cancellation().cancelled().await;
                        self.ended.add_permits(1);
                        return Ok(());
                    }
                    Behavior::Missing => return Ok(()),
                    Behavior::WrongStartPhase => {
                        send(
                            &channel,
                            &ControlResponse::Description {
                                description: Box::new(self.description()),
                            },
                        )
                        .await?;
                        return Ok(());
                    }
                    Behavior::FailureText => {
                        let failure = AiError::new(
                            ErrorKind::Server,
                            ErrorPhase::Stream,
                            DispatchStatus::Dispatched,
                            "native-test-credential",
                        )
                        .unwrap();
                        send(
                            &channel,
                            &ControlResponse::Language {
                                event: Box::new(LanguageEvent::Failed {
                                    error: failure,
                                    replay: None,
                                }),
                            },
                        )
                        .await?;
                        return Ok(());
                    }
                    Behavior::Media | Behavior::UnauthorizedMedia | Behavior::BadMediaPage => {
                        for _ in 0..2 {
                            send(
                                &channel,
                                &ControlResponse::Media {
                                    descriptor: common::descriptor(),
                                    offset: if matches!(self.behavior, Behavior::BadMediaPage) {
                                        3
                                    } else {
                                        0
                                    },
                                    length: 3,
                                },
                            )
                            .await?;
                            if !matches!(self.behavior, Behavior::Media) {
                                return Ok(());
                            }
                            let packet = receive(&mut channel).await;
                            assert_eq!(packet.kind, Kind::Binary);
                            assert_eq!(packet.bytes.as_bytes(), b"abc");
                        }
                    }
                    _ => {}
                }
                if matches!(self.behavior, Behavior::DependencyFlood) {
                    for index in 0..=portable::MAXIMUM_DEPENDENCY_REQUESTS {
                        send(&channel, &ControlResponse::Credential {}).await?;
                        if index == portable::MAXIMUM_DEPENDENCY_REQUESTS {
                            return Ok(());
                        }
                        let packet = receive(&mut channel).await;
                        assert_eq!(packet.kind, Kind::Binary);
                    }
                }
                send(&channel, &ControlResponse::Credential {}).await?;
                let credential = receive(&mut channel).await;
                assert_eq!(credential.kind, Kind::Binary);
                assert_eq!(credential.bytes.as_bytes(), b"native-test-credential");
                drop(credential);
                for event in [
                    LanguageEvent::ContentStarted {
                        index: 0,
                        content: ContentStart::Text,
                    },
                    LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::Text("received".into()),
                    },
                    LanguageEvent::ContentFinished { index: 0 },
                    LanguageEvent::Finished {
                        reason: FinishReason::Stop,
                        replay: None,
                    },
                ] {
                    send(
                        &channel,
                        &ControlResponse::Language {
                            event: Box::new(event),
                        },
                    )
                    .await?;
                }
                if matches!(self.behavior, Behavior::ExtraTerminal) {
                    send(&channel, &ControlResponse::Credential {}).await?;
                }
            }
            ControlRequest::PrepareImage { input } => {
                send(
                    &channel,
                    &ControlResponse::Prepared {
                        snapshot: Box::new(input.snapshot),
                        state: Value::Null,
                    },
                )
                .await?;
            }
            ControlRequest::StartImage { .. } => {
                use portable::ImageHeader;
                send(
                    &channel,
                    &ControlResponse::Image {
                        header: ImageHeader::OutputStarted {
                            index: 0,
                            mime_type: "image/png".into(),
                        },
                    },
                )
                .await?;
                send(
                    &channel,
                    &ControlResponse::Image {
                        header: ImageHeader::OutputChunk {
                            index: 0,
                            sequence: 1,
                        },
                    },
                )
                .await?;
                let kind = if matches!(self.behavior, Behavior::WrongStartPhase) {
                    Kind::Json
                } else {
                    Kind::Binary
                };
                for frame in
                    portable::frames(kind, &vec![42; portable::MAXIMUM_BINARY_BYTES]).unwrap()
                {
                    channel.send(Message::new(frame)).await?;
                }
                send(
                    &channel,
                    &ControlResponse::Image {
                        header: ImageHeader::OutputFinished { index: 0 },
                    },
                )
                .await?;
                if !matches!(self.behavior, Behavior::Missing) {
                    send(
                        &channel,
                        &ControlResponse::Image {
                            header: ImageHeader::Finished {},
                        },
                    )
                    .await?;
                }
                if matches!(self.behavior, Behavior::ExtraTerminal) {
                    send(&channel, &ControlResponse::Credential {}).await?;
                }
            }
        }
        Ok(())
    }
}
#[derive(Debug)]
struct Supply(Arc<Endpoint>);
#[async_trait]
impl PluginFactory for Supply {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context().provide(
            "test.ai",
            portable::PROVIDER_CONTRACT,
            ContractVersion(portable::PROVIDER_VERSION),
            self.0.clone(),
        )?;
        Ok(())
    }
}
async fn harness(
    behavior: Behavior,
) -> (
    Runtime,
    Arc<Endpoint>,
    Arc<AtomicUsize>,
    rsi_meta::FiberHandle,
) {
    let (runtime, reads) = common::base().await;
    let endpoint = Arc::new(Endpoint {
        behavior,
        prepares: AtomicUsize::new(0),
        starts: AtomicUsize::new(0),
        entered: Semaphore::new(0),
        ended: Semaphore::new(0),
        frozen: Mutex::new(None),
    });
    runtime
        .root()
        .apply(
            common::linked("supply", Supply(endpoint.clone())),
            Value::Null,
        )
        .await
        .unwrap();
    let bridge = runtime
        .root()
        .apply(
            common::linked("bridge", PortableProviderFactory),
            common::config("test.ai"),
        )
        .await
        .unwrap();
    (runtime, endpoint, reads, bridge)
}
fn request(media: bool) -> LanguageRequest {
    LanguageRequest::new(vec![if media {
        AiMessage::user(vec![MessageContent::Image(common::descriptor())]).unwrap()
    } else {
        AiMessage::user_text("hello".repeat(20_000)).unwrap()
    }])
    .unwrap()
}
fn model() -> ModelRef {
    ModelRef::new("native", "native-text").unwrap()
}
#[tokio::test]
async fn frozen_prepare_fragmented_start_and_request_owned_media() {
    for behavior in [Behavior::Echo, Behavior::Media] {
        let (runtime, endpoint, reads, bridge) = harness(behavior).await;
        assert_eq!(bridge.snapshot().state, rsi_meta::FiberState::Active);
        let calls = runtime
            .root()
            .lookup_local::<LanguageCallContract>()
            .unwrap();
        let prepared = calls
            .prepare(model(), request(matches!(behavior, Behavior::Media)))
            .await
            .unwrap();
        assert_eq!(endpoint.prepares.load(Ordering::SeqCst), 1);
        assert_eq!(endpoint.starts.load(Ordering::SeqCst), 0);
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        let mut stream = prepared.start(CancellationToken::new()).await.unwrap();
        let mut assembler = LanguageAssembler::default();
        while let Some(event) = stream.next().await {
            assembler.push(&event.unwrap()).unwrap();
        }
        assert_eq!(assembler.finish().unwrap().visible_text(), "received");
        assert_eq!(endpoint.starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            reads.load(Ordering::SeqCst),
            usize::from(matches!(behavior, Behavior::Media))
        );
        drop(stream);
        drop(calls);
        assert!(runtime.shutdown().await.is_clean());
    }
}
#[tokio::test]
async fn invalid_prepare_never_starts_or_reads_dependencies() {
    for behavior in [
        Behavior::AlterSnapshot,
        Behavior::PrepareCredential,
        Behavior::ExtraPrepare,
        Behavior::OversizedState,
    ] {
        let (runtime, endpoint, reads, _) = harness(behavior).await;
        let calls = runtime
            .root()
            .lookup_local::<LanguageCallContract>()
            .unwrap();
        let failure = calls.prepare(model(), request(false)).await.unwrap_err();
        assert_eq!(failure.dispatch_status(), DispatchStatus::NotDispatched);
        assert_eq!(endpoint.starts.load(Ordering::SeqCst), 0);
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        drop(calls);
        assert!(runtime.shutdown().await.is_clean());
    }
}
#[tokio::test]
async fn invalid_start_never_commits_success_or_grants_unrequested_media() {
    for behavior in [
        Behavior::Missing,
        Behavior::ExtraTerminal,
        Behavior::UnauthorizedMedia,
        Behavior::BadMediaPage,
        Behavior::WrongStartPhase,
        Behavior::DependencyFlood,
    ] {
        let (runtime, _, reads, _) = harness(behavior).await;
        let calls = runtime
            .root()
            .lookup_local::<LanguageCallContract>()
            .unwrap();
        let prepared = calls
            .prepare(model(), request(matches!(behavior, Behavior::BadMediaPage)))
            .await
            .unwrap();
        let mut stream = prepared.start(CancellationToken::new()).await.unwrap();
        let mut error = None;
        while let Some(event) = stream.next().await {
            match event {
                Err(failure) => {
                    error = Some(failure);
                    break;
                }
                Ok(LanguageEvent::Finished { .. }) => {
                    panic!("invalid native exchange yielded successful terminal")
                }
                Ok(_) => {}
            }
        }
        assert_eq!(
            error.unwrap().dispatch_status(),
            DispatchStatus::Unknown,
            "{behavior:?}"
        );
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        drop(stream);
        drop(calls);
        assert!(runtime.shutdown().await.is_clean());
    }
}
#[tokio::test]
async fn compatibility_precedes_missing_credentials_and_invalid_declarations_publish_nothing() {
    let (runtime, endpoint, _, _) = harness(Behavior::Echo).await;
    runtime
        .root()
        .lookup_local::<rsi_credentials_protocol::CredentialsAdminContract>()
        .unwrap()
        .unset(&rsi_credentials_protocol::CredentialRef::new("fixture", "key").unwrap())
        .await
        .unwrap();
    let calls = runtime
        .root()
        .lookup_local::<LanguageCallContract>()
        .unwrap();
    let request = request(false)
        .with_settings(LanguageSettings::default().with_seed(1))
        .unwrap();
    assert_eq!(
        calls.prepare(model(), request).await.unwrap_err().kind(),
        ErrorKind::Unsupported
    );
    assert_eq!(endpoint.prepares.load(Ordering::SeqCst), 0);
    drop(calls);
    assert!(runtime.shutdown().await.is_clean());
    for behavior in [Behavior::NoModels, Behavior::DuplicateModels] {
        let (runtime, _, _, bridge) = harness(behavior).await;
        assert_ne!(bridge.snapshot().state, rsi_meta::FiberState::Active);
        assert!(
            runtime
                .root()
                .lookup_local::<LanguageCallContract>()
                .unwrap()
                .describe(&model())
                .is_err()
        );
        assert!(runtime.shutdown().await.is_clean());
    }
}
#[tokio::test]
async fn native_error_text_is_replaced_with_static_safe_facts() {
    let (runtime, _, _, _) = harness(Behavior::FailureText).await;
    let calls = runtime
        .root()
        .lookup_local::<LanguageCallContract>()
        .unwrap();
    let prepared = calls.prepare(model(), request(false)).await.unwrap();
    let mut stream = prepared.start(CancellationToken::new()).await.unwrap();
    let event = stream.next().await.unwrap().unwrap();
    assert!(!format!("{event:?}").contains("native-test-credential"));
    assert!(
        matches!(event,LanguageEvent::Failed { error, .. } if error.kind() == ErrorKind::Server && error.dispatch_status() == DispatchStatus::Dispatched)
    );
    assert!(stream.next().await.is_none());
    drop(stream);
    drop(calls);
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn pre_cancel_active_cancel_and_drop_have_one_attempt_and_release_provider_channel() {
    for mode in 0..3 {
        let (runtime, endpoint, _, _) = harness(Behavior::Cancel).await;
        let calls = runtime
            .root()
            .lookup_local::<LanguageCallContract>()
            .unwrap();
        let prepared = calls.prepare(model(), request(false)).await.unwrap();
        let cancellation = CancellationToken::new();
        if mode == 0 {
            cancellation.cancel();
            assert!(prepared.start(cancellation).await.is_err());
            assert_eq!(endpoint.starts.load(Ordering::SeqCst), 0);
        } else {
            let mut stream = prepared.start(cancellation.clone()).await.unwrap();
            endpoint.entered.acquire().await.unwrap().forget();
            if mode == 1 {
                cancellation.cancel();
                assert_eq!(
                    stream.next().await.unwrap().unwrap_err().kind(),
                    ErrorKind::Cancelled
                );
            }
            drop(stream);
            tokio::time::timeout(std::time::Duration::from_secs(5), endpoint.ended.acquire())
                .await
                .unwrap()
                .unwrap()
                .forget();
            assert_eq!(endpoint.starts.load(Ordering::SeqCst), 1);
        }
        drop(calls);
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn image_binary_fragments_require_correct_kind_and_both_terminals() {
    use rsi_ai_protocol::{ImageAssembler, ImageCallContract, ImageEvent, ImageRequest};
    for behavior in [
        Behavior::Echo,
        Behavior::WrongStartPhase,
        Behavior::Missing,
        Behavior::ExtraTerminal,
    ] {
        let (runtime, _, _, _) = harness(behavior).await;
        let calls = runtime.root().lookup_local::<ImageCallContract>().unwrap();
        let prepared = calls
            .prepare(
                ModelRef::new("native", "native-image").unwrap(),
                ImageRequest::new("one", 1).unwrap(),
            )
            .await
            .unwrap();
        let mut stream = prepared.start(CancellationToken::new()).await.unwrap();
        let mut assembler = ImageAssembler::default();
        let mut failed = false;
        while let Some(event) = stream.next().await {
            match event {
                Ok(event) => {
                    if matches!(event, ImageEvent::Finished) {
                        assert!(matches!(behavior, Behavior::Echo));
                    }
                    assembler.push(&event).unwrap();
                }
                Err(error) => {
                    assert_eq!(error.dispatch_status(), DispatchStatus::Unknown);
                    failed = true;
                    break;
                }
            }
        }
        if matches!(behavior, Behavior::Echo) {
            assert_eq!(
                assembler.finish().unwrap().images[0].bytes.len(),
                portable::MAXIMUM_BINARY_BYTES
            );
            assert!(!failed);
        } else {
            assert!(failed);
            assert!(assembler.finish().is_err());
        }
        drop(stream);
        drop(calls);
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn retired_bridge_fences_prepared_start_and_facet_conflicts_publish_no_partial_route() {
    let (runtime, endpoint, _, bridge) = harness(Behavior::Echo).await;
    let calls = runtime
        .root()
        .lookup_local::<LanguageCallContract>()
        .unwrap();
    let prepared = calls.prepare(model(), request(false)).await.unwrap();
    assert!(bridge.dispose().await.is_clean());
    assert!(prepared.start(CancellationToken::new()).await.is_err());
    assert_eq!(endpoint.starts.load(Ordering::SeqCst), 0);
    assert!(calls.describe(&model()).is_err());
    let mut image_config = common::config("test.ai");
    image_config["language"] = json!(false);
    let image = runtime
        .root()
        .apply(
            common::linked("image-bridge", PortableProviderFactory),
            image_config,
        )
        .await
        .unwrap();
    assert_eq!(image.snapshot().state, rsi_meta::FiberState::Active);
    let failed = runtime
        .root()
        .apply(
            common::linked("both-bridge", PortableProviderFactory),
            common::config("test.ai"),
        )
        .await
        .unwrap();
    assert_ne!(failed.snapshot().state, rsi_meta::FiberState::Active);
    assert!(calls.describe(&model()).is_err());
    drop(calls);
    assert!(runtime.shutdown().await.is_clean());
}
