use super::editor::Editor;
use super::{Action, Client, Menu, Update, error, state::State};
use rsi_ui::{ActionInput, BoundView, Ui, UiElement, UiReference, UiTarget};
use std::{collections::BTreeMap, sync::Arc};
use termina::event::{KeyCode, KeyEvent, Modifiers};

pub(super) struct Bindings {
    pub registry: Arc<Ui>,
    pub application: Arc<UiTarget>,
    pub surface: Arc<UiTarget>,
}
pub(super) struct Form {
    view: BoundView,
    fields: BTreeMap<String, String>,
    busy: bool,
}
pub(super) struct Edit {
    pub name: String,
    pub label: String,
    pub editor: Editor,
    multiline: bool,
}
impl Form {
    fn new(view: BoundView) -> Self {
        let fields = view
            .view
            .elements
            .iter()
            .filter_map(|element| {
                if let UiElement::Input { name, value, .. } = element {
                    Some((name.clone(), value.clone()))
                } else {
                    None
                }
            })
            .collect();
        Self {
            view,
            fields,
            busy: false,
        }
    }
    fn text(&self) -> String {
        use std::fmt::Write as _;
        let mut text = self.view.view.title.clone();
        for element in &self.view.view.elements {
            match element {
                UiElement::Text { text: value } | UiElement::Code { text: value } => {
                    let _ = write!(text, "\n\n{value}");
                }
                UiElement::Field { label, value } => {
                    let _ = write!(text, "\n\n{label}: {value}");
                }
                UiElement::Input { name, label, .. } => {
                    let _ = write!(text, "\n\n{label}: {}", self.fields[name]);
                }
                UiElement::Button { .. } => {}
            }
        }
        super::super::terminal_text(&text)
    }
    fn menu(&self, revision: u64) -> Menu {
        let items = self
            .view
            .view
            .elements
            .iter()
            .filter_map(|element| match element {
                UiElement::Input { name, label, .. } => Some((
                    format!("Edit {}", super::super::terminal_text(label)),
                    Action::UiEdit(name.clone(), revision),
                )),
                UiElement::Button {
                    action,
                    label,
                    value,
                } => Some((
                    super::super::terminal_text(label),
                    Action::UiInvoke(self.view.actions[action].clone(), value.clone(), revision),
                )),
                _ => None,
            })
            .collect();
        Menu {
            title: super::super::terminal_text(&self.view.view.title),
            selected: 0,
            items,
        }
    }
}
impl State {
    pub(super) fn close_ui(&mut self) {
        self.ui_edit = None;
        if self.ui_form.take().is_some() {
            self.detail = None;
            self.detail_actions = None;
            self.detail_next = None;
            self.detail_previous = None;
        }
    }
    pub(super) fn refresh_ui(&mut self) {
        if let Some(form) = &self.ui_form {
            self.detail = Some(form.text());
            self.detail_offset = 0;
            self.detail_actions = (!form.busy).then(|| form.menu(self.view_revision));
        }
    }
    pub(super) fn ui_key(&mut self, key: KeyEvent) -> bool {
        let Some(edit) = &mut self.ui_edit else {
            return false;
        };
        if key.code == KeyCode::Enter && !key.modifiers.contains(Modifiers::SHIFT) {
            let mut edit = self.ui_edit.take().expect("editing UI field");
            if let Some(form) = &mut self.ui_form {
                form.fields.insert(edit.name, edit.editor.take());
            }
            self.refresh_ui();
            self.notice("Field updated · Enter opens card actions");
        } else if !edit.multiline
            && (key.code == KeyCode::Enter
                || (key.code == KeyCode::Char('j') && key.modifiers.contains(Modifiers::CONTROL)))
        {
            self.notice("This field accepts one line");
        } else if let Err(message) = edit.editor.key(key) {
            self.notice(message);
        }
        true
    }
    pub(super) fn ui_paste(&mut self, text: &str) -> bool {
        let Some(edit) = &mut self.ui_edit else {
            return false;
        };
        if !edit.multiline && text.contains(['\r', '\n']) {
            self.notice("This field accepts one line");
        } else if let Err(message) = edit.editor.insert(text) {
            self.notice(message);
        }
        true
    }
}
impl Client {
    pub(super) fn action_menu(&mut self) {
        if self.state.ui_form.is_none() {
            self.state.invalidate_detail();
        }
        let mut menu = Menu::actions();
        if self.ui.registry.has_block_renderers(&self.ui.surface) {
            menu.items.push(("Card details".into(), Action::UiCard));
        }
        for target in [&self.ui.application, &self.ui.surface] {
            if let Ok(surfaces) = self.ui.registry.surfaces(target) {
                menu.items.extend(surfaces.into_iter().map(|surface| {
                    (
                        super::super::terminal_text(&surface.title),
                        Action::UiSurface(surface.reference),
                    )
                }));
            }
        }
        self.state.menu = Some(menu);
    }
    pub(super) fn show_ui(&mut self, view: BoundView) {
        if !self.ui.registry.is_current(&view.reference) {
            return;
        }
        let form = Form::new(view);
        self.state.open_detail(form.text());
        self.state.ui_form = Some(form);
        self.state.refresh_ui();
        self.state
            .notice("Card details · Enter actions · Ctrl+Y copies displayed text");
    }
    pub(super) fn ui_surface(&mut self, reference: &UiReference) {
        let result = if self.ui.registry.matches_target(&self.ui.surface, reference)
            || self
                .ui
                .registry
                .matches_target(&self.ui.application, reference)
        {
            self.ui.registry.surface(reference)
        } else {
            Err(rsi_ui::UiError::Retired)
        };
        match result {
            Ok(view) => self.show_ui(view),
            Err(problem) => self.state.notice(problem.to_string()),
        }
    }
    pub(super) fn ui_card(&mut self) {
        let Some(block) = self.state.transcript.blocks.get(self.state.focused) else {
            self.state.notice("No focused card");
            return;
        };
        let full = block.text();
        let window = rsi_conversation::FieldWindow::text(&full, 0, rsi_ui::MAXIMUM_VIEW_BYTES)
            .expect("UI block bound");
        let result = self.ui.registry.block(
            &self.ui.surface,
            &rsi_ui::BlockInput {
                key: &block.key,
                text: &window.text,
                tool: block.tool.as_ref(),
                sources: block.sources(),
            },
        );
        match result {
            Ok(Some(view)) => self.show_ui(view),
            Ok(None) => self
                .state
                .notice("No active contribution renders this card"),
            Err(problem) => self.state.notice(problem.to_string()),
        }
    }
    pub(super) fn ui_action(&mut self, action: Action) {
        let Some(form) = &mut self.state.ui_form else {
            return;
        };
        if form.busy || !self.ui.registry.is_current(&form.view.reference) {
            return;
        }
        match action {
            Action::UiEdit(name, revision) if revision == self.state.view_revision => {
                let Some((label, multiline)) = form.view.view.elements.iter().find_map(|element| {
                    if let UiElement::Input {
                        name: key,
                        label,
                        multiline,
                        ..
                    } = element
                        && *key == name
                    {
                        Some((label.clone(), *multiline))
                    } else {
                        None
                    }
                }) else {
                    return;
                };
                let value = form.fields[&name].clone();
                let other_bytes: usize = form
                    .fields
                    .iter()
                    .filter(|(key, _)| **key != name)
                    .map(|(_, value)| value.len())
                    .sum();
                let editor = Editor {
                    cursor: value.len(),
                    text: value,
                    limit: rsi_ui::MAXIMUM_INPUT_BYTES.saturating_sub(other_bytes),
                };
                self.state.invalidate_detail();
                self.state.ui_edit = Some(Edit {
                    name,
                    label: super::super::terminal_text(&label),
                    editor,
                    multiline,
                });
                self.state
                    .notice("Edit field · Enter accepts · Esc discards this edit");
            }
            Action::UiInvoke(reference, value, revision)
                if revision == self.state.view_revision =>
            {
                if form.view.actions.get(&reference.name) != Some(&reference) {
                    return;
                }
                let input = ActionInput {
                    value,
                    fields: form.fields.clone(),
                };
                form.busy = true;
                self.state.invalidate_detail();
                self.state.refresh_ui();
                let stop = self.state.detail_stop.clone();
                let work = self.ui.registry.invoke_in_view(&reference, input, stop);
                self.spawn_detail(async move { work.await.map(Update::Ui).map_err(error) });
                self.state.notice("Working…");
            }
            _ => {}
        }
    }
    pub(super) fn ui_failed(&mut self) {
        if let Some(form) = &mut self.state.ui_form {
            form.busy = false;
        }
        self.state.refresh_ui();
    }
    pub(super) fn ui_changed(&mut self) {
        if self
            .state
            .ui_form
            .as_ref()
            .is_some_and(|form| !self.ui.registry.is_current(&form.view.reference))
        {
            self.state.invalidate_detail();
            self.state.close_ui();
            self.state.menu = None;
            self.state.notice("This UI contribution has retired");
        } else if self
            .state
            .menu
            .as_ref()
            .is_some_and(|menu| menu.title == "Actions")
        {
            self.action_menu();
        }
    }
}
