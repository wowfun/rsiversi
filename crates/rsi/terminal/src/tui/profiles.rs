//! Terminal input and projection for the shared reviewed Host source workbench.
use super::{editor, *};
use futures_util::future::BoxFuture;
use rsi_configuration_api::leaf::{self as wire, CatalogRequest, ChangeKind, Principal, Target};
use rsi_terminal_ui::scene::{ApplicationScene, Draft, Scene};
use rsi_workbench_ui::{
    LeafCommand, LeafView, PluginsCommand, PluginsFeature, PluginsFeatureContract,
};
mod view;

#[derive(Clone)]
enum After {
    Browse,
    Leaf(Target),
    Review,
    Receipt,
    Grant(Target),
    Grants(usize),
}
#[derive(Clone)]
enum Choice {
    Run(LeafCommand, After),
    Leaf(Target),
    Review(wire::Preview),
    Configuration(Target),
    GrantKind(Target),
    GrantPrincipal(Target, ChangeKind),
    GrantInput(Target, ChangeKind, bool),
    Grants(usize),
    Receipts(usize),
    Browse,
    Close,
}
enum Form {
    Configuration(Target),
    Grant(Target, ChangeKind, bool),
}
pub(super) struct Ui {
    pub active: bool,
    feature: Option<Arc<PluginsFeature>>,
    work: Option<BoxFuture<'static, (LeafView, std::result::Result<(), String>)>>,
    after: After,
    view: LeafView,
    items: Vec<(String, Choice)>,
    selected: usize,
    revision: u64,
    title: String,
    explanation: String,
    detail: Option<String>,
    detail_open: bool,
    offset: usize,
    status: String,
    form: Option<Form>,
    editor: editor::Editor,
}
impl Ui {
    pub fn new(context: &rsi_meta::Context) -> Self {
        Self {
            active: false,
            feature: context.lookup_local::<PluginsFeatureContract>(),
            work: None,
            after: After::Browse,
            view: LeafView::default(),
            items: vec![],
            selected: 0,
            revision: 0,
            title: "Host Profiles".into(),
            explanation: String::new(),
            detail: None,
            detail_open: false,
            offset: 0,
            status: String::new(),
            form: None,
            editor: editor::Editor::default(),
        }
    }
    pub fn open(&mut self) {
        self.active = true;
        self.run(
            LeafCommand::Read {
                query: CatalogRequest::default(),
            },
            After::Browse,
        );
    }
    fn run(&mut self, command: LeafCommand, after: After) {
        if self.work.is_some() {
            self.status = "The current Profile operation is still running".into();
            return;
        }
        let Some(feature) = self.feature.clone() else {
            self.status = "Host Profile management is unavailable".into();
            return;
        };
        self.after = after;
        self.status = "Waiting for Profile owner…".into();
        self.work = Some(Box::pin(async move {
            let result = feature.command(PluginsCommand::Leaves { command }).await;
            (feature.snapshot().leaves, result)
        }));
    }
    fn menu(
        &mut self,
        title: impl Into<String>,
        explanation: impl Into<String>,
        items: Vec<(String, Choice)>,
    ) {
        self.title = title.into();
        self.explanation = explanation.into();
        self.items = items;
        self.selected = 0;
        self.detail = None;
        self.detail_open = false;
        self.offset = 0;
        self.form = None;
        self.editor = editor::Editor::default();
        self.revision = self.revision.wrapping_add(1);
    }
    fn details(&mut self) {
        self.detail = Some(std::mem::take(&mut self.explanation));
        self.detail_open = true;
    }
    fn choose(&mut self, choice: Choice) {
        match choice {
            Choice::Run(command, after) => self.run(command, after),
            Choice::Leaf(target) => self.leaf(&target),
            Choice::Review(preview) => self.review(&preview),
            Choice::Configuration(target) => {
                self.menu(
                    "Replace leaf configuration",
                    "Enter the complete JSON replacement. Review follows preparation.",
                    vec![],
                );
                self.form = Some(Form::Configuration(target));
                self.editor.limit = 64 * 1024;
            }
            Choice::GrantKind(target) => self.grant_kind(&target),
            Choice::GrantPrincipal(target, kind) => self.grant_principal(&target, kind),
            Choice::GrantInput(target, kind, agent) => {
                self.menu(
                    "Grant an exact change",
                    "Enter the exact principal ID to receive this leaf operation.",
                    vec![],
                );
                self.editor.limit = 256;
                self.form = Some(Form::Grant(target, kind, agent));
            }
            Choice::Grants(offset) => self.grants(offset),
            Choice::Receipts(offset) => self.receipts(offset),
            Choice::Browse => self.browse(),
            Choice::Close => self.active = false,
        }
    }
    pub fn paste(&mut self, text: &str) {
        if self.form.is_some()
            && self.work.is_none()
            && let Err(error) = self.editor.insert(text)
        {
            self.status = error.into();
        }
    }
    pub fn key(&mut self, key: termina::event::KeyEvent) {
        if key.code == KeyCode::Escape {
            if self.form.is_some() {
                self.browse();
            } else if self.detail.is_some() && !self.detail_open {
                self.detail_open = true;
            } else {
                self.active = false;
            }
            return;
        }
        if self.work.is_some() {
            return;
        }
        if self.detail_open {
            match key.code {
                KeyCode::Up | KeyCode::PageUp => self.offset = self.offset.saturating_sub(4),
                KeyCode::Down | KeyCode::PageDown => self.offset = self.offset.saturating_add(4),
                KeyCode::Enter => self.detail_open = false,
                _ => {}
            }
            return;
        }
        if self.form.is_some() {
            if key.code == KeyCode::Enter && !key.modifiers.contains(Modifiers::SHIFT) {
                self.submit();
            } else if let Err(error) = self.editor.key(key) {
                self.status = error.into();
            }
        } else {
            match key.code {
                KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down => {
                    self.selected = (self.selected + 1).min(self.items.len().saturating_sub(1));
                }
                KeyCode::Enter => {
                    if let Some((_, choice)) = self.items.get(self.selected) {
                        self.choose(choice.clone());
                    }
                }
                _ => {}
            }
        }
    }
    fn submit(&mut self) {
        let Some(form) = &self.form else {
            return;
        };
        let (command, after) = match form {
            Form::Configuration(target) => (
                LeafCommand::Configuration {
                    target: target.clone(),
                    document: self.editor.text().into(),
                },
                After::Review,
            ),
            Form::Grant(target, kind, agent) => {
                let principal = serde_json::from_value::<Principal>(
                    serde_json::json!({"kind":if *agent {"agent"} else {"device"},"id":self.editor.text()}),
                );
                let Ok(principal) = principal else {
                    self.status = "Enter a valid exact principal ID".into();
                    return;
                };
                let Some(grants) = &self.view.grants else {
                    self.status = "Read current grants first".into();
                    return;
                };
                (
                    LeafCommand::Grant {
                        revision: grants.revision.clone(),
                        scope: wire::Grant {
                            principal,
                            target: target.clone(),
                            operation: *kind,
                        },
                        granted: true,
                    },
                    After::Leaf(target.clone()),
                )
            }
        };
        self.run(command, after);
    }
    pub fn mouse(
        &mut self,
        mouse: termina::event::MouseEvent,
        view: &rsi_terminal_ui::render::View,
    ) {
        if self.form.is_none()
            && !self.detail_open
            && view.choice_revision == self.revision
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(index) = view.choice_at(mouse.column, mouse.row)
            && index < self.items.len()
        {
            self.selected = index;
        }
    }
    pub async fn next(&mut self) {
        let Some(work) = self.work.as_mut() else {
            return futures_util::future::pending().await;
        };
        let (view, result) = work.await;
        self.work = None;
        self.view = view;
        self.status = result
            .as_ref()
            .err()
            .cloned()
            .or_else(|| self.view.notice.clone())
            .unwrap_or_default();
        if result.is_err() && self.form.is_some() {
            return;
        }
        match self.after.clone() {
            After::Browse => self.browse(),
            After::Leaf(target) => self.leaf(&target),
            After::Review => {
                if let Some(preview) = self.view.previews.last().cloned() {
                    self.review(&preview);
                } else {
                    self.browse();
                }
            }
            After::Receipt => self.receipt(),
            After::Grant(target) => self.grant_kind(&target),
            After::Grants(offset) => self.grants(offset),
        }
    }
    pub fn scene(&self) -> std::result::Result<Scene, &'static str> {
        Ok(Scene::from(ApplicationScene {
            title: self.title.clone(),
            explanation: self.explanation.clone(),
            status: self.status.clone(),
            revision: self.revision,
            detail: self.detail_open.then(|| self.detail.clone()).flatten(),
            detail_offset: self.offset,
            items: if self.detail_open {
                vec![]
            } else {
                self.items.iter().map(|(label, _)| label.clone()).collect()
            },
            selected: self.selected,
            field: self.form.as_ref().map(|form| {
                match form {
                    Form::Configuration(_) => "Complete JSON configuration",
                    Form::Grant(_, _, true) => "Agent Session ID",
                    Form::Grant(_, _, false) => "Device ID",
                }
                .into()
            }),
            input: Draft::capture(&self.editor)?,
            hint: if self.detail_open {
                "Enter actions · ↑/↓ scroll · Esc back"
            } else {
                "Enter select/submit · Shift+Enter line · Esc back; admitted writes continue"
            }
            .into(),
            progress: self
                .work
                .as_ref()
                .map(|_| "Waiting for Profile owner…".into()),
            ..ApplicationScene::default()
        }))
    }
    pub async fn shutdown(&mut self) {
        self.active = false;
        while self.work.is_some() {
            self.next().await;
        }
    }
}
