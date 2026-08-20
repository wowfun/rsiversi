use std::fs;
use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "../../observer_support.rs"]
mod observer_support;

use rsi_agent::{
    AgentError, AgentHost, AgentRealtimeEvent, AgentWorkspace, AiOperationId,
    OpenOptions as AgentOpenOptions, RunRequest, RunStatus, SessionId, ToolOutcome, Transcript,
    TranscriptEventKind,
};
use rsi_ai_protocol::{
    ImageRequest, MediaKind, RealtimeRequest, SpeechFormat, SpeechRequest, TranscriptionRequest,
};
use rsi_meta::{
    ApplyRequest, ApplyResult, CompositionHost, CompositionProject, CompositionWorkspace,
    InstanceId, LockResult, OpenOptions as CompositionOpenOptions, OperationId, ServiceKey,
    ServiceOpenRequest, ServiceStream, StreamKind,
};
use rsi_meta_loader::BUILD_TARGET;

pub const ECHO_PROMPT: &str = "Use the echo tool to repeat: hello";
pub const DIRECT_PROMPT: &str = "Answer directly with: ready";
pub const ECHO_SESSION_ID: &str = "keyless-echo";
pub const DIRECT_SESSION_ID: &str = "keyless-direct";

const TOOLS_OBSERVER_SERVICE: &str = "fixture.rsi-agent.tools-observer";
const OBSERVER_DEADLINE: Duration = Duration::from_secs(2);
const OBSERVER_OUTPUT_CREDIT: u64 = 64 * 1024;
const CONCURRENT_RUN_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PrimaryServiceObservation {
    open_attempts: u64,
    accepted_opens: u64,
    data_frames: u64,
    max_concurrent_streams: u64,
}

const QUIET_SERVICE: PrimaryServiceObservation = PrimaryServiceObservation {
    open_attempts: 0,
    accepted_opens: 0,
    data_frames: 0,
    max_concurrent_streams: 0,
};
const EXPECTED_TOOLS_ACTIVITY: PrimaryServiceObservation = PrimaryServiceObservation {
    open_attempts: 2,
    accepted_opens: 2,
    data_frames: 3,
    max_concurrent_streams: 2,
};

#[derive(Clone, Copy, Debug)]
struct NativeFixture {
    package: &'static str,
    library_stem: &'static str,
}

const NATIVE_FIXTURES: &[NativeFixture] = &[
    NativeFixture {
        package: "fixtures/rsi-agent/scripted-model",
        library_stem: "rsi_agent_fixture_scripted_model",
    },
    NativeFixture {
        package: "fixtures/rsi-agent/echo-tools",
        library_stem: "rsi_agent_fixture_echo_tools",
    },
    NativeFixture {
        package: "fixtures/rsi-agent/capability-anchor",
        library_stem: "rsi_agent_fixture_capability_anchor",
    },
];

/// Resolves the repository containing this fixture package.
///
/// # Panics
///
/// Panics if the checked-in fixture is moved outside its product namespace.
pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("conformance package is nested three levels below repository")
        .to_owned()
}

/// Returns the checked-in keyless composition source.
pub fn composition_manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("composition")
        .join("rsi-meta.toml")
}

/// Stages every prebuilt native fixture inside its package boundary.
///
/// # Errors
///
/// Returns an error when an expected current-target `cdylib` is absent or
/// cannot be copied into the package-owned ignored target directory.
pub fn stage_native_fixtures() -> Result<(), String> {
    for fixture in NATIVE_FIXTURES {
        stage_native_fixture(*fixture)?;
    }
    Ok(())
}

