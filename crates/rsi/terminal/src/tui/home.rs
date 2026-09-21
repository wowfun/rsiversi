//! Unattached application state: editing, setup and durable history need no controller.
use super::*;
use rsi_terminal_ui::scene::{ApplicationScene, Scene};

pub(super) async fn needs_setup(
    context: &rsi_meta::Context,
    catalog: &dyn rsi_ai_protocol::LanguageModels,
) -> Result<Option<String>> {
    let Some(settings) = context.lookup_local::<rsi_settings_protocol::SettingsAccessContract>()
    else {
        return Ok(None);
    };
    let snapshot = settings.read("rsi.agent").await.map_err(error)?;
    let Some(model) = snapshot.value.get("default_model") else {
        return Ok(Some(
            "No default model. Use /login or /model to start.".into(),
        ));
    };
    let model: ModelRef = serde_json::from_value(model.clone()).map_err(error)?;
    match setup::configured_models(catalog, Some(&model)).await {
        Ok(routes) if routes.contains(&model) => return Ok(None),
        Err(problem) => {
            return Ok(Some(format!(
                "Could not read model catalog: {problem}. Use /model to refresh or /login; history remains available."
            )));
        }
        Ok(_) => {}
    }
    Ok(Some(format!(
        "Default route {}/{} is unavailable. Use /login or /model.",
        model.deployment(),
        model.model()
    )))
}

enum Startup {
    Attached(Box<Attachment>),
    Home(String),
}

struct Menu {
    title: &'static str,
    hint: &'static str,
    items: Vec<(String, Option<SessionId>)>,
}

