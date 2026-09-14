//! Application command display snapshots, separate from invocation authority.
use super::*;
use futures_util::future::BoxFuture;
use rsi_terminal_ui::scene::{ApplicationScene, Completion, Draft, Scene};
use termina::event::KeyEvent;

const BUILTINS: &[(&str, &str)] = &[
    ("help", "Commands and keyboard shortcuts"),
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
    name: String,
    description: String,
    application: bool,
}
type Catalog = BoxFuture<'static, (u64, Result<Vec<Entry>>)>;
#[derive(Default)]
pub(super) struct Ui {
    pub popup: Option<Completion>,
    entries: Vec<Entry>,
    catalog: Vec<Entry>,
    pending: Option<Catalog>,
    refresh: bool,
    generation: u64,
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
        let changed = self.observed != editor.text() || self.cursor != editor.cursor();
        if changed && !self.diagnostic.is_empty() {
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
                let result = controller.commands().await.map_err(error).map(|view| {
                    let mut remaining: usize = 64 * 1024 - 1024;
                    view.commands()
                        .iter()
                        .filter(|command| {
                            visible_session_command(command.name())
                                && !BUILTINS.iter().any(|(name, _)| *name == command.name())
                        })
                        .filter_map(|command| {
                            if command.name().len() > remaining {
                                return None;
                            }
                            let description =
                                bounded(command.description(), remaining - command.name().len());
                            remaining -= description.len() + command.name().len();
                            Some(Entry {
                                name: command.name().into(),
                                description,
                                application: false,
                            })
                        })
                        .collect()
                });
                (generation, result)
            }));
        }
    }
    pub async fn next(&mut self) {
        let (generation, result) = match &mut self.pending {
            Some(work) => work.await,
            None => std::future::pending().await,
        };
        self.pending = None;
        if generation != self.generation || ((self.token.is_none() || self.dismissed) && !self.help)
        {
            return;
        }
        self.revision += 1;
        match result {
            Ok(entries) => {
                self.catalog = entries;
                self.diagnostic.clear();
            }
            Err(error) => {
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
    fn rebuild(&mut self) {
        self.token = None;
        if self.help
            || self.dismissed
            || !self.observed.starts_with('/')
            || self.observed.starts_with("//")
            || self.observed.contains(['\n', '\r'])
        {
            self.popup = None;
            return;
        }
        let end = self
            .observed
            .find(char::is_whitespace)
            .unwrap_or(self.observed.len());
        let providers = self.observed.starts_with("/login ") && self.cursor >= 7;
        let range = if providers {
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
        let filter = self.observed[range.clone()].trim_start_matches('/');
        let mut entries = if providers {
            ["deepseek", "openai", "openai-compatible"]
                .iter()
                .map(|name| Entry {
                    name: (*name).into(),
                    description: "Provider".into(),
                    application: true,
                })
                .collect::<Vec<_>>()
        } else {
            self.all()
        };
        entries.retain(|entry| {
            entry.name.contains(filter) && (!filter.is_empty() || entry.name != "exit")
        });
        if !filter.is_empty() {
            entries.sort_by_key(|entry| {
                (
                    if entry.name == filter {
                        0
                    } else if entry.name.starts_with(filter) {
                        1
                    } else {
                        2
                    },
                    entry.name.clone(),
                )
            });
        }
        let items = entries
            .iter()
            .map(|entry| {
                (
                    if providers {
                        entry.name.clone()
                    } else {
                        format!("/{}", entry.name)
                    },
                    bounded(&entry.description, 256),
                )
            })
            .collect::<Vec<_>>();
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
            })
            .chain(self.catalog.iter().cloned())
            .collect()
    }
    /// Returns true when the key was consumed. Enter may leave submission to caller.
    pub fn key(&mut self, key: KeyEvent, editor: &mut editor::Editor) -> bool {
        let control = key.modifiers.contains(Modifiers::CONTROL);
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
                let mut text = editor.text().to_owned();
                text.replace_range(range, replacement);
                let _ = editor.replace_text(&text);
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
            title:if detail.is_some() {format!("RSI · Help · page {}",self.page+1)} else {"RSI · Help".into()},
            explanation:"Enter sends · Shift+Enter / Ctrl+J line · Ctrl+P actions · Ctrl+R recall · Ctrl+Y copy ID · Ctrl+C cancel turn".into(),
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
                Ok(vec![Entry {
                    name: "old-session-command".into(),
                    description: "obsolete".into(),
                    application: false,
                }]),
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
        assert_eq!(ui.popup.as_ref().unwrap().items.len(), 7);
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
        assert_eq!(ui.selected, 7);
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
                Ok(vec![Entry {
                    name: "unknown-old".into(),
                    description: "stale".into(),
                    application: false,
                }]),
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
        assert_eq!(ui.popup.as_ref().unwrap().items.len(), 7);
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