/// Runs the assembled concurrent direct-final and model-to-tool-to-model scenario.
///
/// # Errors
///
/// Returns artifact staging, composition, persistence, protocol, or shutdown failures.
///
/// # Panics
///
/// Panics when a successful component violates the fixed conformance
/// transcript or replay expectations.
#[allow(clippy::too_many_lines)] // Keep the ordered black-box scenario visible.
pub async fn run_conformance() -> Result<(), Box<dyn std::error::Error>> {
    stage_native_fixtures().map_err(std::io::Error::other)?;
    let temporary = tempfile::tempdir()?;
    let composition_root = temporary.path().join("composition-host");
    let lock_path = temporary.path().join("candidate.lock");
    let project = CompositionProject {
        manifest_path: composition_manifest(),
        lock_path: Some(lock_path),
    };
    let lock_result = project.lock()?;
    assert!(
        matches!(
            lock_result,
            LockResult::Created { .. } | LockResult::Unchanged { .. }
        ),
        "keyless composition must lock cleanly: {lock_result:?}"
    );

    let composition = CompositionHost::open(CompositionOpenOptions::new(CompositionWorkspace {
        database_path: composition_root.join("state.sqlite3"),
        cache_root: composition_root.join("cache"),
        manifest_path: composition_root.join("installed.toml"),
        lock_path: composition_root.join("installed.lock"),
    }))
    .await?;
    let apply = composition
        .apply(ApplyRequest {
            operation_id: OperationId("rsi-agent-keyless-conformance".to_owned()),
            project,
            expected_revision: None,
        })
        .await?;
    assert!(
        matches!(apply, ApplyResult::Applied { .. }),
        "keyless composition must apply cleanly: {apply:?}"
    );

    let mut tools_observer =
        open_observer(&composition, TOOLS_OBSERVER_SERVICE, "echo-tools").await;
    assert_eq!(observe_primary(&mut tools_observer).await, QUIET_SERVICE);

    let workspace = AgentWorkspace::new(temporary.path().join("agent-workspace"));
    let echo_session = SessionId::new(ECHO_SESSION_ID)?;
    let direct_session = SessionId::new(DIRECT_SESSION_ID)?;
    let (echo_record, echo_transcript, direct_record, direct_transcript) = {
        let agent = AgentHost::open(
            AgentOpenOptions::new(
                workspace.clone(),
                composition.clone(),
                InstanceId::new("agent-capability-anchor"),
            )
            .with_max_concurrent_runs(NonZeroU8::new(2).expect("two is nonzero")),
        )
        .await?;
        let echo_agent = agent.clone();
        let direct_agent = agent.clone();
        let echo_request = RunRequest::new(echo_session.clone(), ECHO_PROMPT)?;
        let direct_request = RunRequest::new(direct_session.clone(), DIRECT_PROMPT)?;
        let (echo, direct) = tokio::time::timeout(CONCURRENT_RUN_DEADLINE, async move {
            tokio::join!(
                echo_agent.run(echo_request),
                direct_agent.run(direct_request),
            )
        })
        .await
        .expect("the two first model requests must meet at the provider barrier");
        let echo = echo?;
        let direct = direct?;
        assert_eq!(
            echo.status(),
            &RunStatus::Completed {
                final_message: "hello".to_owned()
            }
        );
        assert_eq!(
            direct.status(),
            &RunStatus::Completed {
                final_message: "ready".to_owned()
            }
        );
        let echo_transcript = agent
            .transcript(&echo_session)
            .await?
            .expect("completed echo run has a transcript");
        let direct_transcript = agent
            .transcript(&direct_session)
            .await?
            .expect("completed direct run has a transcript");
        assert_echo_vertical_slice(&echo_transcript);
        assert_direct_vertical_slice(&direct_transcript);
        assert_five_capability_slice(&agent).await?;
        assert_expected_primary_activity(&mut tools_observer, "concurrent runs").await;

        let echo_replay_agent = agent.clone();
        let direct_replay_agent = agent.clone();
        let (echo_repeated, direct_repeated) = tokio::join!(
            echo_replay_agent.run(RunRequest::new(echo_session.clone(), ECHO_PROMPT)?),
            direct_replay_agent.run(RunRequest::new(direct_session.clone(), DIRECT_PROMPT)?),
        );
        assert_eq!(
            echo_repeated?, echo,
            "same-host echo retry must replay its record"
        );
        assert_eq!(
            direct_repeated?, direct,
            "same-host direct retry must replay its record"
        );
        assert_eq!(
            agent.transcript(&echo_session).await?,
            Some(echo_transcript.clone()),
            "same-host echo retry must not mutate the transcript"
        );
        assert_eq!(
            agent.transcript(&direct_session).await?,
            Some(direct_transcript.clone()),
            "same-host direct retry must not mutate the transcript"
        );
        assert_expected_primary_activity(&mut tools_observer, "same-host replays").await;
        (echo, echo_transcript, direct, direct_transcript)
    };

    let reopen_options = AgentOpenOptions::new(
        workspace,
        composition.clone(),
        InstanceId::new("agent-capability-anchor"),
    )
    .with_max_concurrent_runs(NonZeroU8::new(2).expect("two is nonzero"));
    let reopened = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match AgentHost::open(reopen_options.clone()).await {
                Err(AgentError::WorkspaceOccupied { .. }) => tokio::task::yield_now().await,
                result => return result,
            }
        }
    })
    .await??;
    assert_expected_primary_activity(&mut tools_observer, "agent host reopen").await;
    assert_eq!(
        reopened.transcript(&echo_session).await?,
        Some(echo_transcript.clone()),
        "reopening the store must reproduce the echo transcript"
    );
    assert_eq!(
        reopened.transcript(&direct_session).await?,
        Some(direct_transcript.clone()),
        "reopening the store must reproduce the direct transcript"
    );
    let echo_replay_agent = reopened.clone();
    let direct_replay_agent = reopened.clone();
    let (echo_replayed, direct_replayed) = tokio::join!(
        echo_replay_agent.run(RunRequest::new(echo_session.clone(), ECHO_PROMPT)?),
        direct_replay_agent.run(RunRequest::new(direct_session.clone(), DIRECT_PROMPT)?),
    );
    assert_eq!(
        echo_replayed?, echo_record,
        "reopened echo replay must return the same durable record"
    );
    assert_eq!(
        direct_replayed?, direct_record,
        "reopened direct replay must return the same durable record"
    );
    assert_eq!(
        reopened.transcript(&echo_session).await?,
        Some(echo_transcript),
        "reopened echo replay must not append events"
    );
    assert_eq!(
        reopened.transcript(&direct_session).await?,
        Some(direct_transcript),
        "reopened direct replay must not append events"
    );
    assert_expected_primary_activity(&mut tools_observer, "reopened replays").await;
    assert_composition_healthy(&composition);
    drop(reopened);
    close_observer(&mut tools_observer).await;

    composition
        .request_shutdown(OperationId("rsi-agent-conformance-shutdown".to_owned()))
        .await?;
    composition
        .wait_terminated(Instant::now() + Duration::from_secs(5))
        .await?;
    Ok(())
}