async fn startup(
    context: &rsi_meta::Context,
    application: &Arc<dyn SessionService>,
    workspace: &Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    selection: SessionSelection,
    catalog: &dyn rsi_ai_protocol::LanguageModels,
) -> Result<Startup> {
    let resumed = matches!(selection, SessionSelection::Resume { .. });
    if !resumed && let Some(reason) = needs_setup(context, catalog).await? {
        return Ok(Startup::Home(reason));
    }
    let handle = match resolve_application_handle(application, workspace, selection).await {
        Ok(handle) => handle,
        Err(HandleError::Session(SessionError::SetupRequired)) if !resumed => {
            return Ok(Startup::Home("Default model configuration is no longer available. Use /login or /model to start.".into()));
        }
        Err(error) => return Err(error.into()),
    };
    Ok(Startup::Attached(Box::new(
        attachment(handle, resumed, None).await?,
    )))
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) async fn run(
    context: &rsi_meta::Context,
    application: &Arc<dyn SessionService>,
    workspace: &Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    selection: SessionSelection,
    setup: &mut setup::Ui,
    external: &mut external::Ui,
    profiles: &mut profiles::Ui,
    terminal: &mut terminal::Terminal,
    presentation: &mut crate::presentation::Owner,
    input: &mut mpsc::Receiver<input::Input>,
    stop: &tokio_util::sync::CancellationToken,
    terminate: &mut std::pin::Pin<Box<impl std::future::Future<Output = ()>>>,
    catalog: &dyn rsi_ai_protocol::LanguageModels,
    markdown: &mut bool,
) -> Result<Option<(Attachment, editor::Editor)>> {
    let initial = tokio::select! { biased;
        () = stop.cancelled() => return Ok(None),
        () = &mut *terminate => return Ok(None),
        result = startup(context, application, workspace, selection.clone(), catalog) => result?,
    };
    let mut status = match initial {
        Startup::Attached(attached) => return Ok(Some((*attached, editor::Editor::default()))),
        Startup::Home(reason) => reason,
    };
    let mut editor = editor::Editor::default();
    let mut slash = slash::Ui::default();
    let mut view = render::View::default();
    let mut menu: Option<Menu> = None;
    let mut selected: usize = 0;
    let mut recent = None;
    let mut history: Option<
        futures_util::future::BoxFuture<'static, Result<rsi_session_protocol::RecentSessionPage>>,
    > = None;
    let mut attaching: Option<futures_util::future::BoxFuture<'static, Result<Attachment>>> = None;
    let mut copying: Option<futures_util::future::BoxFuture<'static, clipboard::Delivery>> = None;
    let mut changes = presentation.changes();
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut revision = 0_u64;
    let mut epoch = 1_u64;
    let mut rendering: Option<RenderJob> = None;
    let mut render_stop = stop.child_token();
    let _cancel_render = render_stop.clone().drop_guard();
    let mut dirty = true;
    let mut dimensions = terminal::size();
    let mut navigation_active = (external.active, profiles.active);
    let outcome = loop {
        if let Some(session_id) = external.native.take() {
            let application = application.clone();
            let workspace = workspace.clone();
            attaching = Some(Box::pin(async move {
                attachment(
                    resolve_application_handle(
                        &application,
                        &workspace,
                        SessionSelection::Resume {
                            session_id,
                            cwd: None,
                        },
                    )
                    .await?,
                    true,
                    None,
                )
                .await
            }));
            dirty = true;
        }
        if navigation_active != (external.active, profiles.active) {
            navigation_active = (external.active, profiles.active);
            render_stop.cancel();
            rendering = None;
            render_stop = stop.child_token();
            epoch += 1;
            view = render::View::default();
            dirty = true;
        }
        if profiles.active || external.active || menu.is_some() || setup.active {
            slash.hide();
        } else {
            slash.update(&editor, None);
        }
        if setup.chosen.take().is_some() {
            let application = application.clone();
            let workspace = workspace.clone();
            let selection = selection.clone();
            attaching = Some(Box::pin(async move {
                attachment(
                    resolve_application_handle(&application, &workspace, selection).await?,
                    false,
                    None,
                )
                .await
            }));
            status = "Starting session… Draft retained.".into();
        }
        tokio::select! {
            () = stop.cancelled() => break Ok(None),
            () = &mut *terminate => break Ok(None),
            change = changes.changed() => { change.map_err(error)?; render_stop.cancel(); rendering = None; render_stop = stop.child_token(); epoch += 1; dirty = true; },
            () = external.next() => {dirty=true;},
                () = profiles.next() => {dirty=true;},
            () = setup.next() => { if !setup.active {status=setup.notice();} dirty = true; },
            ack = terminal.presented.changed() => { ack.map_err(error)?; if let Some(frame)=terminal.presented.borrow_and_update().as_ref() && frame.presentation==epoch {view=frame.view.clone(); slash.presented(&view); } },
            delivery = async { match &mut copying { Some(work) => work.await, None => std::future::pending().await } } => {
                copying = None; status = delivery.status;
                if let Some(osc) = delivery.osc { let _ = terminal.commands.try_send(osc); }
                dirty = true;
            },
            result = async { match &mut attaching { Some(work) => work.await, None => std::future::pending().await } } => {
                attaching = None;
                match result { Ok(attached) => break Ok(Some((attached, editor))), Err(problem) => status = problem.to_string() }
                dirty = true;
            },
            result = async { match &mut history { Some(work) => work.await, None => std::future::pending().await } } => {
                history = None; dirty = true;
                match result {
                    Ok(page) => {
                        recent = page.sessions.last().map(rsi_session_protocol::SessionSummary::cursor);
                        let mut items = page.sessions.into_iter().map(|summary| (format!("{} · {}", summary.header.session_id(), summary.header.canonical_cwd()), Some(summary.header.session_id().clone()))).collect::<Vec<_>>();
                        if page.has_more { items.push(("More sessions…".into(), None)); }
                        selected = 0; menu = Some(Menu { title: "Recent sessions", hint: "Enter opens · Ctrl+Y copy ID · Esc back", items }); status.clear();
                    }
                    Err(problem) => status = problem.to_string(),
                }
            },
            (request, result) = async { match &mut rendering { Some(work) => work.await, None => std::future::pending().await } } => {
                rendering = None;
                if request.identity.presentation != epoch || (request.width, request.height) != terminal::size() { dirty = true; continue; }
                match result {
                    Ok((buffer, view)) => { terminal.frames.send_replace(Some(Arc::new(terminal::RenderedFrame { generation: 0, presentation: epoch, revision: request.identity.revision, buffer, view: render::View(view) }))); }
                    Err(problem) => { status = problem; dirty = true; }
                }
            },
            incoming = input.recv() => {
                dirty = true;
                match incoming.unwrap_or(input::Input::Closed) {
                    input::Input::Closed => break Ok(None),
                    input::Input::Rejected(message) => status = message.into(),
                    input::Input::Terminal(termina::Event::Paste(text)) => {
                        if profiles.active {profiles.paste(&text);continue;}
                            if external.active {external.paste(&text);continue;}
                        if setup.active { setup.paste(text); } else if slash.paste(&text) || menu.is_some() {} else if let Err(problem) = editor.insert(&text) { status = problem.into(); }
                    }
                    input::Input::Terminal(termina::Event::Key(key)) if key.kind != KeyEventKind::Release => {
                        if profiles.active {profiles.key(key);continue;}
                            if external.active {external.key(key);continue;}
                        if setup.active { setup.key(key); if !setup.active { status = setup.notice(); } continue; }
                        let control = key.modifiers.contains(Modifiers::CONTROL);
                        if slash.help { slash.key(key, &mut editor); continue; }
                        if menu.is_some() && control && key.code == KeyCode::Char('c') { menu = None; continue; }
                        if control && key.code == KeyCode::Char('d') && editor.text().is_empty() { break Ok(None); }
                        if control && key.code == KeyCode::Char('c') { break Ok(None); }
                        if menu.is_none() && slash.key(key, &mut editor) {continue;}
                        if control && key.code == KeyCode::Char('y') && let Some((_,Some(id)))=menu.as_ref().and_then(|menu|menu.items.get(selected)) {
                            if copying.is_none() { copying = Some(Box::pin(clipboard::copy(id.to_string()))); status = "Copying session ID…".into(); }
                            continue;
                        }
                        if key.code == KeyCode::Escape { menu = None; continue; }
                        if let Some(active) = &menu {
                            let items = &active.items;
                            match key.code {
                                KeyCode::Up => selected = selected.saturating_sub(1),
                                KeyCode::Down => selected = (selected + 1).min(items.len().saturating_sub(1)),
                                KeyCode::Enter => if let Some((label, session)) = items.get(selected).cloned() {
                                    menu = None;
                                    if let Some(session_id) = session {
                                        let application = application.clone(); let workspace = workspace.clone();
                                        attaching = Some(Box::pin(async move { attachment(resolve_application_handle(&application, &workspace, SessionSelection::Resume { session_id, cwd: None }).await?, true, None).await }));
                                    } else if label == "Exit" { break Ok(None); }
                                    else if label == "/help" { slash.open_help(); }
                                    else if label == "/login" { setup.open(setup::Command::Login(None), false); }
                                    else if label == "/external" {external.open();}
                                    else if label == "/profiles" {profiles.open();}
                                    else if label == "/attention" {external.open_attention();}
                                    else if label == "/model" { setup.open(setup::Command::Models, false); }
                                    else { let application = application.clone(); let cursor = if label == "More sessions…" { recent.clone() } else { None }; history = Some(Box::pin(async move { application.list_recent(cursor.as_ref(), 128).await.map_err(error) })); }
                                },
                                _ => {},
                            }
                        } else if control && key.code == KeyCode::Char('p') {
                            selected = 0; status.clear(); menu = Some(Menu { title: "Actions", hint: "Enter select · Esc back", items: vec![("/login".into(), None), ("/model".into(), None), ("Recent sessions".into(), None), ("/external".into(),None), ("/profiles".into(),None), ("/attention".into(),None), ("Exit".into(), None), ("/help".into(), None)] });
                        } else if (key.code == KeyCode::Enter && !key.modifiers.contains(Modifiers::SHIFT)) || (control && key.code == KeyCode::Char('s')) {
                            if let Some(command) = setup::command(editor.text()) {
                                if editor.cursor()!=editor.text().len() || matches!(command,setup::Command::Invalid) {status="Invalid command or cursor not at end. Draft retained; /help lists usage.".into();continue;}
                                editor.take();
                                match command {
                                    setup::Command::Attention => external.open_attention(),
                                    setup::Command::ExternalOpen(id) => external.open_conversation(id),
                                    setup::Command::External => external.open(),
                                    setup::Command::Profiles => profiles.open(),
                                    setup::Command::Quit => break Ok(None),
                                    setup::Command::Help => slash.open_help(),
                                    setup::Command::Markdown(mode) => { *markdown = mode.unwrap_or(!*markdown); status = super::markdown_status(*markdown).into(); editor.take(); },
                                    setup::Command::New | setup::Command::Effort => status="Choose a model with /login or /model first. Draft retained.".into(),
                                    setup::Command::Resume(None) => {let application=application.clone();history=Some(Box::pin(async move{application.list_recent(None,128).await.map_err(error)}));},
                                    setup::Command::Resume(Some(session_id)) => {let application=application.clone();let workspace=workspace.clone();attaching=Some(Box::pin(async move{attachment(resolve_application_handle(&application,&workspace,SessionSelection::Resume{session_id,cwd:None}).await?,true,None).await}));},
                                    command => setup.open(command,false),
                                }
                            } else {status="Choose a model with /login or /model first. Draft retained; nothing was sent.".into();}
                        } else if let Err(problem) = editor.key(key) { status = problem.into(); }
                    }
                    input::Input::Terminal(termina::Event::Mouse(mouse)) => { if terminal.presented.borrow().as_ref().is_some_and(|frame| frame.presentation==epoch && (frame.buffer.area.width,frame.buffer.area.height)==terminal::size()) {if profiles.active {profiles.mouse(mouse,&view.0);} else if external.active {external.mouse(mouse,&view.0);}else if setup.active {setup.mouse(mouse,&view.0);} else if let Some(menu) = &menu { if let Some(index) = view.0.choice_at(mouse.column, mouse.row) && index < menu.items.len() && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) { selected = index; } } else {slash.mouse(mouse,&view.0);}} },
                    input::Input::Terminal(_) => {},
                }
            },
            _ = tick.tick() => {
                if terminal.failed() { break Err(error("Terminal writer stopped")); }
                let (width, height) = terminal::size();
                if dimensions != (width, height) { dimensions = (width, height); dirty = true; }
                if !dirty || rendering.is_some() { continue; }
                let base = Scene::from(ApplicationScene { title: "RSI · No session attached".into(), explanation: "Connect a provider with /login, or choose a saved model with /model.".into(), input: rsi_terminal_ui::scene::Draft::capture(&editor).map_err(error)?, status: if menu.is_some() { String::new() } else { status.clone() }, field: Some(String::new()), hint: if slash.popup.is_some() {"↑/↓ select · Tab fill · Esc hide"} else {"Enter send · Ctrl+J line · /help"}.into(), completion: if setup.active || slash.help || menu.is_some() { None } else { slash.popup.clone() }, ..ApplicationScene::default() });
                let scene = if profiles.active {profiles.scene()} else if external.active {external.scene()} else if setup.active { base.with_dialog(setup.scene().map_err(error)?) } else if slash.help { base.with_dialog(slash.scene().map_err(error)?) } else if let Some(menu) = &menu { base.with_dialog(Scene::from(ApplicationScene { title: menu.title.into(), items: menu.items.iter().map(|(label, _)| label.clone()).collect(), selected, hint: menu.hint.into(), status: status.clone(), ..ApplicationScene::default() })) } else { Ok(base) }.map_err(error)?;
                revision += 1;
                let request = rsi_terminal_ui::wire::Request { identity: rsi_terminal_ui::wire::Identity { attachment: 0, presentation: epoch, revision }, width, height, bytes: 0 };
                rendering = Some(presentation.render(request, scene, render_stop.clone())); dirty = false;
            }
        }
    };
    render_stop.cancel();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_ai_protocol::{LanguageModelPage, LanguageModels, ModelsError};
    use rsi_settings_protocol::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct CreateFailure {
        error: SessionError,
        creates: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl SessionService for CreateFailure {
        async fn create(
            &self,
            _: CreateSession,
        ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            Err(self.error.clone())
        }
        async fn attach(
            &self,
            _: &SessionId,
        ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
            Err(self.error.clone())
        }
        async fn list_recent(
            &self,
            _: Option<&rsi_session_protocol::RecentSessionCursor>,
            _: usize,
        ) -> rsi_session_protocol::Result<rsi_session_protocol::RecentSessionPage> {
            unreachable!()
        }
    }
    #[derive(Debug)]
    struct Workspace;
    #[async_trait::async_trait]
    impl rsi_workspace_protocol::WorkspaceRegistry for Workspace {
        async fn get_or_create(
            &self,
            path: &std::path::Path,
        ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceRecord> {
            Ok(rsi_workspace_protocol::WorkspaceRecord {
                id: rsi_workspace_protocol::WorkspaceId::parse("a".repeat(64)).unwrap(),
                path: path.into(),
            })
        }
        async fn get(
            &self,
            _: &rsi_workspace_protocol::WorkspaceId,
        ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceRecord> {
            unreachable!()
        }
        async fn list(
            &self,
            _: Option<rsi_workspace_protocol::WorkspaceCursor>,
            _: usize,
        ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspacePage> {
            unreachable!()
        }
        async fn status(
            &self,
            _: &rsi_workspace_protocol::WorkspaceId,
        ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceStatus> {
            unreachable!()
        }
        async fn delete_registration(
            &self,
            _: &rsi_workspace_protocol::WorkspaceId,
        ) -> rsi_workspace_protocol::Result<bool> {
            unreachable!()
        }
    }
    #[tokio::test]
    async fn setup_removed_between_catalog_check_and_create_returns_home_only_for_fresh_sessions() {
        let runtime = rsi_meta::Runtime::default();
        let context = runtime.root();
        let _provider = context
            .apply(
                rsi_meta::ResolvedFactory::linked(
                    "test.settings",
                    "1",
                    rsi_meta::UpdateMode::Replayable,
                    Arc::new(Settings(
                        serde_json::json!({"default_model":{"deployment":"test","model":"00000"}}),
                    )),
                ),
                serde_json::Value::Null,
            )
            .await
            .unwrap();
        let workspace: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry> = Arc::new(Workspace);
        let fresh = SessionSelection::Fresh {
            cwd: std::path::PathBuf::from("."),
            session_id: Some(SessionId::new("setup-race").unwrap()),
            agent_preset_id: None,
        };
        for error in [
            SessionError::SetupRequired,
            SessionError::Backend(SessionError::SetupRequired.to_string()),
            SessionError::Invalid("corrupt settings".into()),
            SessionError::ShuttingDown,
        ] {
            let domain = Arc::new(CreateFailure {
                error: error.clone(),
                creates: AtomicUsize::new(0),
            });
            let application: Arc<dyn SessionService> = domain.clone();
            let catalog = Catalog {
                total: 1,
                fail: false,
                calls: AtomicUsize::new(0),
            };
            let result = startup(&context, &application, &workspace, fresh.clone(), &catalog).await;
            assert_eq!(
                domain.creates.load(Ordering::SeqCst),
                1,
                "preflight must allow the create attempt"
            );
            assert_eq!(catalog.calls.load(Ordering::SeqCst), 1);
            if error == SessionError::SetupRequired {
                assert!(
                    matches!(result, Ok(Startup::Home(ref reason)) if reason.contains("/login") && reason.contains("/model")),
                    "SetupRequired after preflight must keep the application open"
                );
            } else {
                assert!(matches!(result, Err(RsiError::Boot(_))));
            }
            let resumed = SessionSelection::Resume {
                session_id: SessionId::new("saved-session").unwrap(),
                cwd: None,
            };
            assert!(matches!(
                startup(&context, &application, &workspace, resumed, &catalog).await,
                Err(RsiError::Boot(_))
            ));
            assert_eq!(
                catalog.calls.load(Ordering::SeqCst),
                1,
                "resume skips current defaults and catalog"
            );
        }
        assert!(runtime.shutdown().await.is_clean());
    }

    #[derive(Debug)]
    struct Catalog {
        total: usize,
        fail: bool,
        calls: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl LanguageModels for Catalog {
        async fn describe_model(
            &self,
            _: &rsi_ai_protocol::ModelRef,
        ) -> std::result::Result<
            rsi_ai_protocol::LanguageModelDescription,
            rsi_ai_protocol::ModelsError,
        > {
            Err(rsi_ai_protocol::ModelsError::Invalid(
                "test catalog has no model profile".into(),
            ))
        }

        async fn list_models(
            &self,
            after: Option<&ModelRef>,
            limit: usize,
        ) -> std::result::Result<LanguageModelPage, ModelsError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(ModelsError::Capacity);
            }
            let start = after.map_or(0, |model| model.model().parse::<usize>().unwrap() + 1);
            let end = (start + limit).min(self.total);
            Ok(LanguageModelPage {
                models: (start..end)
                    .map(|i| ModelRef::new("test", format!("{i:05}")).unwrap())
                    .collect(),
                has_more: end < self.total,
            })
        }
    }
    #[derive(Clone, Debug)]
    struct Settings(serde_json::Value);
    #[async_trait::async_trait]
    impl rsi_meta::PluginFactory for Settings {
        fn prepare(
            &self,
            value: &serde_json::Value,
        ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
            Ok(rsi_meta::PreparedActivation::new(value.clone()))
        }
        async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
            let supply = plan
                .context()
                .provide_local::<SettingsAccessContract>(Arc::new(self.clone()))?;
            plan.defer(
                "withdraw test settings",
                Box::new(move || {
                    Box::pin(async move {
                        drop(supply);
                        Ok(())
                    })
                }),
            )
        }
    }
    #[async_trait::async_trait]
    impl SettingsAccess for Settings {
        async fn read(&self, _: &str) -> rsi_settings_protocol::Result<SettingsSnapshot> {
            Ok(SettingsSnapshot {
                scope_id: SettingsScopeId::parse("a".repeat(32)).unwrap(),
                revision: 0,
                value: self.0.clone(),
            })
        }
        async fn list(
            &self,
            _: Option<&str>,
            _: usize,
        ) -> rsi_settings_protocol::Result<SettingsPage> {
            unreachable!()
        }
        async fn describe(&self, _: &str) -> rsi_settings_protocol::Result<SettingsDescription> {
            unreachable!()
        }
        async fn replace(
            &self,
            _: &str,
            _: &SettingsVersion,
            _: serde_json::Value,
        ) -> rsi_settings_protocol::Result<SettingsSnapshot> {
            unreachable!()
        }
        async fn clear(
            &self,
            _: &str,
            _: &SettingsVersion,
        ) -> rsi_settings_protocol::Result<SettingsSnapshot> {
            unreachable!()
        }
    }
    #[tokio::test]
    async fn catalog_failure_or_bound_keeps_setup_reachable_but_invalid_settings_still_fail() {
        let runtime = rsi_meta::Runtime::default();
        let context = runtime.root();
        let provider = context
            .apply(rsi_meta::ResolvedFactory::linked("test.settings", "1", rsi_meta::UpdateMode::Replayable, Arc::new(Settings(
                serde_json::json!({"default_model":{"deployment":"absent","model":"model"}}),
            ))), serde_json::Value::Null).await
            .unwrap();
        for (total, fail, expected) in [(0, true, "capacity"), (4097, false, "4096")] {
            let catalog = Catalog {
                total,
                fail,
                calls: AtomicUsize::new(0),
            };
            let reason = needs_setup(&context, &catalog)
                .await
                .unwrap()
                .expect("recoverable setup screen");
            assert!(reason.contains(expected), "{reason}");
            assert!(reason.contains("/model"));
            assert!(catalog.calls.load(Ordering::SeqCst) <= 16);
        }
        assert!(provider.dispose().await.is_clean());
        let _invalid = context
            .apply(
                rsi_meta::ResolvedFactory::linked(
                    "test.settings",
                    "1",
                    rsi_meta::UpdateMode::Replayable,
                    Arc::new(Settings(serde_json::json!({"default_model":false}))),
                ),
                serde_json::Value::Null,
            )
            .await
            .unwrap();
        let catalog = Catalog {
            total: 0,
            fail: false,
            calls: AtomicUsize::new(0),
        };
        assert!(needs_setup(&context, &catalog).await.is_err());
        assert_eq!(catalog.calls.load(Ordering::SeqCst), 0);
        assert!(runtime.shutdown().await.is_clean());
    }
    #[tokio::test]
    async fn catalog_scan_accepts_exact_bound_and_stops_when_startup_route_is_found() {
        let catalog = Catalog {
            total: 4096,
            fail: false,
            calls: AtomicUsize::new(0),
        };
        assert_eq!(
            setup::configured_models(&catalog, None)
                .await
                .unwrap()
                .len(),
            4096
        );
        assert_eq!(catalog.calls.load(Ordering::SeqCst), 16);
        let catalog = Catalog {
            total: 4097,
            fail: false,
            calls: AtomicUsize::new(0),
        };
        let model = ModelRef::new("test", "00000").unwrap();
        assert!(
            setup::configured_models(&catalog, Some(&model))
                .await
                .unwrap()
                .contains(&model)
        );
        assert_eq!(catalog.calls.load(Ordering::SeqCst), 1);
    }
}
