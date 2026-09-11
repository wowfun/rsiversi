//! Ordinary presentation Profile, independent of terminal modes and Session ownership.
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_meta::{
    ActivationPlan, ConfigValue, ContractVersion, LocalContract, Message, MetaError, PluginFactory,
    PreparedActivation, Requirement,
};
use rsi_terminal_ui::{
    scene::Scene,
    wire::{self, Request},
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const PORTABLE_RENDER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// One complete display result; the resident writer owns presentation acknowledgement.
pub type Frame = (ratatui::buffer::Buffer, rsi_terminal_ui::render::View);
/// The renderer holds no terminal descriptor or Session controller.
pub trait FrameRenderer: std::fmt::Debug + Send + Sync {
    /// Materializes a complete replacement frame under one read cancellation token.
    fn render(
        &self,
        request: Request,
        scene: Vec<u8>,
        stop: CancellationToken,
    ) -> BoxFuture<'static, Result<Frame, String>>;
}
/// Local output of the independent terminal presentation Profile.
#[derive(Debug)]
pub struct FrameRendererContract;
impl LocalContract for FrameRendererContract {
    const KEY: &'static str = "rsi.terminal.presentation";
    type Service = dyn FrameRenderer;
}
/// Linked implementation using the same presentation library as the native module.
#[derive(Clone, Debug, Default)]
pub struct LinkedPresentationFactory;
#[derive(Debug)]
struct Linked(std::sync::Mutex<rsi_terminal_ui::scene::Renderer>);
impl FrameRenderer for Linked {
    fn render(
        &self,
        request: Request,
        scene: Vec<u8>,
        stop: CancellationToken,
    ) -> BoxFuture<'static, Result<Frame, String>> {
        let result = if stop.is_cancelled() {
            Err("presentation stopped".into())
        } else {
            request
                .validate()
                .and({
                    if scene.len() == request.bytes {
                        Ok(())
                    } else {
                        Err("scene source length mismatch")
                    }
                })
                .and_then(|()| Scene::decode(&scene))
                .and_then(|scene| {
                    self.0
                        .lock()
                        .map_err(|_| "linked presentation poisoned")?
                        .render(scene, request.width, request.height)
                })
                .map_err(str::to_owned)
        };
        Box::pin(async move {
            if stop.is_cancelled() {
                return Err("presentation stopped".into());
            }
            result
        })
    }
}
#[async_trait]
impl PluginFactory for LinkedPresentationFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(invalid("linked renderer configuration must be null"));
        }
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<FrameRendererContract>(Arc::new(
                Linked(std::sync::Mutex::default()),
            ))?;
        Ok(())
    }
}
/// Imports the explicitly required Portable terminal rendering service.
#[derive(Clone, Debug, Default)]
pub struct PortablePresentationFactory;
#[derive(Debug)]
struct Portable(rsi_meta::Capability);
impl FrameRenderer for Portable {
    fn render(
        &self,
        request: Request,
        scene: Vec<u8>,
        stop: CancellationToken,
    ) -> BoxFuture<'static, Result<Frame, String>> {
        let capability = self.0.clone();
        Box::pin(async move {
            let work = async {
                request.validate().map_err(str::to_owned)?;
                if scene.len() != request.bytes {
                    return Err("scene source length mismatch".into());
                }
                let mut call = capability.open().map_err(|e| e.to_string())?;
                call.send(Message::new(
                    wire::request_header(&request).map_err(str::to_owned)?,
                ))
                .await
                .map_err(|e| e.to_string())?;
                for chunk in scene.chunks(wire::MAXIMUM_FRAGMENT) {
                    call.send(Message::new(chunk.to_vec()))
                        .await
                        .map_err(|e| e.to_string())?;
                }
                call.finish();
                let mut bytes = Vec::new();
                while let Some(message) = call.recv().await.map_err(|e| e.to_string())? {
                    if !message.capabilities().is_empty()
                        || message.as_bytes().is_empty()
                        || message.as_bytes().len() > wire::MAXIMUM_FRAGMENT
                        || message.as_bytes().len() > wire::MAXIMUM_FRAME - bytes.len()
                    {
                        return Err("invalid terminal frame fragment".into());
                    }
                    bytes.extend_from_slice(message.as_bytes());
                }
                wire::decode(&bytes, &request).map_err(str::to_owned)
            };
            tokio::select! {
                biased;
                () = stop.cancelled() => Err("presentation stopped".into()),
                result = tokio::time::timeout(PORTABLE_RENDER_TIMEOUT, work) => {
                    result.unwrap_or_else(|_| Err(format!(
                        "terminal rendering exceeded {} seconds",
                        PORTABLE_RENDER_TIMEOUT.as_secs()
                    )))
                }
            }
        })
    }
}
#[async_trait]
impl PluginFactory for PortablePresentationFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(invalid("portable renderer configuration must be null"));
        }
        Ok(
            PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                wire::SERVICE,
                wire::CONTRACT,
                ContractVersion(wire::VERSION),
            )),
        )
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let capability = plan
            .inject(wire::SERVICE)
            .ok_or_else(|| invalid("missing renderer"))?
            .clone();
        plan.context()
            .provide_local::<FrameRendererContract>(Arc::new(Portable(capability)))?;
        Ok(())
    }
}
fn invalid(message: &str) -> MetaError {
    MetaError::InvalidInput(message.into())
}

