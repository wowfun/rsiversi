//! Ordinary presentation Profile, independent of terminal modes and Session ownership.
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_meta::{
    ActivationPlan, ConfigValue, ContractVersion, LocalContract, Message, MetaError, PluginFactory,
    PreparedActivation, Requirement,
};
use rsi_terminal_ui::{scene::Scene, wire};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const PORTABLE_RENDER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// One complete display result; the resident writer owns presentation acknowledgement.
pub type Frame = (ratatui::buffer::Buffer, rsi_terminal_ui::render::View);
/// Process-local frame identity and viewport; encoded lengths belong to Portable.
#[derive(Clone, Copy, Debug)]
pub struct FrameRequest {
    /// Attachment, presentation generation and monotonic frame revision.
    /// Attachment zero identifies an unattached application scene; presentation
    /// and revision must be nonzero.
    pub identity: wire::Identity,
    /// Viewport columns, checked by the renderer.
    pub width: u16,
    /// Viewport rows, checked by the renderer.
    pub height: u16,
}
impl FrameRequest {
    fn validate(self) -> Result<(), &'static str> {
        wire::dimensions(self.width, self.height)?;
        if self.identity.presentation == 0 || self.identity.revision == 0 {
            return Err("invalid render request");
        }
        Ok(())
    }
}
/// The renderer holds no terminal descriptor or Session controller.
pub trait FrameRenderer: std::fmt::Debug + Send + Sync {
    /// Materializes a complete replacement frame under one read cancellation token.
    fn render(
        &self,
        request: FrameRequest,
        scene: Scene,
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
struct Linked(Arc<std::sync::Mutex<rsi_terminal_ui::scene::Renderer>>);
impl FrameRenderer for Linked {
    fn render(
        &self,
        request: FrameRequest,
        scene: Scene,
        stop: CancellationToken,
    ) -> BoxFuture<'static, Result<Frame, String>> {
        let renderer = self.0.clone();
        Box::pin(async move {
            request.validate().map_err(str::to_owned)?;
            if stop.is_cancelled() {
                return Err("presentation stopped".into());
            }
            let worker_stop = stop.clone();
            let result = tokio::task::spawn_blocking(move || {
                if worker_stop.is_cancelled() {
                    return Err("presentation stopped".into());
                }
                (|| {
                    let mut renderer = renderer
                        .lock()
                        .map_err(|_| "linked presentation poisoned")?;
                    if worker_stop.is_cancelled() {
                        return Err("presentation stopped");
                    }
                    renderer.render(scene, request.width, request.height)
                })()
                .map_err(str::to_owned)
            })
            .await
            .map_err(|error| format!("presentation worker failed: {error}"))?;
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
            .provide_local::<FrameRendererContract>(Arc::new(Linked(Arc::default())))?;
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
        request: FrameRequest,
        scene: Scene,
        stop: CancellationToken,
    ) -> BoxFuture<'static, Result<Frame, String>> {
        let capability = self.0.clone();
        Box::pin(async move {
            request.validate().map_err(str::to_owned)?;
            if stop.is_cancelled() {
                return Err("presentation stopped".into());
            }
            let work = async {
                let scene = tokio_util::task::AbortOnDropHandle::new(tokio::task::spawn_blocking(
                    move || scene.encode(),
                ))
                .await
                .map_err(|error| format!("scene worker failed: {error}"))?
                .map_err(str::to_owned)?;
                let request = wire::Request {
                    identity: request.identity,
                    width: request.width,
                    height: request.height,
                    bytes: scene.len(),
                };
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
        request: FrameRequest,
        scene: Scene,
        stop: CancellationToken,
    ) -> BoxFuture<'static, (FrameRequest, Result<Frame, String>)> {
        let renderer = self.renderer();
        Box::pin(async move {
            let result = async {
                let renderer = renderer.ok_or("terminal presentation renderer unavailable")?;
                if stop.is_cancelled() {
                    return Err("presentation stopped".into());
                }
                renderer.render(request, scene, stop).await
            }
            .await;
            (request, result)
        })
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
    use rsi_host::{HostBuilder, Profile, ProfileEntry, ProfileProgram};

    #[derive(Debug)]
    struct UnusedEndpoint;
    #[async_trait]
    impl rsi_meta::ServiceEndpoint for UnusedEndpoint {
        async fn serve(
            &self,
            _: rsi_meta::InvocationContext,
            _: rsi_meta::ProviderChannel<'_>,
        ) -> rsi_meta::Result<()> {
            panic!("queued encoding must time out before opening the service")
        }
    }
    #[async_trait]
    impl PluginFactory for UnusedEndpoint {
        fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone()))
        }
        async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
            plan.context().provide(
                wire::SERVICE,
                wire::CONTRACT,
                ContractVersion(wire::VERSION),
                Arc::new(Self),
            )?;
            Ok(())
        }
    }

    #[test]
    fn portable_deadline_and_cancellation_cover_queued_encoding() {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        executor.block_on(async {
            let runtime = rsi_meta::Runtime::default();
            let mut builder = HostBuilder::without_paths(std::env::consts::OS);
            builder
                .register_local_contract::<FrameRendererContract>()
                .unwrap();
            builder
                .register_linked(
                    "test.renderer",
                    "1",
                    rsi_meta::UpdateMode::Replayable,
                    Arc::new(UnusedEndpoint),
                )
                .unwrap();
            builder
                .register_linked(
                    "test.adapter",
                    "1",
                    rsi_meta::UpdateMode::Replayable,
                    Arc::new(PortablePresentationFactory),
                )
                .unwrap();
            let profile = rsi_application::ScopedProfile::start(
                &builder.build().unwrap(),
                &runtime.root(),
                ProfileProgram::from_profile(Profile::new([
                    ProfileEntry::new("renderer", "test.renderer", ConfigValue::Null),
                    ProfileEntry::new("adapter", "test.adapter", ConfigValue::Null),
                ])),
            )
            .await
            .unwrap();
            let renderer = profile.lookup_local::<FrameRendererContract>().unwrap();
            for cancelled in [false, true] {
                let (release, wait) = std::sync::mpsc::channel();
                let (entered, ready) = tokio::sync::oneshot::channel();
                let blocked = tokio::task::spawn_blocking(move || {
                    entered.send(()).unwrap();
                    let _ = wait.recv();
                });
                ready.await.unwrap();
                let stop = CancellationToken::new();
                let (request, source) = scene();
                let mut render = renderer.render(request, source, stop.clone());
                assert!(futures_util::poll!(&mut render).is_pending());
                if cancelled {
                    stop.cancel();
                }
                let result = tokio::time::timeout(std::time::Duration::from_secs(3), render).await;
                // Release even on regression, so executor shutdown cannot hang.
                release.send(()).unwrap();
                blocked.await.unwrap();
                assert_eq!(
                    result
                        .expect("encoding escaped the render deadline")
                        .unwrap_err(),
                    if cancelled {
                        "presentation stopped"
                    } else {
                        "terminal rendering exceeded 2 seconds"
                    }
                );
            }
            assert_eq!(runtime.resource_snapshot().service_calls.current, 0);
            drop(renderer);
            assert!(profile.shutdown().await.is_clean());
            assert!(runtime.shutdown().await.is_clean());
        });
    }

    #[tokio::test]
    async fn linked_render_creation_does_not_wait_for_the_renderer() {
        let renderer = Arc::new(Linked(Arc::default()));
        let held_renderer = renderer.clone();
        let (held, ready) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _guard = held_renderer.0.lock().unwrap();
            held.send(()).unwrap();
            let _ = released.recv();
        });
        ready.recv().unwrap();
        let (sent, received) = tokio::sync::oneshot::channel();
        let other = renderer.clone();
        let (request, source) = scene();
        let worker = std::thread::spawn(move || {
            let future = other.render(request, source, CancellationToken::new());
            let _ = sent.send(future);
        });
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), received).await;
        if result.is_err() {
            release.send(()).unwrap();
            holder.join().unwrap();
            worker.join().unwrap();
            panic!("render creation blocked on CPU work");
        }
        worker.join().unwrap();
        let mut future = result.unwrap().unwrap();
        assert!(futures_util::poll!(&mut future).is_pending());
        release.send(()).unwrap();
        holder.join().unwrap();
        assert!(future.await.is_ok());
    }

    fn scene() -> (FrameRequest, Scene) {
        scene_with(&rsi_terminal_ui::transcript::Transcript::default())
    }

    #[tokio::test]
    async fn typed_linked_scene_matches_portable_cells_and_source_map() {
        let (request, source) = scene();
        let decoded = Scene::decode(&source.encode().unwrap()).unwrap();
        let expected = decoded.render(request.width, request.height).unwrap();
        let actual = Linked(Arc::default())
            .render(request, source, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            wire::encode(request.identity, &actual.0, &actual.1).unwrap(),
            wire::encode(request.identity, &expected.0, &expected.1).unwrap()
        );
    }

    #[tokio::test]
    async fn unattached_application_frame_accepts_zero_attachment() {
        let request = FrameRequest {
            identity: wire::Identity {
                attachment: 0,
                presentation: 1,
                revision: 1,
            },
            width: 80,
            height: 24,
        };
        let source = Scene::from(rsi_terminal_ui::scene::ApplicationScene {
            title: "No session attached".into(),
            ..Default::default()
        });
        let encoded = source.encode().unwrap();
        let wire_request = wire::Request {
            identity: request.identity,
            width: request.width,
            height: request.height,
            bytes: encoded.len(),
        };
        wire::parse_header(&wire::request_header(&wire_request).unwrap()).unwrap();
        let (cells, view) = Linked(Arc::default())
            .render(request, source, CancellationToken::new())
            .await
            .unwrap();
        let encoded = wire::encode(request.identity, &cells, &view).unwrap();
        let (decoded, _) = wire::decode(&encoded, &wire_request).unwrap();
        assert_eq!(decoded, cells);
        let text: String = cells
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("No session attached"));
    }

    #[tokio::test]
    async fn typed_linked_scene_still_rejects_invalid_public_fields_and_cancelled_work() {
        let (request, _) = scene();
        let invalid = Scene::from(rsi_terminal_ui::scene::ApplicationScene {
            title: "x".repeat(rsi_terminal_ui::scene::MAXIMUM_SCENE_BYTES + 1),
            ..Default::default()
        });
        let renderer = Linked(Arc::default());
        assert!(
            renderer
                .render(request, invalid, CancellationToken::new())
                .await
                .is_err()
        );
        let (_, valid) = scene();
        let stop = CancellationToken::new();
        stop.cancel();
        assert_eq!(
            renderer.render(request, valid, stop).await.unwrap_err(),
            "presentation stopped"
        );
    }
    fn capture_scene(transcript: &rsi_terminal_ui::transcript::Transcript) -> Scene {
        use rsi_agent_session_protocol::{
            AgentPresetId, FrozenAgentSettings, SessionHeader, SessionId,
        };
        let header = SessionHeader::new_local(
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
                markdown: true,
                fold_focus: None,
                activity: None,
                header: &header,
                workspace_label: header.canonical_cwd(),
                transcript,
                editor: &rsi_terminal_ui::editor::Editor::with_text("retained draft".into(), 1024),
                model: None,
                reasoning_effort: None,
                menu_revision: 0,
                metrics: None,
                metrics_complete: false,
                model_unavailable: false,
                todos: None,
                completion: None,
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
            80,
            24,
        )
        .unwrap()
    }
    fn scene_with(transcript: &rsi_terminal_ui::transcript::Transcript) -> (FrameRequest, Scene) {
        let source = capture_scene(transcript);
        (
            FrameRequest {
                identity: wire::Identity {
                    attachment: 1,
                    presentation: 1,
                    revision: 1,
                },
                width: 80,
                height: 24,
            },
            source,
        )
    }

    #[tokio::test]
    #[ignore = "diagnostic typed Local versus JSON/render benchmark; no timing threshold"]
    async fn linked_visible_window_capture_and_render_cost() {
        use rsi_agent_session_protocol::{SessionFact, SessionFactBody, TurnId};
        let mut transcript = rsi_terminal_ui::transcript::Transcript::default();
        for seq in 1..=2 {
            transcript.apply(
                &SessionFact::new(
                    seq,
                    seq,
                    SessionFactBody::TurnAccepted {
                        reasoning_effort: None,
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
        let runtime = rsi_meta::Runtime::default();
        let mut owner = Owner::start(&runtime.root(), None).await.unwrap();
        let mut samples = [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()];
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
            let (request, source) = scene_with(&transcript);
            let started = std::time::Instant::now();
            std::hint::black_box(
                owner
                    .render(request, source, CancellationToken::new())
                    .await
                    .1
                    .unwrap(),
            );
            samples[4].push(started.elapsed());
        }
        owner.close().await.unwrap();
        assert!(runtime.shutdown().await.is_clean());
        for (phase, mut values) in [
            "capture",
            "encode",
            "decode",
            "render",
            "typed-owner-to-frame",
        ]
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
        let renderer = Arc::new(Linked(Arc::default()));
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
                .render(request, source, CancellationToken::new())
                .await
                .1
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
        let (request, source) = scene();
        let (frame, _) = owner
            .render(request, source, CancellationToken::new())
            .await
            .1
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