async fn assert_five_capability_slice(agent: &AgentHost) -> Result<(), Box<dyn std::error::Error>> {
    let image = agent
        .generate_image(
            AiOperationId::new("fixture-image")?,
            "fixture-model",
            ImageRequest::new("draw a fixture", 1)?,
        )
        .await?;
    assert_eq!(image.images.len(), 1);
    assert_eq!(
        agent.artifacts().read(&image.images[0]).await?,
        b"fixture-image"
    );

    let input_audio = agent
        .import_artifact(MediaKind::Audio, "audio/wav", b"fixture-input".to_vec())
        .await?;
    let transcription = agent
        .transcribe(
            AiOperationId::new("fixture-transcription")?,
            "fixture-model",
            TranscriptionRequest::new(input_audio.descriptor().clone())?,
        )
        .await?;
    assert_eq!(transcription.transcription.text, "fixture transcript");

    let speech = agent
        .synthesize(
            AiOperationId::new("fixture-speech")?,
            "fixture-model",
            SpeechRequest::new("speak", "fixture", SpeechFormat::Wav)?,
        )
        .await?;
    assert_eq!(
        agent.artifacts().read(&speech.audio).await?,
        b"fixture-speech"
    );

    let mut realtime = agent
        .open_realtime(
            AiOperationId::new("fixture-realtime")?,
            "fixture-model",
            RealtimeRequest::new("fixture")?,
        )
        .await?;
    assert_eq!(realtime.session_id(), "fixture-realtime");
    realtime.append_audio(1, &input_audio).await?;
    realtime.append_text("hello live session").await?;
    realtime.commit_input("input-1").await?;
    realtime.request_response().await?;
    assert!(matches!(
        realtime.next_event().await?,
        Some(AgentRealtimeEvent::OutputTextDelta { text, .. }) if text == "live"
    ));
    let audio = match realtime.next_event().await? {
        Some(AgentRealtimeEvent::OutputAudio { artifact, .. }) => artifact,
        other => panic!("expected Realtime audio, got {other:?}"),
    };
    assert_eq!(agent.artifacts().read(&audio).await?, b"fixture-live-audio");
    assert!(matches!(
        realtime.next_event().await?,
        Some(AgentRealtimeEvent::Closed { .. })
    ));
    realtime.close().await?;
    Ok(())
}

