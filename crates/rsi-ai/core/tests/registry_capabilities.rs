use futures_util::StreamExt;
use rsi_ai::{ModelRef, Registry};
use rsi_ai_auth::CredentialManager;
use rsi_ai_protocol::{
    AiError, DispatchStatus, ErrorKind, ErrorPhase, ImageEvent, ImageRequest, MediaDescriptor,
    MediaKind, RealtimeCloseReason, RealtimeCommand, RealtimeEvent, RealtimeRequest, SpeechEvent,
    SpeechFormat, SpeechRequest, TranscriptionEvent, TranscriptionRequest,
};
use rsi_ai_provider::{
    AdapterFuture, ImageAdapter, ImageAdapterStream, PrepareContext, Prepared, ProviderRegistration,
};
use rsi_ai_testkit::{
    ScriptedImageAdapter, ScriptedRealtimeAdapter, ScriptedSpeechAdapter,
    ScriptedTranscriptionAdapter,
};

fn audio() -> MediaDescriptor {
    MediaDescriptor::new(
        MediaKind::Audio,
        "audio/wav",
        11,
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9",
    )
    .expect("audio")
}

fn registry(
    image: ScriptedImageAdapter,
    transcription: ScriptedTranscriptionAdapter,
    speech: ScriptedSpeechAdapter,
    realtime: ScriptedRealtimeAdapter,
) -> Registry {
    Registry::builder(CredentialManager::builder().build())
        .register(
            ProviderRegistration::builder("multimedia", "scripted")
                .expect("registration")
                .with_image(image)
                .with_transcription(transcription)
                .with_speech(speech)
                .with_realtime(realtime)
                .build()
                .expect("provider"),
        )
        .expect("register")
        .build()
        .expect("registry")
}

#[derive(Debug)]
struct FailingImageAdapter(AiError);

impl ImageAdapter for FailingImageAdapter {
    fn prepare(
        &self,
        context: PrepareContext,
        _model: String,
        _request: ImageRequest,
    ) -> AdapterFuture<Result<Prepared<ImageAdapterStream>, AiError>> {
        let snapshot = context.snapshot().clone();
        let error = self.0.clone();
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |_abort| {
                Box::pin(async move {
                    Ok(Box::pin(futures_util::stream::iter([Err(error)])) as ImageAdapterStream)
                })
            }))
        })
    }
}

#[tokio::test]
async fn media_generation_preserves_structured_provider_error_facts() {
    let provider_error = AiError::new(
        ErrorKind::Server,
        ErrorPhase::Stream,
        DispatchStatus::Dispatched,
        "provider failed",
    )
    .expect("error")
    .with_status(503)
    .expect("valid HTTP status")
    .with_retry_after_ms(900);
    let registry = Registry::builder(CredentialManager::builder().build())
        .register(
            ProviderRegistration::builder("failure", "scripted")
                .expect("registration")
                .with_image(FailingImageAdapter(provider_error.clone()))
                .build()
                .expect("provider"),
        )
        .expect("register")
        .build()
        .expect("registry");
    let model = registry
        .image(ModelRef::new("failure", "image-v1").expect("model"))
        .expect("image");
    let mut generation = model
        .prepare(ImageRequest::new("dot", 1).expect("request"))
        .await
        .expect("prepare")
        .start()
        .await
        .expect("start");
    assert!(generation.next().await.is_none());
    let error = generation.finish().expect_err("provider failure");
    assert_eq!(error.provider_error(), Some(&provider_error));
}

#[tokio::test]
async fn image_transcription_and_speech_each_stream_through_typed_handles() {
    let image = ScriptedImageAdapter::new(vec![
        ImageEvent::OutputStarted {
            index: 0,
            mime_type: "image/png".to_owned(),
        },
        ImageEvent::OutputChunk {
            index: 0,
            sequence: 1,
            bytes: vec![137, 80, 78, 71],
        },
        ImageEvent::OutputFinished { index: 0 },
        ImageEvent::Finished,
    ]);
    let transcription = ScriptedTranscriptionAdapter::new(vec![
        TranscriptionEvent::TextDelta {
            text: "hello".to_owned(),
        },
        TranscriptionEvent::Finished {
            language: Some("en".to_owned()),
        },
    ]);
    let speech = ScriptedSpeechAdapter::new(vec![
        SpeechEvent::OutputStarted {
            mime_type: "audio/pcm".to_owned(),
        },
        SpeechEvent::AudioChunk {
            sequence: 1,
            bytes: vec![0, 1],
        },
        SpeechEvent::OutputFinished,
        SpeechEvent::Finished,
    ]);
    let registry = registry(
        image.clone(),
        transcription.clone(),
        speech.clone(),
        ScriptedRealtimeAdapter::new(Vec::new()),
    );

    let mut generation = registry
        .image(ModelRef::new("multimedia", "image-v1").expect("model"))
        .expect("image model")
        .prepare(ImageRequest::new("dot", 1).expect("request"))
        .await
        .expect("prepare")
        .start()
        .await
        .expect("start");
    while generation.next().await.is_some() {}
    assert_eq!(generation.finish().expect("image").images[0].bytes.len(), 4);

    let mut generation = registry
        .transcription(ModelRef::new("multimedia", "asr-v1").expect("model"))
        .expect("asr model")
        .prepare(TranscriptionRequest::new(audio()).expect("request"))
        .await
        .expect("prepare")
        .start()
        .await
        .expect("start");
    while generation.next().await.is_some() {}
    assert_eq!(generation.finish().expect("transcription").text, "hello");

    let mut generation = registry
        .speech(ModelRef::new("multimedia", "tts-v1").expect("model"))
        .expect("speech model")
        .prepare(SpeechRequest::new("hello", "alloy", SpeechFormat::Pcm16).expect("request"))
        .await
        .expect("prepare")
        .start()
        .await
        .expect("start");
    while generation.next().await.is_some() {}
    assert_eq!(generation.finish().expect("speech").audio.bytes, vec![0, 1]);

    assert_eq!(image.start_count(), 1);
    assert_eq!(transcription.start_count(), 1);
    assert_eq!(speech.start_count(), 1);
}