pub(crate) struct Owner {
    profile: Arc<rsi_application::ScopedProfile>,
    execution: rsi_meta::Execution,
    closed: bool,
}
impl Owner {
    pub async fn start(
        parent: &rsi_meta::Context,
        presentation: Option<rsi_host::Profile>,
    ) -> crate::Result<Self> {
        use rsi_host::{HostBuilder, Profile, ProfileEntry, ProfileProgram};
        let profile = if let Some(presentation) = presentation {
            let source = parent
                .lookup_local::<rsi_application::ProfileCatalogContract>()
                .ok_or_else(|| {
                    crate::RsiError::Run(
                        "custom terminal presentation requires the application's catalog source"
                            .into(),
                    )
                })?;
            rsi_application::ScopedProfile::start_following(
                source,
                parent,
                ProfileProgram::from_profile(presentation),
            )
            .await
        } else {
            let mut builder = HostBuilder::without_paths(std::env::consts::OS);
            builder
                .register_local_contract::<FrameRendererContract>()
                .map_err(failure)?;
            builder
                .register_linked(
                    "rsi.terminal.ui",
                    env!("CARGO_PKG_VERSION"),
                    rsi_meta::UpdateMode::Replayable,
                    Arc::new(LinkedPresentationFactory),
                )
                .map_err(failure)?;
            rsi_application::ScopedProfile::start(
                &builder.build().map_err(failure)?,
                parent,
                ProfileProgram::from_profile(Profile::new(vec![ProfileEntry::new(
                    "renderer",
                    "rsi.terminal.ui",
                    ConfigValue::Null,
                )])),
            )
            .await
        }
        .map_err(failure)?;
        let owner = Self {
            profile: Arc::new(profile),
            execution: parent.runtime().execution().clone(),
            closed: false,
        };
        if owner.renderer().is_none() {
            return Err(failure("presentation Profile did not publish a renderer"));
        }
        Ok(owner)
    }
    pub fn renderer(&self) -> Option<Arc<dyn FrameRenderer>> {
        self.profile.lookup_local::<FrameRendererContract>()
    }
    pub fn render(
        &self,
        request: Request,
        scene: Vec<u8>,
        stop: CancellationToken,
    ) -> BoxFuture<'static, Result<Frame, String>> {
        match self.renderer() {
            Some(renderer) => renderer.render(request, scene, stop),
            None => Box::pin(async { Err("terminal presentation renderer unavailable".into()) }),
        }
    }
    pub fn changes(&self) -> tokio::sync::watch::Receiver<rsi_host::ProfileStatus> {
        self.profile.subscribe_profile()
    }
    pub async fn close(&mut self) -> crate::Result<()> {
        let report = self.profile.shutdown().await;
        self.closed = true;
        if report.is_clean() {
            Ok(())
        } else {
            Err(failure("terminal presentation cleanup failed"))
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        if !self.closed {
            let profile = self.profile.clone();
            drop(self.execution.spawn(async move {
                profile.shutdown().await;
            }));
        }
    }
}
fn failure(error: impl std::fmt::Display) -> crate::RsiError {
    crate::RsiError::Run(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> (Request, Vec<u8>) {
        scene_with(&rsi_terminal_ui::transcript::Transcript::default())
    }
    fn capture_scene(transcript: &rsi_terminal_ui::transcript::Transcript) -> Scene {
        use rsi_agent_session_protocol::{
            AgentPresetId, FrozenAgentSettings, SessionHeader, SessionId,
        };
        let header = SessionHeader::new(
            SessionId::new("presentation").unwrap(),
            1,
            "/workspace",
            AgentPresetId::new("default").unwrap(),
            FrozenAgentSettings::new(
                "default",
                "system",
                rsi_ai_protocol::ModelRef::new("fixture", "text").unwrap(),
                rsi_sandbox::SandboxMode::WorkspaceWrite,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        Scene::capture(
            &rsi_terminal_ui::Input {
                header: &header,
                transcript,
                editor: &rsi_terminal_ui::editor::Editor::with_text("retained draft".into(), 1024),
                model: None,
                enter_submit: false,
                menu: None,
                answer: None,
                ui_edit: None,
                detail: None,
                detail_offset: 0,
                selection: None,
                top: None,
                status: "Ready",
                actual_model: None,
                active: false,
                busy: false,
                remote: false,
                questions: 0,
                approvals: 0,
            },
            24,
        )
        .unwrap()
    }
    fn scene_with(transcript: &rsi_terminal_ui::transcript::Transcript) -> (Request, Vec<u8>) {
        let source = capture_scene(transcript).encode().unwrap();
        (
            Request {
                identity: wire::Identity {
                    attachment: 1,
                    presentation: 1,
                    revision: 1,
                },
                width: 80,
                height: 24,
                bytes: source.len(),
            },
            source,
        )
    }

    #[tokio::test]
    #[ignore = "diagnostic linked capture/JSON/render benchmark; no timing threshold"]
    async fn linked_visible_window_capture_and_render_cost() {
        use rsi_agent_session_protocol::{SessionFact, SessionFactBody, TurnId};
        let mut transcript = rsi_terminal_ui::transcript::Transcript::default();
        for seq in 1..=2 {
            transcript.apply(
                &SessionFact::new(
                    seq,
                    seq,
                    SessionFactBody::TurnAccepted {
                        turn_id: TurnId::new(format!("turn-{seq}")).unwrap(),
                        text: "word ".repeat(50_000),
                        model: None,
                        sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap(),
            );
        }
        let mut renderer = rsi_terminal_ui::scene::Renderer::default();
        let mut samples = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        let mut scene_bytes = 0;
        for _ in 0..100 {
            let started = std::time::Instant::now();
            let scene = capture_scene(&transcript);
            samples[0].push(started.elapsed());
            let started = std::time::Instant::now();
            let source = scene.encode().unwrap();
            samples[1].push(started.elapsed());
            scene_bytes = source.len();
            let started = std::time::Instant::now();
            let decoded = Scene::decode(&source).unwrap();
            samples[2].push(started.elapsed());
            let started = std::time::Instant::now();
            std::hint::black_box(renderer.render(decoded, 80, 24).unwrap());
            samples[3].push(started.elapsed());
        }
        for (phase, mut values) in ["capture", "encode", "decode", "render"]
            .into_iter()
            .zip(samples)
        {
            values.sort_unstable();
            eprintln!(
                "terminal phase={phase} frames=100 scene_bytes={scene_bytes} optimized={} p50={:?} p95={:?} p99={:?}",
                !cfg!(debug_assertions),
                values[50],
                values[95],
                values[99]
            );
        }
    }
    #[tokio::test]
    async fn poisoned_linked_renderer_returns_a_recoverable_failure() {
        let renderer = Arc::new(Linked(std::sync::Mutex::default()));
        let poison = renderer.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = poison.0.lock().unwrap();
                panic!("fixture rendering panicked");
            })
            .join()
            .is_err()
        );
        let (request, source) = scene();
        assert_eq!(
            renderer
                .render(request, source, CancellationToken::new())
                .await
                .unwrap_err(),
            "linked presentation poisoned"
        );
    }

    #[tokio::test]
    async fn withdrawn_presentation_returns_a_failure_then_recovers_with_the_same_scene() {
        use rsi_host::{HostBuilder, Profile, ProfileEntry, ProfileProgram};
        let runtime = rsi_meta::Runtime::default();
        let mut owner = Owner::start(&runtime.root(), None).await.unwrap();
        let mut builder = HostBuilder::without_paths(std::env::consts::OS);
        builder
            .register_local_contract::<FrameRendererContract>()
            .unwrap();
        builder
            .register_linked(
                "rsi.terminal.ui",
                env!("CARGO_PKG_VERSION"),
                rsi_meta::UpdateMode::Replayable,
                Arc::new(LinkedPresentationFactory),
            )
            .unwrap();
        let host = builder.build().unwrap();
        let updater = owner.profile.updater();
        updater
            .submit(
                updater.input_revision(),
                host.profile_input(ProfileProgram::from_profile(Profile::default()))
                    .unwrap(),
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
        let (request, source) = scene();
        assert_eq!(
            owner
                .render(request, source.clone(), CancellationToken::new())
                .await
                .unwrap_err(),
            "terminal presentation renderer unavailable"
        );
        updater
            .submit(
                updater.input_revision(),
                host.profile_input(ProfileProgram::from_profile(Profile::new(vec![
                    ProfileEntry::new("renderer", "rsi.terminal.ui", ConfigValue::Null),
                ])))
                .unwrap(),
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
        let (request, _) = scene();
        let (frame, _) = owner
            .render(request, source, CancellationToken::new())
            .await
            .unwrap();
        let text: String = frame
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("retained draft"));
        owner.close().await.unwrap();
        assert!(runtime.shutdown().await.is_clean());
    }
}