async fn open_observer(
    composition: &CompositionHost,
    service: &str,
    expected_provider: &str,
) -> ServiceStream {
    let mut stream = composition
        .open_service(ServiceOpenRequest {
            consumer: InstanceId::new("agent-capability-anchor"),
            service: ServiceKey::new(service),
        })
        .unwrap_or_else(|error| panic!("open {service} observer: {error}"));
    assert_eq!(stream.provider(), &InstanceId::new(expected_provider));
    let credit = recv_observer_frame(&mut stream).await;
    assert_eq!(credit.kind, StreamKind::Credit);
    stream
        .grant_credit(OBSERVER_OUTPUT_CREDIT)
        .await
        .unwrap_or_else(|error| panic!("grant {service} observer output credit: {error}"));
    stream
}

async fn observe_primary(stream: &mut ServiceStream) -> PrimaryServiceObservation {
    stream
        .send(observer_support::QUERY)
        .await
        .expect("send primary-service observer query");
    let frame = recv_observer_frame(stream).await;
    assert_eq!(frame.kind, StreamKind::Data);
    let bytes = frame.data.expect("observer DATA contains bytes");
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).expect("observer DATA is valid JSON");
    assert_eq!(
        serde_json::to_vec(&value).expect("re-encode observer DATA"),
        bytes,
        "observer DATA must be canonical"
    );
    let object = value.as_object().expect("observer DATA is an object");
    assert_eq!(object.len(), 6, "observer snapshot is a closed contract");
    assert_eq!(
        object.get("kind").and_then(serde_json::Value::as_str),
        Some("snapshot")
    );
    assert_eq!(
        object.get("version").and_then(serde_json::Value::as_u64),
        Some(0)
    );
    let observation = PrimaryServiceObservation {
        open_attempts: object
            .get("open_attempts")
            .and_then(serde_json::Value::as_u64)
            .expect("observer open_attempts is a u64"),
        accepted_opens: object
            .get("accepted_opens")
            .and_then(serde_json::Value::as_u64)
            .expect("observer accepted_opens is a u64"),
        data_frames: object
            .get("data_frames")
            .and_then(serde_json::Value::as_u64)
            .expect("observer data_frames is a u64"),
        max_concurrent_streams: object
            .get("max_concurrent_streams")
            .and_then(serde_json::Value::as_u64)
            .expect("observer max_concurrent_streams is a u64"),
    };
    assert_eq!(
        observer_support::snapshot(
            observation.open_attempts,
            observation.accepted_opens,
            observation.data_frames,
            observation.max_concurrent_streams,
        ),
        bytes,
        "observer snapshot must use the fixture-owned canonical encoding"
    );
    observation
}

async fn assert_expected_primary_activity(tools_observer: &mut ServiceStream, stage: &str) {
    assert_eq!(
        observe_primary(tools_observer).await,
        EXPECTED_TOOLS_ACTIVITY,
        "{stage} must not add a tools service open or DATA frame"
    );
}

async fn close_observer(stream: &mut ServiceStream) {
    stream.half_close().await.expect("half-close observer");
    assert_eq!(recv_observer_frame(stream).await.kind, StreamKind::End);
    let closed = tokio::time::timeout(OBSERVER_DEADLINE, stream.recv())
        .await
        .expect("observer close deadline");
    assert!(closed.is_none(), "observer closes after END");
}

async fn recv_observer_frame(stream: &mut ServiceStream) -> rsi_meta::StreamEnvelope {
    tokio::time::timeout(OBSERVER_DEADLINE, stream.recv())
        .await
        .expect("observer frame deadline")
        .expect("observer stream remains open")
        .expect("observer frame is valid")
}

fn assert_composition_healthy(composition: &CompositionHost) {
    let snapshot = composition.snapshot();
    for id in ["scripted-model", "echo-tools", "agent-capability-anchor"] {
        let instance = snapshot
            .graph
            .instances
            .get(&InstanceId::new(id))
            .unwrap_or_else(|| panic!("composition lost {id}"));
        assert!(
            instance.status.is_active(),
            "replay must not fault {id}: {:?}",
            instance.status
        );
    }
}

