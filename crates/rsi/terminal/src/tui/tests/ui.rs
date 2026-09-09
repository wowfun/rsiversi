use super::*;
use rsi_meta::{ActivationPlan, ConfigValue, Context, PluginFactory, PreparedActivation};
use rsi_ui::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use termina::event::KeyEvent;

#[derive(Debug, Default)]
struct Addon {
    calls: Mutex<Vec<ActionInput>>,
    entered: tokio::sync::Notify,
    finished: AtomicUsize,
    gate: tokio::sync::Notify,
    blocked: bool,
    read: bool,
}
impl SurfaceRenderer for Addon {
    fn render(&self, _: &Context) -> rsi_ui::Result<UiView> {
        Ok(UiView {
            title: "Independent form".into(),
            elements: vec![
                UiElement::Input {
                    name: "title".into(),
                    label: "Title".into(),
                    value: "first".into(),
                    multiline: false,
                },
                UiElement::Input {
                    name: "body".into(),
                    label: "Body".into(),
                    value: "second".into(),
                    multiline: true,
                },
                UiElement::Button {
                    action: "save".into(),
                    label: "Save".into(),
                    value: serde_json::json!({"operation":"save"}),
                },
            ],
        })
    }
}
#[derive(Debug)]
struct Handler(Arc<Addon>);
impl UiAction for Handler {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> futures_util::future::BoxFuture<'static, rsi_ui::Result<UiView>> {
        let addon = self.0.clone();
        Box::pin(async move {
            addon.calls.lock().unwrap().push(input);
            addon.entered.notify_one();
            if addon.blocked {
                if addon.read {
                    tokio::select! {
                        () = target.view_closed() => { addon.finished.fetch_add(1, Ordering::SeqCst); return Err(UiError::Retired); },
                        () = addon.gate.notified() => {},
                    }
                } else {
                    addon.gate.notified().await;
                }
            }
            addon.finished.fetch_add(1, Ordering::SeqCst);
            Err(UiError::Action(
                "fixture rejected; preserve the edits".into(),
            ))
        })
    }
}
#[derive(Debug)]
struct Factory(Arc<Addon>);
#[async_trait::async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<UiContract>()?
            .register(
                &plan,
                Contributions {
                    name: "fixture.form".into(),
                    surfaces: vec![SurfaceContribution {
                        name: "form".into(),
                        title: "Independent form".into(),
                        target: TargetKind::Surface,
                        renderer: self.0.clone(),
                    }],
                    actions: vec![ActionContribution {
                        name: "save".into(),
                        target: TargetKind::Surface,
                        handler: Arc::new(Handler(self.0.clone())),
                    }],
                    renderers: Vec::new(),
                },
            )
            .map_err(|problem| rsi_meta::MetaError::Activation(problem.to_string()))?;
        plan.defer(
            "withdraw form",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    Ok(())
                })
            }),
        )
    }
}
async fn mount(
    client: &mut Client,
    runtime: &rsi_meta::Runtime,
    addon: Arc<Addon>,
) -> rsi_meta::FiberHandle {
    let fiber = runtime
        .root()
        .apply(
            rsi_meta::ResolvedFactory::linked(
                "fixture.form",
                "test",
                rsi_meta::UpdateMode::RestartRequired,
                Arc::new(Factory(addon)),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    assert_eq!(fiber.snapshot().state, rsi_meta::FiberState::Active);
    client.action_menu();
    let action = client
        .state
        .menu
        .take()
        .unwrap()
        .items
        .into_iter()
        .find(|(label, _)| label == "Independent form")
        .unwrap()
        .1;
    client.action(action);
    assert!(
        client
            .state
            .detail
            .as_ref()
            .unwrap()
            .contains("Title: first")
    );
    fiber
}
fn action(client: &Client, label: &str) -> super::Action {
    client
        .state
        .detail_actions
        .as_ref()
        .unwrap()
        .items
        .iter()
        .find(|(name, _)| name == label)
        .unwrap()
        .1
        .clone()
}
fn enter() -> KeyEvent {
    KeyCode::Enter.into()
}

fn rendered(client: &Client, name: &str, width: u16, height: u16) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            super::super::render::draw(frame, &client.state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    for y in 3..height.saturating_sub(6) {
        for x in [0, 1, width - 2, width - 1] {
            assert_eq!(
                buffer[(x, y)].symbol(),
                " ",
                "modal margin exposes the obscured conversation at {x},{y}"
            );
        }
    }
    let text = buffer
        .content
        .chunks(usize::from(width))
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    if let Ok(directory) = std::env::var("RSI_TUI_UI_REPORT") {
        std::fs::create_dir_all(&directory).unwrap();
        let cells: Vec<_> = buffer.content.iter().enumerate().map(|(index, cell)| serde_json::json!({
            "x": index % usize::from(width), "y": index / usize::from(width), "text": cell.symbol(),
            "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg), "bold": cell.modifier.contains(ratatui::style::Modifier::BOLD),
        })).collect();
        let path = std::path::Path::new(&directory).join(name);
        std::fs::write(
            path.with_extension("json"),
            serde_json::to_vec(&serde_json::json!({"width":width,"height":height,"cells":cells}))
                .unwrap(),
        )
        .unwrap();
        std::fs::write(path.with_extension("txt"), &text).unwrap();
    }
    text
}

#[tokio::test]
async fn contributed_form_survives_menu_edit_discard_and_action_failure() {
    let (mut client, _, runtime, surface) = client().await;
    let addon = Arc::new(Addon::default());
    let _fiber = mount(&mut client, &runtime, addon.clone()).await;
    client.state.editor.insert("conversation draft").unwrap();
    client.action_menu();
    client.state.escape();
    client.action(action(&client, "Edit Title"));
    assert!(
        client.state.ui_edit.is_some(),
        "closing the menu left card actions stale"
    );
    client.state.ui_paste("\nforbidden");
    assert_eq!(client.state.ui_edit.as_ref().unwrap().editor.text, "first");
    client.state.ui_paste(" 中文\x1b[31m");
    let screen = rendered(&client, "form-edit", 110, 30);
    assert!(
        screen.contains("first 中 文"),
        "the card popup hides the field editor: {screen}"
    );
    client.state.ui_key(enter());
    let preserved = "first 中文\x1b[31m";
    assert!(!client.state.detail.as_ref().unwrap().contains('\x1b'));
    client.action(action(&client, "Edit Title"));
    client.state.ui_paste(" discarded");
    client.state.escape();
    client.action(action(&client, "Edit Body"));
    client.state.ui_paste("\nline two");
    client.state.ui_key(enter());
    client.action(action(&client, "Save"));
    let work = client.tasks.next().await.unwrap();
    assert!(work.result.is_err());
    assert!(!work.superseded(&client));
    client.ui_failed();
    client
        .state
        .notice(work.result.as_ref().err().unwrap().to_string());
    assert!(rendered(&client, "form-failed", 110, 30).contains("line two"));
    assert!(rendered(&client, "form-narrow", 42, 12).contains("Independent form"));
    let input = addon.calls.lock().unwrap()[0].clone();
    assert_eq!(input.fields["title"], preserved);
    assert_eq!(input.fields["body"], "second\nline two");
    assert_eq!(input.value, serde_json::json!({"operation":"save"}));
    assert!(client.state.detail.as_ref().unwrap().contains("line two"));
    assert_eq!(client.state.editor.text, "conversation draft");
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn closed_contributed_read_stops_but_mutation_keeps_its_owner() {
    for read in [true, false] {
        let (mut client, _, runtime, surface) = client().await;
        let addon = Arc::new(Addon {
            blocked: true,
            read,
            ..Default::default()
        });
        let _fiber = mount(&mut client, &runtime, addon.clone()).await;
        client.action(action(&client, "Save"));
        tokio::time::timeout(Duration::from_secs(1), addon.entered.notified())
            .await
            .unwrap();
        let visible = client.state.detail_stop.clone();
        client.action_menu();
        client.state.escape();
        assert!(
            !visible.is_cancelled(),
            "menu dismissal cancelled a still visible card"
        );
        assert_eq!(addon.finished.load(Ordering::SeqCst), 0);
        client.state.escape();
        let work = tokio::time::timeout(Duration::from_secs(1), client.tasks.next())
            .await
            .unwrap()
            .unwrap();
        assert!(work.superseded(&client));
        assert!(client.state.detail.is_none());
        if !read {
            assert_eq!(addon.finished.load(Ordering::SeqCst), 0);
            addon.gate.notify_one();
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while addon.finished.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(addon.calls.lock().unwrap().len(), 1);
        surface.stop().await;
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn retired_target_closes_contributed_form_and_rejects_old_actions() {
    let (mut client, _, runtime, surface) = client().await;
    let addon = Arc::new(Addon::default());
    let _fiber = mount(&mut client, &runtime, addon.clone()).await;
    let stale = action(&client, "Save");
    surface.stop().await;
    client.ui_changed();
    assert!(client.state.detail.is_none());
    client.action(stale);
    assert!(client.tasks.is_empty());
    assert!(addon.calls.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn removed_addon_withdraws_menu_and_displayed_actions() {
    let (mut client, _, runtime, surface) = client().await;
    let addon = Arc::new(Addon::default());
    let fiber = mount(&mut client, &runtime, addon.clone()).await;
    let stale = action(&client, "Save");
    assert!(fiber.dispose().await.is_clean());
    client.ui_changed();
    assert!(client.state.detail.is_none());
    client.action(stale);
    assert!(client.tasks.is_empty());
    assert!(addon.calls.lock().unwrap().is_empty());
    client.action_menu();
    assert!(
        !client
            .state
            .menu
            .as_ref()
            .unwrap()
            .items
            .iter()
            .any(|(label, _)| label == "Independent form")
    );
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn contributed_source_read_uses_and_cancels_the_actual_session_controller() {
    let fixture = Arc::new(UnknownThenAcceptedHandle {
        read_gate: Some(Arc::new(tokio::sync::Semaphore::new(0))),
        ..Default::default()
    });
    let (mut client, handle, runtime, surface) = client_with(fixture).await;
    client.state.transcript.apply(&history_fact(9));
    client.action(super::Action::UiCard);
    let read = client.state.detail_actions.as_ref().unwrap().items[0]
        .1
        .clone();
    client.action(read);
    tokio::time::timeout(Duration::from_secs(1), async {
        while handle.read_active.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    client.state.escape();
    let work = tokio::time::timeout(Duration::from_secs(1), client.tasks.next())
        .await
        .unwrap()
        .unwrap();
    assert!(work.superseded(&client));
    tokio::time::timeout(Duration::from_secs(1), async {
        while handle.read_active.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(handle.cancellations.lock().unwrap().is_empty());
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}
