//! Application command display snapshots, separate from invocation authority.
use super::*;
use futures_util::future::BoxFuture;
use rsi_terminal_ui::scene::{ApplicationScene, Completion, Draft, Scene};
use termina::event::KeyEvent;

const BUILTINS: &[(&str, &str)] = &[
    ("help", "Commands and keyboard shortcuts"),
    (
        "history",
        "Search conversation text: /history <session-id|external:id> <query>",
    ),
    (
        "attention",
        "Pending requests, running conversations and unread activity",
    ),
    (
        "markdown",
        "Render assistant Markdown: /markdown [on|off]; process-local",
    ),
    ("plugins", "Read and refresh observed plugin status"),
    (
        "profiles",
        "Review Host leaf changes, explicit grants and source receipts",
    ),
    (
        "external",
        "Configured external agents, permissions and observed history",
    ),
    (
        "reference",
        "Frozen conversation reference: /reference [session_id]",
    ),
    (
        "login",
        "Log in: /login [deepseek|openai|openai-compatible]",
    ),
    ("model", "Select model; Ctrl+S also saves startup default"),
    ("effort", "Select reasoning effort for the current model"),
    (
        "new",
        "New session in the current workspace; retain this draft",
    ),
    (
        "resume",
        "Recent sessions or /resume <session_id>; Ctrl+Y copies ID",
    ),
    (
        "quit",
        "Exit immediately; process-local drafts are discarded",
    ),
    ("exit", "Alias for /quit"),
];
fn bounded(text: &str, maximum: usize) -> String {
    let end = text.floor_char_boundary(text.len().min(maximum));
    text[..end].into()
}
fn detail_pages(mut text: &str) -> Vec<&str> {
    let mut pages = vec![];
    while !text.is_empty() {
        let end = text.floor_char_boundary(text.len().min(8192));
        pages.push(&text[..end]);
        text = &text[end..];
    }
    pages
}
pub(super) fn visible_session_command(name: &str) -> bool {
    name != "model-selection"
}
pub(super) fn reserved(text: &str) -> bool {
    let word = text.split_whitespace().next().unwrap_or_default();
    BUILTINS
        .iter()
        .any(|(name, _)| word.strip_prefix('/') == Some(*name))
}
pub(super) fn literal(text: &str) -> bool {
    text.trim_start().starts_with("//") || (text.contains(['\r', '\n']) && reserved(text))
}
#[derive(Clone)]
struct Entry {
    name: Arc<str>,
    description: Arc<str>,
    application: bool,
    skill: Option<Arc<rsi_client::InputCompletion>>,
}
fn catalog_entry(entry: rsi_client::InputCompletion, remaining: &mut usize) -> Option<Entry> {
    let resource = entry.group != rsi_client::CompletionGroup::Command;
    let agent = entry.group == rsi_client::CompletionGroup::Agent;
    // The encoded entry includes every resource coordinate; count the separate display copy too.
    let bytes = serde_json::to_vec(&entry)
        .ok()?
        .len()
        .saturating_add(entry.name.len())
        .saturating_add(entry.description.len())
        .saturating_add(10);
    if bytes > *remaining {
        return None;
    }
    *remaining -= bytes;
    Some(Entry {
        name: entry.name.clone().into(),
        description: if resource {
            format!(
                "{} · {}",
                if agent { "Agent" } else { "Skill" },
                entry.description
            )
            .into()
        } else {
            entry.description.clone().into()
        },
        application: false,
        skill: resource.then(|| Arc::new(entry)),
    })
}
fn completion_items(entries: &[Entry], providers: bool, dollar: bool) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|entry| {
            (
                if dollar {
                    format!("${}", entry.name)
                } else if providers {
                    entry.name.to_string()
                } else {
                    entry.skill.as_ref().map_or_else(
                        || format!("/{}", entry.name),
                        |skill| skill.replacement.clone(),
                    )
                },
                bounded(&entry.description, 256),
            )
        })
        .collect::<Vec<_>>()
}
type Catalog = BoxFuture<'static, (u64, Result<(Vec<Entry>, String)>)>;
type Preview = BoxFuture<'static, (u64, Result<String>)>;
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent completion, help, dismissal and refresh flags"
)]
pub(super) struct Ui {
    pub popup: Option<Completion>,
    entries: Vec<Entry>,
    catalog: Vec<Entry>,
    pending: Option<Catalog>,
    preview: Option<Preview>,
    preview_request: Option<rsi_agent_session_protocol::SessionResourceRequest>,
    previewing: bool,
    preview_kind: &'static str,
    refresh: bool,
    failed: bool,
    generation: u64,
    session: Option<rsi_agent_session_protocol::SessionId>,
    revision: u64,
    observed: String,
    cursor: usize,
    dismissed: bool,
    token: Option<std::ops::Range<usize>>,
    pub help: bool,
    help_filter: editor::Editor,
    detail: Option<String>,
    offset: usize,
    detail_area: ratatui::layout::Rect,
    page: usize,
    selected: usize,
    pub diagnostic: String,
}
impl Ui {
    pub fn presented(&mut self, view: &rsi_terminal_ui::render::View) {
        if self.help
            && self.detail.is_some()
            && view.choice_revision == self.revision
            && view.area.width > 0
        {
            self.detail_area = view.area;
            self.offset = self.offset.min(self.maximum_detail_offset());
        }
    }
    fn maximum_detail_offset(&self) -> usize {
        if self.detail_area.width == 0 {
            return 8192;
        }
        let text = self
            .detail
            .as_deref()
            .and_then(|text| detail_pages(text).get(self.page).copied())
            .unwrap_or_default();
        ratatui::widgets::Paragraph::new(rsi_terminal_ui::terminal_text(text))
            .wrap(ratatui::widgets::Wrap { trim: false })
            .line_count(self.detail_area.width)
            .saturating_sub(usize::from(self.detail_area.height))
    }
    pub fn hide(&mut self) {
        if self.token.is_some() {
            self.generation += 1;
        }
        self.refresh = false;
        self.popup = None;
        self.token = None;
        self.dismissed = true;
    }
    pub fn invalidate(&mut self) {
        if (self.token.is_some() && !self.dismissed) || self.help {
            self.generation += 1;
            self.refresh = true;
        }
    }
    pub fn update(
        &mut self,
        editor: &editor::Editor,
        controller: Option<&Arc<rsi_client::SessionController>>,
    ) {
        let session = controller.map(|controller| controller.session_id());
        if session != self.session.as_ref() {
            self.session = session.cloned();
            self.failed = false;
            self.generation += 1;
            self.pending = None;
            self.preview = None;
            self.preview_request = None;
            self.previewing = false;
            self.detail = None;
            self.help = false;
            self.catalog.clear();
            self.refresh = true;
            self.rebuild();
        }
        if let Some(controller) = controller
            && let Some(request) = self.preview_request.take()
        {
            let controller = controller.clone();
            let generation = self.generation;
            self.preview = Some(Box::pin(async move {
                let result = controller
                    .read_resource(request)
                    .await
                    .map_err(error)
                    .and_then(|snapshot| {
                        if let rsi_agent_session_protocol::SessionResourceValue::Read {
                            resource,
                            text,
                        } = &snapshot.response().value
                        {
                            Ok(format!(
                                "{}\n{}\n\n{}",
                                resource.name, resource.source, text
                            ))
                        } else {
                            Err(error("Invalid resource preview"))
                        }
                    });
                (generation, result)
            }));
        }
        let changed = self.observed != editor.text() || self.cursor != editor.cursor();
        if changed && self.failed {
            self.invalidate();
        }
        if changed {
            self.observed = editor.text().into();
            self.cursor = editor.cursor();
            self.dismissed = false;
        }
        let was_open = self.token.is_some() && !self.dismissed;
        if changed {
            self.rebuild();
        }
        if !was_open && self.token.is_some() && !self.dismissed {
            self.invalidate();
        }
        if self.refresh
            && self.pending.is_none()
            && let Some(controller) = controller
        {
            self.refresh = false;
            let controller = controller.clone();
            let generation = self.generation;
            self.pending = Some(Box::pin(async move {
                let reserved: Vec<_> = BUILTINS.iter().map(|(name, _)| *name).collect();
                let result = controller
                    .completion_catalog(&reserved)
                    .await
                    .map_err(error)
                    .map(|(entries, mut notice)| {
                        let mut remaining = 64 * 1024 - 1024;
                        let mut omitted = false;
                        let entries = entries
                            .into_iter()
                            .filter_map(|entry| {
                                let value = catalog_entry(entry, &mut remaining);
                                omitted |= value.is_none();
                                value
                            })
                            .collect();
                        if omitted {
                            notice.push_str(
                                " Some completion entries exceed the 64 KiB display limit.",
                            );
                        }
                        (entries, notice)
                    });
                (generation, result)
            }));
        }
    }
    pub async fn next(&mut self) {
        let (generation, result) = tokio::select! {
            result = async { match &mut self.pending { Some(work) => work.await, None => std::future::pending().await } } => result,
            (generation, result) = async { match &mut self.preview { Some(work) => work.await, None => std::future::pending().await } } => {
                self.preview = None;
                if generation == self.generation && self.help && self.previewing {
                    self.detail = Some(result.unwrap_or_else(|error| format!("Resource unavailable: {error}. Close and preview again to retry.")));
                    self.revision += 1;
                }
                return;
            }
        };
        self.pending = None;
        if generation != self.generation || ((self.token.is_none() || self.dismissed) && !self.help)
        {
            return;
        }
        self.revision += 1;
        match result {
            Ok((entries, notice)) => {
                self.failed = false;
                self.catalog = entries;
                self.diagnostic = notice;
            }
            Err(error) => {
                self.failed = true;
                self.catalog.clear();
                self.diagnostic = format!("Commands unavailable: {error}; edit or reopen to retry");
            }
        }
        if self.help {
            self.selected = self
                .selected
                .min(self.help_entries().len().saturating_sub(1));
        }
        self.rebuild();
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Cursor-local commands, skills, and agents share one mutually exclusive popup decision"
    )]
    fn rebuild(&mut self) {
        self.token = None;
        let dollar =
            rsi_agent_workspace_context::skill_input::dollar_token_at(&self.observed, self.cursor);
        let agent =
            rsi_agent_workspace_context::skill_input::at_token_at(&self.observed, self.cursor);
        if self.help
            || self.dismissed
            || (dollar.is_none()
                && agent.is_none()
                && (!self.observed.starts_with('/')
                    || self.observed.starts_with("//")
                    || self.observed.contains(['\n', '\r'])))
        {
            self.popup = None;
            return;
        }
        let end = self
            .observed
            .find(char::is_whitespace)
            .unwrap_or(self.observed.len());
        let providers = dollar.is_none()
            && agent.is_none()
            && self.observed.starts_with("/login ")
            && self.cursor >= 7;
        let range = if let Some(token) = agent.as_ref().or(dollar.as_ref()) {
            token.range.clone()
        } else if providers {
            let start = self.observed[..self.cursor]
                .rfind(char::is_whitespace)
                .map_or(7, |i| i + 1);
            let end = self.observed[self.cursor..]
                .find(char::is_whitespace)
                .map_or(self.observed.len(), |i| self.cursor + i);
            start..end
        } else {
            if self.cursor > end {
                self.popup = None;
                return;
            }
            0..end
        };
        if self.cursor < range.start {
            self.popup = None;
            return;
        }
        let filter = self.observed[range.clone()].trim_start_matches(['/', '$', '@']);
        let mut entries = if providers {
            ["deepseek", "openai", "openai-compatible"]
                .iter()
                .map(|name| Entry {
                    name: (*name).into(),
                    description: "Provider".into(),
                    application: true,
                    skill: None,
                })
                .collect::<Vec<_>>()
        } else {
            self.all()
                .into_iter()
                .filter(|entry| {
                    let group = entry.skill.as_ref().map(|entry| entry.group);
                    if agent.is_some() {
                        group == Some(rsi_client::CompletionGroup::Agent)
                    } else if dollar.is_some() {
                        group == Some(rsi_client::CompletionGroup::Skill)
                    } else {
                        group != Some(rsi_client::CompletionGroup::Agent)
                    }
                })
                .collect()
        };
        entries.retain(|entry| {
            rsi_client::completion_rank(&entry.name, filter).is_some()
                && (!filter.is_empty() || entry.name.as_ref() != "exit")
        });
        if !filter.is_empty() {
            entries.sort_by(|left, right| {
                let key = |entry: &Entry| {
                    (
                        entry.skill.is_some(),
                        rsi_client::completion_rank(&entry.name, filter).unwrap_or(3),
                    )
                };
                key(left)
                    .cmp(&key(right))
                    .then_with(|| left.name.cmp(&right.name))
            });
        }
        let items = completion_items(&entries, providers, dollar.is_some());
        let selected = self
            .popup
            .as_ref()
            .filter(|p| p.items == items)
            .map_or(0, |p| p.selected);
        if self.popup.as_ref().is_none_or(|p| p.items != items) {
            self.revision += 1;
        }
        self.popup = (!items.is_empty()).then_some(Completion {
            revision: self.revision,
            items,
            selected,
        });
        self.entries = entries;
        self.token = Some(range);
    }
    fn all(&self) -> Vec<Entry> {
        BUILTINS
            .iter()
            .map(|(name, description)| Entry {
                name: (*name).into(),
                description: (*description).into(),
                application: true,
                skill: None,
            })
            .chain(self.catalog.iter().cloned())
            .collect()
    }
    /// Returns true when the key was consumed. Enter may leave submission to caller.
    #[expect(
        clippy::too_many_lines,
        reason = "One exhaustive projection keeps related state transitions and ownership visible together"
    )]
    pub fn key(&mut self, key: KeyEvent, editor: &mut editor::Editor) -> bool {
        let control = key.modifiers.contains(Modifiers::CONTROL);
        if self.help && self.previewing && key.code == KeyCode::Escape {
            self.preview = None;
            self.preview_request = None;
            self.previewing = false;
            self.help = false;
            self.detail = None;
            self.dismissed = false;
            self.revision += 1;
            self.rebuild();
            return true;
        }
        if self.help {
            self.revision += 1;
            if key.code == KeyCode::Escape {
                if self.detail.take().is_none() {
                    self.help = false;
                    self.dismissed = true;
                }
            } else if control && key.code == KeyCode::Char('c') {
                self.help = false;
                self.detail = None;
                self.dismissed = true;
            } else if let Some(detail) = &self.detail {
                match key.code {
                    KeyCode::PageDown => {
                        self.page =
                            (self.page + 1).min(detail_pages(detail).len().saturating_sub(1));
                        self.offset = 0;
                    }
                    KeyCode::PageUp => {
                        self.page = self.page.saturating_sub(1);
                        self.offset = 0;
                    }
                    KeyCode::Down => {
                        self.offset = self
                            .offset
                            .saturating_add(1)
                            .min(self.maximum_detail_offset());
                    }
                    KeyCode::Up => self.offset = self.offset.saturating_sub(1),
                    _ => {}
                }
            } else if key.code == KeyCode::Enter {
                if let Some(entry) = self.help_entries().get(self.selected) {
                    self.detail = Some(format!("/{}\n{}", entry.name, entry.description));
                    self.offset = 0;
                    self.page = 0;
                }
            } else if key.code == KeyCode::Up {
                self.selected = self.selected.saturating_sub(1);
            } else if key.code == KeyCode::Down {
                self.selected =
                    (self.selected + 1).min(self.help_entries().len().saturating_sub(1));
            } else {
                let _ = self.help_filter.key(key);
                self.selected = 0;
            }
            return true;
        }
        if key.code == KeyCode::Tab && self.popup.is_none() {
            self.dismissed = false;
            self.rebuild();
            if self.token.is_some() {
                self.invalidate();
            }
        }
        if key.code == KeyCode::Escape && self.token.is_some() {
            self.hide();
            return true;
        }
        let Some(popup) = &mut self.popup else {
            return key.code == KeyCode::Tab && self.token.is_some();
        };
        if key.code == KeyCode::Function(2) {
            self.preview_kind = if self.entries[popup.selected]
                .skill
                .as_ref()
                .is_some_and(|entry| entry.group == rsi_client::CompletionGroup::Agent)
            {
                "Agent"
            } else {
                "Skill"
            };
            if let Some(request) = self.entries[popup.selected]
                .skill
                .as_ref()
                .and_then(|skill| skill.resource.clone())
            {
                self.preview_request = Some(request);
                self.previewing = true;
                self.help = true;
                self.detail = Some("Loading resource…".into());
                self.page = 0;
                self.offset = 0;
                self.revision += 1;
            }
            return true;
        }
        match key.code {
            KeyCode::Up => {
                popup.selected = popup.selected.saturating_sub(1);
                true
            }
            KeyCode::Down => {
                popup.selected = (popup.selected + 1).min(popup.items.len().saturating_sub(1));
                true
            }
            KeyCode::Tab | KeyCode::Enter
                if !key.modifiers.contains(Modifiers::SHIFT) && !control =>
            {
                let entry = &self.entries[popup.selected];
                let range = self.token.clone().expect("visible token");
                let replacement = &popup.items[popup.selected].0;
                let complete = &editor.text()[range.clone()] == replacement;
                if key.code == KeyCode::Enter && complete {
                    return false;
                }
                let mid_cursor = editor.cursor() != editor.text().len();
                if let Err(error) = editor.replace_range(range, replacement) {
                    self.diagnostic = error.into();
                    return true;
                }
                // Application execution still passes exact argument/cursor validation.
                key.code == KeyCode::Tab || !entry.application || mid_cursor
            }
            _ => false,
        }
    }
    pub fn paste(&mut self, text: &str) -> bool {
        if !self.help {
            return false;
        }
        self.revision += 1;
        if self.detail.is_none() {
            if let Err(problem) = self.help_filter.insert(text) {
                self.diagnostic = problem.into();
            }
            self.selected = 0;
        }
        true
    }
    pub fn open_help(&mut self) {
        self.previewing = false;
        self.revision += 1;
        self.page = 0;
        self.offset = 0;
        self.selected = 0;
        self.help = true;
        self.popup = None;
        self.help_filter = editor::Editor::with_text(String::new(), 512);
        self.detail = None;
        self.invalidate();
    }
    fn help_entries(&self) -> Vec<Entry> {
        self.all()
            .into_iter()
            .filter(|e| e.name.contains(self.help_filter.text()))
            .collect()
    }
    pub fn scene(&self) -> std::result::Result<Scene, &'static str> {
        let detail = self.detail.as_ref().map(|text| {
            detail_pages(text)
                .get(self.page)
                .copied()
                .unwrap_or_default()
                .to_owned()
        });
        Ok(Scene::from(ApplicationScene {
            revision:self.revision,
            title:if self.previewing {format!("RSI · Skill · page {}",self.page+1)} else if detail.is_some() {format!("RSI · Help · page {}",self.page+1)} else {"RSI · Help".into()},
            explanation:if self.previewing {"Preview only · Esc returns to your draft"} else {"Enter sends · Shift+Enter / Ctrl+J line · Ctrl+P actions · Ctrl+R recall · Ctrl+Y copy ID · Ctrl+C cancel turn"}.into(),
            items:if detail.is_some() {vec![]} else {self.help_entries().iter().map(|e|format!("/{} · {}",e.name,bounded(&e.description,256))).collect()},
            selected:if detail.is_some() {0} else {self.selected},
            field:detail.is_none().then(||"Filter commands · Enter detail".into()),
            detail, detail_offset:self.offset,
            input:Draft::capture(&self.help_filter)?,
            hint:"Esc back · ↑/↓ scroll · PgUp/Dn page".into(),
            status:self.diagnostic.clone(), ..ApplicationScene::default()
        }))
    }
    pub fn mouse(
        &mut self,
        mouse: termina::event::MouseEvent,
        view: &rsi_terminal_ui::render::View,
    ) -> bool {
        if self.help {
            if view.choice_revision != self.revision
                || !view.area.contains((mouse.column, mouse.row).into())
            {
                return true;
            }
            if self.detail.is_some() {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.offset = self.offset.saturating_sub(3),
                    MouseEventKind::ScrollDown => {
                        self.offset = self.offset.saturating_add(3).min(view.scroll_max);
                    }
                    _ => {}
                }
                return true;
            }
            match mouse.kind {
                MouseEventKind::ScrollUp => self.selected = self.selected.saturating_sub(1),
                MouseEventKind::ScrollDown => {
                    self.selected =
                        (self.selected + 1).min(self.help_entries().len().saturating_sub(1));
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) = view.choice_at(mouse.column, mouse.row)
                        && index < self.help_entries().len()
                    {
                        self.selected = index;
                    }
                }
                _ => {}
            }
            self.revision += 1;
            return true;
        }
        let Some(popup) = &mut self.popup else {
            return false;
        };
        let Some((revision, area, from)) = view.completion else {
            return false;
        };
        if revision != popup.revision || !area.contains((mouse.column, mouse.row).into()) {
            return false;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => popup.selected = popup.selected.saturating_sub(1),
            MouseEventKind::ScrollDown => {
                popup.selected = (popup.selected + 1).min(popup.items.len() - 1);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                popup.selected =
                    (from + usize::from(mouse.row - area.y)).min(popup.items.len() - 1);
            }
            _ => {}
        }
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn skill(name: &str, replacement: &str) -> Entry {
        Entry {
            name: name.into(),
            description: "Review a change".into(),
            application: false,
            skill: Some(Arc::new(rsi_client::InputCompletion {
                name: name.into(),
                description: "Review a change".into(),
                replacement: replacement.into(),
                group: rsi_client::CompletionGroup::Skill,
                resource: Some(rsi_agent_session_protocol::SessionResourceRequest::Read {
                    source: rsi_agent_session_protocol::ContributionId::new("rsi.workspace-skills")
                        .unwrap(),
                    id: name.into(),
                }),
            })),
        }
    }
    #[test]
    fn dollar_completion_is_cursor_local_and_preserves_multiline_text_and_undo() {
        let mut ui = Ui {
            catalog: vec![skill("review", "/review"), skill("model", "/skill model")],
            ..Ui::default()
        };
        let original = "first line\n请用 $rev suffix";
        let mut editor = editor::Editor::with_text(original.into(), 1024);
        for _ in 0.." suffix".chars().count() {
            editor.key(KeyCode::Left.into()).unwrap();
        }
        let cursor = editor.cursor();
        ui.update(&editor, None);
        assert_eq!(ui.popup.as_ref().unwrap().items.len(), 1);
        assert!(ui.key(KeyCode::Tab.into(), &mut editor));
        assert_eq!(editor.text(), "first line\n请用 $review suffix");
        editor
            .key(KeyEvent {
                code: KeyCode::Char('z'),
                modifiers: Modifiers::CONTROL,
                kind: termina::event::KeyEventKind::Press,
                state: termina::event::KeyEventState::empty(),
            })
            .unwrap();
        assert_eq!(editor.text(), original);
        assert_eq!(editor.cursor(), cursor);
        for text in ["`$rev`", "\\$rev", "[label](https://host/$rev)"] {
            let editor = editor::Editor::with_text(text.into(), 1024);
            ui.update(&editor, None);
            assert!(ui.popup.is_none());
        }
    }
    #[test]
    fn skill_preview_escape_and_collision_insert_preserve_the_draft_and_cursor() {
        let mut ui = Ui {
            catalog: vec![skill("model", "/skill model")],
            ..Ui::default()
        };
        let mut editor = editor::Editor::with_text("/mod 参数".into(), 1024);
        editor.key(KeyCode::Home.into()).unwrap();
        for _ in 0..4 {
            editor.key(KeyCode::Right.into()).unwrap();
        }
        ui.update(&editor, None);
        assert_eq!(
            ui.entries
                .iter()
                .filter(|entry| entry.name.as_ref() == "model")
                .count(),
            2
        );
        let selected = ui
            .entries
            .iter()
            .position(|entry| entry.skill.is_some())
            .unwrap();
        ui.popup.as_mut().unwrap().selected = selected;
        assert!(ui.key(KeyCode::Function(2).into(), &mut editor));
        assert!(ui.previewing);
        assert!(ui.preview_request.is_some());
        assert_eq!(editor.text(), "/mod 参数");
        assert!(ui.key(KeyCode::Escape.into(), &mut editor));
        assert!(!ui.help && ui.preview_request.is_none());
        assert_eq!(editor.cursor(), 4);
        let selected = ui
            .entries
            .iter()
            .position(|entry| entry.skill.is_some())
            .unwrap();
        ui.popup.as_mut().unwrap().selected = selected;
        assert!(ui.key(KeyCode::Tab.into(), &mut editor));
        assert_eq!(editor.text(), "/skill model 参数");
        assert_eq!(editor.cursor(), "/skill model".len());
    }
    #[test]
    fn skill_display_budget_and_filter_clones_keep_payload_ownership_bounded() {
        let mut remaining = 64 * 1024 - 1024;
        let entry = rsi_client::InputCompletion {
            name: "large".into(),
            description: "x".repeat(40 * 1024),
            replacement: "/large".into(),
            group: rsi_client::CompletionGroup::Skill,
            resource: None,
        };
        assert!(catalog_entry(entry, &mut remaining).is_none());
        let retained = skill("guide", "/guide");
        let clone = retained.clone();
        assert!(Arc::ptr_eq(&retained.description, &clone.description));
        assert!(Arc::ptr_eq(
            retained.skill.as_ref().unwrap(),
            clone.skill.as_ref().unwrap()
        ));
    }
    #[test]
    fn empty_query_starts_with_help_and_the_exit_alias_remains_searchable() {
        let mut ui = Ui::default();
        let editor = editor::Editor::with_text("/".into(), 1024);
        ui.update(&editor, None);
        let popup = ui.popup.as_ref().unwrap();
        assert_eq!(popup.items[popup.selected].0, "/help");
        assert!(popup.items.iter().all(|(name, _)| name != "/exit"));
        let mut editor = editor::Editor::with_text("/ex".into(), 1024);
        ui.update(&editor, None);
        assert!(!ui.key(KeyCode::Enter.into(), &mut editor));
        assert_eq!(editor.text(), "/exit");
        assert!(matches!(
            setup::command(editor.text()),
            Some(setup::Command::Quit)
        ));
    }
    #[tokio::test]
    async fn late_catalog_results_cannot_replace_reopened_or_invalidated_popups() {
        let mut ui = Ui::default();
        let editor = editor::Editor::with_text("/".into(), 1024);
        ui.update(&editor, None);
        let old = ui.generation;
        let (send, receive) = tokio::sync::oneshot::channel();
        ui.pending = Some(Box::pin(async move {
            receive.await.unwrap();
            (
                old,
                Ok((
                    vec![Entry {
                        name: "old-session-command".into(),
                        description: "obsolete".into(),
                        application: false,
                        skill: None,
                    }],
                    String::new(),
                )),
            )
        }));
        ui.invalidate();
        ui.invalidate();
        ui.update(&editor, None);
        assert!(ui.pending.is_some());
        assert!(ui.refresh);
        send.send(()).unwrap();
        ui.next().await;
        assert!(ui.catalog.is_empty());
        assert!(ui.refresh);
        assert!(ui.pending.is_none());
        let generation = ui.generation;
        ui.pending = Some(Box::pin(
            async move { (generation, Err(error("read denied"))) },
        ));
        ui.next().await;
        assert!(ui.diagnostic.contains("read denied"));
        assert_eq!(ui.popup.as_ref().unwrap().items.len(), BUILTINS.len() - 1);
        assert!(
            !ui.popup
                .as_ref()
                .unwrap()
                .items
                .iter()
                .any(|(name, _)| name.contains("old-session"))
        );
    }
    #[tokio::test]
    async fn advisory_notice_does_not_refetch_after_each_keystroke_but_errors_retry() {
        let mut ui = Ui::default();
        let editor = editor::Editor::with_text("/".into(), 1024);
        ui.update(&editor, None);
        ui.refresh = false;
        let generation = ui.generation;
        ui.pending = Some(Box::pin(async move {
            (
                generation,
                Ok((vec![], "Skills unavailable; refresh when ready".into())),
            )
        }));
        ui.next().await;
        ui.update(&editor::Editor::with_text("/h".into(), 1024), None);
        assert_eq!(
            ui.generation, generation,
            "an advisory response is a successful catalog"
        );
        assert!(!ui.refresh);
        ui.pending = Some(Box::pin(
            async move { (generation, Err(error("read denied"))) },
        ));
        ui.next().await;
        ui.update(&editor::Editor::with_text("/he".into(), 1024), None);
        assert!(ui.generation > generation);
        assert!(ui.refresh);
    }
    #[tokio::test]
    async fn failed_help_refresh_clamps_selection_to_remaining_application_commands() {
        let mut ui = Ui::default();
        ui.open_help();
        ui.selected = 100;
        let generation = ui.generation;
        ui.pending = Some(Box::pin(async move {
            (generation, Err(error("permission revoked")))
        }));
        ui.next().await;
        let scene = ui.scene().unwrap();
        Scene::decode(&scene.encode().unwrap())
            .unwrap()
            .render(42, 12)
            .unwrap();
        assert_eq!(ui.selected, BUILTINS.len() - 1);
        assert!(ui.diagnostic.contains("permission revoked"));
    }
    #[tokio::test]
    async fn escape_before_discovery_and_tab_reopen_fence_the_old_request() {
        let mut ui = Ui::default();
        let mut editor = editor::Editor::with_text("/unknown".into(), 1024);
        ui.update(&editor, None);
        let generation = ui.generation;
        assert!(ui.popup.is_none());
        ui.pending = Some(Box::pin(async move {
            (
                generation,
                Ok((
                    vec![Entry {
                        name: "unknown-old".into(),
                        description: "stale".into(),
                        application: false,
                        skill: None,
                    }],
                    String::new(),
                )),
            )
        }));
        assert!(ui.key(KeyCode::Escape.into(), &mut editor));
        ui.update(&editor, None);
        assert!(ui.key(KeyCode::Tab.into(), &mut editor));
        assert!(ui.refresh);
        ui.next().await;
        assert!(ui.catalog.is_empty());
        assert!(ui.refresh);
    }
    #[test]
    fn help_pages_bound_unicode_details_and_scroll_without_mutating_draft() {
        let mut ui = Ui::default();
        ui.open_help();
        ui.detail = Some("界".repeat(10_000));
        ui.key(KeyCode::PageDown.into(), &mut editor::Editor::default());
        let rsi_terminal_ui::scene::Surface::Application(scene) = ui.scene().unwrap().surface
        else {
            panic!("help")
        };
        assert_eq!(ui.page, 1);
        assert!(scene.detail.as_ref().unwrap().len() <= 8192);
        Scene::decode(&Scene::from(*scene).encode().unwrap())
            .unwrap()
            .render(42, 12)
            .unwrap();
    }
    #[test]
    fn help_scroll_stops_at_the_last_wrapped_page() {
        use std::fmt::Write as _;
        let mut ui = Ui::default();
        ui.open_help();
        let mut detail = String::new();
        for n in 0..20 {
            writeln!(
                &mut detail,
                "line-{n:02} 界界界界界界界界界界界界界界界界界界"
            )
            .unwrap();
        }
        ui.detail = Some(detail);
        let mut editor = editor::Editor::with_text("preserve draft".into(), 1024);
        for _ in 0..100 {
            ui.key(KeyCode::Down.into(), &mut editor);
        }
        let (buffer, view) = ui.scene().unwrap().render(42, 12).unwrap();
        let text: String = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            text.contains("line-19"),
            "Down scrolled past all content: {text}"
        );
        if let Some(directory) = std::env::var_os("RSI_TUI_VISUAL_DIR") {
            std::fs::create_dir_all(&directory).unwrap();
            let cells: Vec<_> = buffer.content.iter().enumerate().map(|(i, cell)| serde_json::json!({
                "x": i % 42, "y": i / 42, "text": cell.symbol(),
                "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg), "bold": false,
            })).collect();
            std::fs::write(
                std::path::Path::new(&directory).join("help-scrolled-42x12.json"),
                serde_json::to_vec(&serde_json::json!({"width":42,"height":12,"cells":cells}))
                    .unwrap(),
            )
            .unwrap();
        }
        ui.presented(&view);
        let last = ui.offset;
        assert!(last > 0);
        ui.key(KeyCode::Down.into(), &mut editor);
        assert_eq!(ui.offset, last);
        ui.key(KeyCode::Up.into(), &mut editor);
        assert_eq!(ui.offset, last - 1, "Up responds immediately at the bottom");
        let (_, resized) = ui.scene().unwrap().render(80, 40).unwrap();
        ui.presented(&resized);
        assert_eq!(ui.offset, 0, "the complete text fits after enlargement");
        assert_eq!(editor.text(), "preserve draft");
    }

    #[test]
    fn help_paste_preserves_composer_and_provider_completion_never_submits_on_tab() {
        let mut ui = Ui::default();
        let mut editor = editor::Editor::with_text("/login d".into(), 1024);
        ui.update(&editor, None);
        assert_eq!(ui.popup.as_ref().unwrap().items[0].0, "deepseek");
        assert!(ui.key(KeyCode::Tab.into(), &mut editor));
        assert_eq!(editor.text(), "/login deepseek");
        ui.open_help();
        assert!(ui.paste("login"));
        assert_eq!(ui.help_entries().len(), 1);
        assert_eq!(editor.text(), "/login deepseek");
    }
    #[test]
    fn live_completion_filters_preserves_suffix_and_escape_reopens() {
        let mut ui = Ui::default();
        let mut editor = editor::Editor::with_text("/".into(), 1024);
        ui.update(&editor, None);
        assert_eq!(ui.popup.as_ref().unwrap().items.len(), BUILTINS.len() - 1);
        editor.replace_text("/log deepseek").unwrap();
        for _ in 0..9 {
            editor.key(KeyCode::Left.into()).unwrap();
        }
        ui.update(&editor, None);
        assert_eq!(ui.popup.as_ref().unwrap().items[0].0, "/login");
        assert!(ui.key(KeyCode::Tab.into(), &mut editor));
        assert_eq!(editor.text(), "/login deepseek");
        ui.update(&editor, None);
        ui.key(KeyCode::Escape.into(), &mut editor);
        ui.update(&editor, None);
        assert!(ui.popup.is_none());
        editor.replace_text("//login").unwrap();
        ui.update(&editor, None);
        assert!(ui.popup.is_none());
        assert!(literal("/model\nnotes"));
        assert!(literal("//model"));
        assert!(!literal("/plan\nnotes"));
    }
}