fn assert_echo_vertical_slice(transcript: &Transcript) {
    assert_eq!(
        transcript.status(),
        &RunStatus::Completed {
            final_message: "hello".to_owned()
        }
    );
    assert_eq!(
        transcript
            .events()
            .iter()
            .filter(|event| matches!(
                event.kind(),
                TranscriptEventKind::ModelRequestPrepared { .. }
            ))
            .count(),
        2,
        "vertical slice must make two committed model requests"
    );
    assert_eq!(
        transcript
            .events()
            .iter()
            .filter(|event| matches!(
                event.kind(),
                TranscriptEventKind::ToolDispatchStarted { .. }
            ))
            .count(),
        1,
        "vertical slice must dispatch echo once"
    );
    let contexts = transcript
        .events()
        .iter()
        .filter_map(|event| match event.kind() {
            TranscriptEventKind::ContextSnapshot { context } => Some(context),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(contexts.len(), 1, "context must be captured exactly once");
    let context = contexts[0];
    assert_eq!(context.system_prompt, rsi_agent::SYSTEM_PROMPT);
    assert_eq!(context.model_provider, "scripted-model");
    assert_eq!(context.tools_provider, "echo-tools");
    assert_eq!(context.model_protocol_version, 0);
    assert_eq!(context.tools_protocol_version, 0);
    assert_eq!(context.tools.len(), 1);
    assert_eq!(context.tools[0].name, "echo");

    let results = transcript
        .events()
        .iter()
        .filter_map(|event| match event.kind() {
            TranscriptEventKind::ToolResult { outcome, .. } => Some(outcome),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 1, "echo must have one terminal result");
    assert!(matches!(
        results[0],
        ToolOutcome::Succeeded { value } if value.get("text").and_then(serde_json::Value::as_str) == Some("hello")
    ));
}

fn assert_direct_vertical_slice(transcript: &Transcript) {
    assert_eq!(
        transcript.status(),
        &RunStatus::Completed {
            final_message: "ready".to_owned()
        }
    );
    assert_eq!(
        transcript
            .events()
            .iter()
            .filter(|event| matches!(
                event.kind(),
                TranscriptEventKind::ModelRequestPrepared { .. }
            ))
            .count(),
        1,
        "direct slice must make one committed model request"
    );
    assert!(
        transcript.events().iter().all(|event| !matches!(
            event.kind(),
            TranscriptEventKind::ToolDispatchStarted { .. }
        )),
        "direct slice must not dispatch a tool"
    );
}

fn native_library_name(fixture: NativeFixture) -> String {
    format!(
        "{}{}{}",
        std::env::consts::DLL_PREFIX,
        fixture.library_stem,
        std::env::consts::DLL_SUFFIX
    )
}

fn built_native_fixture(fixture: NativeFixture) -> PathBuf {
    repository()
        .join("fixtures/rsi-agent/target")
        .join(BUILD_TARGET)
        .join("release")
        .join(native_library_name(fixture))
}

fn package_native_fixture(fixture: NativeFixture) -> PathBuf {
    repository()
        .join(fixture.package)
        .join("target/native")
        .join(BUILD_TARGET)
        .join(native_library_name(fixture))
}

fn stage_native_fixture(fixture: NativeFixture) -> Result<(), String> {
    let source = built_native_fixture(fixture);
    if !source.is_file() {
        return Err(format!(
            "native fixture artifact missing: {}",
            source.display()
        ));
    }
    let destination = package_native_fixture(fixture);
    let parent = destination
        .parent()
        .expect("native fixture destination has a parent");
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "could not create native fixture directory `{}`: {error}",
            parent.display()
        )
    })?;
    fs::copy(&source, &destination).map_err(|error| {
        format!(
            "could not stage native fixture `{}` as `{}`: {error}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn composition_binds_both_services_to_the_capability_anchor() {
        let source = fs::read_to_string(composition_manifest()).expect("composition source");
        assert!(source.contains("id = \"agent-capability-anchor\""));
        assert!(source.contains("\"rsi.ai.language\" = \"scripted-model\""));
        for service in ["image", "transcription", "speech", "realtime"] {
            assert!(source.contains(&format!("\"rsi.ai.{service}\" = \"scripted-model\"")));
        }
        assert!(source.contains("\"rsi.agent.tools\" = \"echo-tools\""));
        assert!(source.contains("\"fixture.rsi-agent.tools-observer\" = \"echo-tools\""));
        for fixture in NATIVE_FIXTURES {
            assert!(
                repository()
                    .join(fixture.package)
                    .join("plugin.toml")
                    .is_file()
            );
        }
        assert!(repository().join("fixtures/rsi-agent/Cargo.lock").is_file());
    }

    #[test]
    fn native_fixture_lookup_uses_the_workspace_target_directory() {
        for fixture in NATIVE_FIXTURES {
            assert!(
                built_native_fixture(*fixture).starts_with(
                    repository()
                        .join("fixtures/rsi-agent/target")
                        .join(BUILD_TARGET)
                )
            );
            assert!(
                package_native_fixture(*fixture)
                    .starts_with(repository().join(fixture.package).join("target/native"))
            );
        }
    }
}