#[tokio::test]
async fn realtime_uses_a_separate_live_session_and_one_closed_terminal() {
    let realtime = ScriptedRealtimeAdapter::new(vec![
        RealtimeEvent::SessionStarted {
            session_id: "rt-1".to_owned(),
        },
        RealtimeEvent::OutputTextDelta {
            response_id: "response-1".to_owned(),
            text: "hello".to_owned(),
        },
        RealtimeEvent::Closed {
            reason: RealtimeCloseReason::Provider,
        },
    ]);
    let registry = registry(
        ScriptedImageAdapter::new(Vec::new()),
        ScriptedTranscriptionAdapter::new(Vec::new()),
        ScriptedSpeechAdapter::new(Vec::new()),
        realtime.clone(),
    );
    let mut session = registry
        .realtime(ModelRef::new("multimedia", "realtime-v1").expect("model"))
        .expect("realtime model")
        .connect(RealtimeRequest::new("alloy").expect("request"))
        .await
        .expect("connect");

    assert!(matches!(
        session.next_event().await.expect("event"),
        Some(RealtimeEvent::SessionStarted { .. })
    ));
    session
        .send(RealtimeCommand::AppendText {
            text: "continue".to_owned(),
        })
        .await
        .expect("command");
    assert!(matches!(
        session.next_event().await.expect("event"),
        Some(RealtimeEvent::OutputTextDelta { .. })
    ));
    assert!(matches!(
        session.next_event().await.expect("event"),
        Some(RealtimeEvent::Closed { .. })
    ));
    assert!(session.next_event().await.expect("EOF").is_none());
    assert_eq!(
        realtime.commands(),
        vec![RealtimeCommand::AppendText {
            text: "continue".to_owned(),
        }]
    );
}

#[tokio::test]
async fn interactive_realtime_script_waits_for_response_request() {
    let realtime = ScriptedRealtimeAdapter::new_after_request(vec![
        RealtimeEvent::SessionStarted {
            session_id: "rt-gated".to_owned(),
        },
        RealtimeEvent::OutputTextDelta {
            response_id: "response-1".to_owned(),
            text: "ready".to_owned(),
        },
        RealtimeEvent::Closed {
            reason: RealtimeCloseReason::Provider,
        },
    ]);
    let registry = registry(
        ScriptedImageAdapter::new(Vec::new()),
        ScriptedTranscriptionAdapter::new(Vec::new()),
        ScriptedSpeechAdapter::new(Vec::new()),
        realtime,
    );
    let mut session = registry
        .realtime(ModelRef::new("multimedia", "realtime-v1").expect("model"))
        .expect("realtime model")
        .connect(RealtimeRequest::new("alloy").expect("request"))
        .await
        .expect("connect");
    assert!(matches!(
        session.next_event().await.expect("started"),
        Some(RealtimeEvent::SessionStarted { .. })
    ));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), session.next_event())
            .await
            .is_err(),
        "scripted output must not race ahead of caller commands"
    );
    session
        .send(RealtimeCommand::RequestResponse)
        .await
        .expect("request response");
    assert!(matches!(
        session.next_event().await.expect("output"),
        Some(RealtimeEvent::OutputTextDelta { .. })
    ));
}

#[test]
fn invalid_wire_requests_cannot_reach_adapter_prepare() {
    let image = ScriptedImageAdapter::new(Vec::new());
    let realtime = ScriptedRealtimeAdapter::new(Vec::new());

    serde_json::from_value::<ImageRequest>(serde_json::json!({
        "prompt": "dot", "count": 0, "inputs": [], "mask": null
    }))
    .expect_err("invalid image wire must fail before a typed request exists");
    assert_eq!(image.prepare_count(), 0);

    serde_json::from_value::<RealtimeRequest>(serde_json::json!({
        "voice": "not a voice", "instructions": null,
        "input_format": "pcm16", "output_format": "pcm16"
    }))
    .expect_err("invalid Realtime wire must fail before a typed request exists");
    assert_eq!(realtime.prepare_count(), 0);
}
