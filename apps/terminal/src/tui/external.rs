use super::*;
use futures_util::future::BoxFuture;
use rsi_acp_protocol::{
    observation::ConversationId,
    service::{ExternalConversations, ExternalConversationsContract, Setup},
};
use rsi_client::{ExternalCommand, ExternalController};
use rsi_terminal_ui::scene::{ApplicationScene, Draft, Scene};
#[path = "attention.rs"]
mod attention;
mod drafts;

enum Event {
    Attention(Vec<(String, Choice)>, bool),
    Native(
        rsi_navigation_api::attention::Position,
        Option<rsi_session_protocol::ActivityRequest>,
    ),
    Catalog(Vec<(String, Choice)>),
    Attached(Arc<ExternalController>, Option<String>),
    Done,
    Submitted(drafts::Attempt, std::result::Result<(), String>),
    Source(rsi_conversation::ExternalSource, usize, Vec<u8>),
}
#[derive(Clone)]
enum Choice {
    Attention,
    AttentionOpen(
        rsi_navigation_api::attention::Position,
        Option<rsi_navigation_api::attention::Target>,
    ),
    Open(ConversationId),
    Start(String, ConversationId),
    Control(ExternalCommand),
    Catalog(Option<ConversationId>),
    Source(rsi_conversation::ExternalSource, usize),
}
pub(super) struct Ui {
    attention: Option<rsi_navigation_api::attention::Client>,
    attention_menu: bool,
    deferred_open: Option<Choice>,
    pub native: Option<SessionId>,
    pub focus: Option<(
        rsi_navigation_api::attention::Position,
        Option<rsi_session_protocol::ActivityRequest>,
    )>,
    permission_focus: Option<rsi_navigation_api::attention::Target>,
    pub active: bool,
    service: Option<Arc<dyn ExternalConversations>>,
    execution: rsi_meta::Execution,
    controller: Option<Arc<ExternalController>>,
    changes: Option<tokio::sync::watch::Receiver<u64>>,
    work: Option<BoxFuture<'static, std::result::Result<Event, String>>>,
    items: Vec<(String, Choice)>,
    selected: usize,
    menu: bool,
    editor: editor::Editor,
    drafts: drafts::Drafts,
    detail: Option<String>,
    next_source: Option<Choice>,
    offset: usize,
    status: String,
    revision: u64,
}
impl Ui {
    pub fn new(context: &rsi_meta::Context) -> Self {
        Self {
            attention: context
                .lookup_local::<rsi_api_protocol::ApiClientContract>()
                .and_then(|api| rsi_navigation_api::attention::Client::new(api).ok()),
            attention_menu: false,
            deferred_open: None,
            native: None,
            focus: None,
            permission_focus: None,
            active: false,
            service: context.lookup_local::<ExternalConversationsContract>(),
            execution: context.runtime().execution().clone(),
            controller: None,
            changes: None,
            work: None,
            items: vec![],
            selected: 0,
            menu: true,
            editor: external_editor(String::new()),
            drafts: drafts::Drafts::default(),
            detail: None,
            next_source: None,
            offset: 0,
            status: String::new(),
            revision: 0,
        }
    }
    fn uncertain(&self) -> bool {
        self.drafts
            .bound()
            .is_some_and(|id| self.drafts.uncertain(id))
    }
    fn edit(
        &mut self,
        operation: impl FnOnce(&mut editor::Editor) -> std::result::Result<(), &'static str>,
    ) {
        let Some(id) = self.drafts.bound().cloned() else {
            return;
        };
        match operation(&mut self.editor) {
            Ok(()) => self.drafts.edit(&id, self.editor.text()),
            Err(error) => self.status = error.into(),
        }
    }

    pub fn open(&mut self) {
        self.active = true;
        if self.work.is_some() {
            self.detach();
            self.deferred_open = Some(Choice::Catalog(None));
            return;
        }
        self.attention_menu = false;
        self.catalog(None);
    }
    pub fn open_conversation(&mut self, id: ConversationId) {
        self.active = true;
        if self.work.is_some() {
            self.detach();
            self.deferred_open = Some(Choice::Open(id));
        } else {
            self.choose(Choice::Open(id));
        }
    }
    fn catalog(&mut self, after: Option<ConversationId>) {
        self.attention_menu = false;
        if self.work.is_some() {
            return;
        }
        let service = self.service.clone();
        let previous = self.take_controller();
        self.menu = true;
        self.detail = None;
        self.work = Some(Box::pin(async move {
            if let Some(previous) = previous {
                previous.retire().await;
            }
            let service = service.ok_or("External conversations unavailable on this connection")?;
            let mut items = Vec::new();
            for endpoint in service
                .endpoints()
                .await
                .map_err(|error| error.to_string())?
            {
                if endpoint.enabled {
                    let mut bytes = [0u8; 16];
                    getrandom::fill(&mut bytes)
                        .map_err(|_| "Could not allocate external identity")?;
                    let id = ConversationId::new(format!("external_{}", hex::encode(bytes)))
                        .map_err(|error| error.to_string())?;
                    items.push((
                        format!("Start {}", endpoint.id),
                        Choice::Start(endpoint.id, id),
                    ));
                }
            }
            let saved = service
                .list(after)
                .await
                .map_err(|error| error.to_string())?;
            let next = (saved.len() == 64).then(|| saved.last().expect("nonempty page").id.clone());
            for saved in saved {
                items.push((
                    format!("{} · {}", saved.endpoint, saved.id.as_str()),
                    Choice::Open(saved.id),
                ));
            }
            if let Some(next) = next {
                items.push((
                    "Next conversations page".into(),
                    Choice::Catalog(Some(next)),
                ));
            }
            Ok(Event::Catalog(items))
        }));
    }
    fn control(&mut self, command: ExternalCommand) {
        if self.work.is_some() {
            return;
        }
        if let Some(controller) = &self.controller {
            let attempt = if let ExternalCommand::Submit { .. } = &command {
                let Some(attempt) = self.drafts.begin(controller.id()) else {
                    return;
                };
                Some(attempt)
            } else {
                None
            };
            let command = controller.command(command);
            self.work = Some(Box::pin(async move {
                let result = command.await.map_err(|error| error.to_string());
                if let Some(attempt) = attempt {
                    Ok(Event::Submitted(attempt, result))
                } else {
                    result.map(|()| Event::Done)
                }
            }));
        }
    }
    fn choose(&mut self, choice: Choice) {
        if self.work.is_some() {
            return;
        }
        match choice {
            Choice::Attention => self.open_attention(),
            Choice::AttentionOpen(position, target) => self.attention_open(position, target),
            Choice::Catalog(after) => self.catalog(after),
            Choice::Control(command) => {
                self.menu = false;
                self.control(command);
            }
            choice @ (Choice::Open(_) | Choice::Start(..)) => {
                self.attention_menu = false;
                self.detail = None;
                let service = self.service.clone();
                let (id, endpoint) = match choice {
                    Choice::Start(endpoint, id) => (id, Some(endpoint)),
                    Choice::Open(id) => (id, None),
                    _ => unreachable!(),
                };
                let old = self.take_controller();
                let execution = self.execution.clone();
                self.work = Some(Box::pin(async move {
                    if let Some(old) = old {
                        old.retire().await;
                    }
                    let service =
                        service.ok_or("External conversations unavailable on this connection")?;
                    if let Some(endpoint) = endpoint {
                        service
                            .start(id.clone(), &endpoint)
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                    Ok(Event::Attached(
                        ExternalController::attach(service, execution, id)
                            .await
                            .map_err(|error| error.to_string())?,
                        None,
                    ))
                }));
            }
            Choice::Source(source, start) => {
                let Some(controller) = self.controller.clone() else {
                    return;
                };
                self.menu = false;
                self.work = Some(Box::pin(async move {
                    let bytes = controller
                        .source(&source, start)
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(Event::Source(source, start, bytes))
                }));
            }
        }
    }
    fn controls(&mut self) {
        let Some(controller) = &self.controller else {
            self.catalog(None);
            return;
        };
        let view = controller.view();
        let mut items = vec![];
        for permission in view.observed.permissions {
            for option in permission.options {
                items.push((
                    format!("{} · {} ({})", permission.title, option.name, option.kind),
                    Choice::Control(ExternalCommand::Answer {
                        generation: permission.generation.clone(),
                        permission: permission.id.clone(),
                        option: option.id,
                    }),
                ));
            }
        }
        for (label, command) in [
            ("Refresh", ExternalCommand::Refresh),
            ("Cancel prompt", ExternalCommand::Cancel),
            ("Close peer", ExternalCommand::Close),
            ("History from beginning", ExternalCommand::Beginning),
            ("Next history page", ExternalCommand::Next),
            ("Follow latest", ExternalCommand::Live),
        ] {
            items.push((label.into(), Choice::Control(command)));
        }
        if !view.observed.connected {
            if view.capabilities.resume {
                items.push((
                    "Resume connection".into(),
                    Choice::Control(ExternalCommand::Reconnect {
                        setup: Setup::Resume,
                    }),
                ));
            }
            if view.capabilities.load {
                items.push((
                    "Reload remote history".into(),
                    Choice::Control(ExternalCommand::Reconnect { setup: Setup::Load }),
                ));
            }
        }
        for block in view.blocks {
            items.push((
                format!("Source · {} · {}", block.role, block.key),
                Choice::Source(block.source, 0),
            ));
        }
        items.push(("Other external conversations".into(), Choice::Catalog(None)));
        self.items = items;
        self.selected = 0;
        self.menu = true;
        self.detail = None;
        self.revision += 1;
    }
    pub fn paste(&mut self, text: &str) {
        if !self.menu && self.detail.is_none() {
            self.edit(|editor| editor.insert(text));
        }
    }
    pub fn key(&mut self, key: termina::event::KeyEvent) {
        let control = key.modifiers.contains(Modifiers::CONTROL);
        if key.code == KeyCode::Escape {
            if self.detail.take().is_some() {
                return;
            }
            if self.menu && self.controller.is_some() {
                self.menu = false;
                return;
            }
            self.active = false;
            self.detach();
            return;
        }
        if control && key.code == KeyCode::Char('p') {
            self.controls();
            return;
        }
        if control && key.code == KeyCode::Char('c') {
            self.control(ExternalCommand::Cancel);
            return;
        }
        if self.detail.is_some() {
            match key.code {
                KeyCode::Up | KeyCode::PageUp => self.offset = self.offset.saturating_sub(4),
                KeyCode::Down | KeyCode::PageDown => self.offset = self.offset.saturating_add(4),
                KeyCode::Right => {
                    if let Some(next) = self.next_source.clone() {
                        self.choose(next);
                    }
                }
                _ => {}
            }
            return;
        }
        if self.menu {
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
            return;
        }
        if key.code == KeyCode::Enter && !key.modifiers.contains(Modifiers::SHIFT) {
            if self.work.is_none() && !self.uncertain() && !self.editor.text().trim().is_empty() {
                self.control(ExternalCommand::Submit {
                    text: self.editor.text().into(),
                });
            }
        } else {
            self.edit(|editor| editor.key(key));
        }
    }
    pub fn mouse(
        &mut self,
        mouse: termina::event::MouseEvent,
        view: &rsi_terminal_ui::render::View,
    ) {
        if self.menu
            && view.choice_revision == self.revision
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(index) = view.choice_at(mouse.column, mouse.row)
            && index < self.items.len()
        {
            self.selected = index;
        }
    }
    fn unbind_editor(&mut self) {
        self.changes = None;
        self.drafts.unbind();
        self.editor = external_editor(String::new());
    }
    fn take_controller(&mut self) -> Option<Arc<ExternalController>> {
        self.unbind_editor();
        self.menu = true;
        self.items.clear();
        self.detail = None;
        self.next_source = None;
        self.permission_focus = None;
        self.controller.take()
    }
    fn detach(&mut self) {
        if !self.active {
            self.deferred_open = None;
        }
        if let Some(controller) = self.take_controller() {
            let pending = self.work.take();
            self.work = Some(Box::pin(async move {
                let result = match pending {
                    Some(work) => work.await,
                    None => Ok(Event::Done),
                };
                controller.retire().await;
                result
            }));
        }
    }
    pub async fn next(&mut self) {
        tokio::select! {
            result=async {match &mut self.work {Some(work)=>work.await,None=>std::future::pending().await}}=>{
                self.work=None;self.revision+=1;
                match result {
                    Ok(Event::Attention(items, truncated))=>{self.items=items;self.selected=0;self.menu=true;self.status=if truncated {"Showing bounded active conversations; more requests may exist".into()}else{"Current owner activity; reading does not cancel tasks".into()};}
                    Ok(Event::Native(position, request))=>{if let rsi_conversation::ConversationIdentity::Native(id)=&position.conversation {self.native=Some(id.clone());self.focus=Some((position,request));self.active=false;self.detach();}}
                    Ok(Event::Catalog(items))=>{self.items=items;self.selected=0;self.menu=true;self.status=if self.items.is_empty(){"No enabled endpoints or saved external conversations".into()}else{String::new()};}
                    Ok(Event::Attached(controller, notice))=>{if self.active && self.deferred_open.is_none(){
                        let text=self.drafts.bind(controller.id().clone()).to_owned();
                        self.editor=external_editor(text);self.changes=Some(controller.changes());self.controller=Some(controller);self.menu=false;self.status=notice.unwrap_or_default();
                    }else{controller.retire().await;}}
                    Ok(Event::Done)=>{if !self.uncertain(){self.status="Operation observed".into();}}
                    Ok(Event::Submitted(attempt,result))=>{
                        self.drafts.finish(&attempt,result.is_ok());
                        if let Some(id)=self.drafts.bound() {let text=self.drafts.text(id);if self.editor.text()!=text{self.editor=external_editor(text.to_owned());}}
                        self.status=result.map_or_else(|error|error,|()|"Prompt admitted; following local observations".into());
                    }
                    Ok(Event::Source(source,start,bytes))=>{if self.active && self.deferred_open.is_none() && self.controller.is_some() {let length=bytes.len().min(8000);let text=String::from_utf8_lossy(&bytes[..length]);self.detail=Some(format!("Observed source · bytes {start}–{}\n{}",start+length,text));self.next_source=(bytes.len()>length || bytes.len()==64*1024).then_some(Choice::Source(source,start+length));self.offset=0;}}
                    Err(error)=>{self.status=error;}
                }
                if self.controller.is_some() && let Some(target) = self.permission_focus.take() {self.focus_permission(&target);}
                if self.active && let Some(choice)=self.deferred_open.take() {self.choose(choice);}
                if !self.active {self.detach();}
            }
            ()=async {match &mut self.changes {Some(changes)=>{let _=changes.changed().await;},None=>std::future::pending().await}}=>{self.revision+=1;}
        }
    }
    pub fn scene(&self) -> std::result::Result<Scene, &'static str> {
        let view = if self.attention_menu {
            None
        } else {
            self.controller
                .as_ref()
                .map(|controller| controller.view_tail(8192))
        };
        let title = if self.attention_menu {
            "Needs attention".into()
        } else {
            view.as_ref().map_or_else(
                || "External agents".into(),
                |view| format!("{} · External agent", view.observed.snapshot.endpoint),
            )
        };
        let explanation = view.as_ref().map_or_else(
            || {
                if self.attention_menu {
                    "Pending requests first. Refresh to read current activity.".into()
                } else {
                    "Select an operator-configured endpoint or observed conversation.".into()
                }
            },
            |view| {
                format!(
                    "{} · {} · {:?} · {} pending permissions",
                    view.observed.snapshot.cwd,
                    if view.observed.connected {
                        "Connected"
                    } else {
                        "Unknown / disconnected"
                    },
                    view.observed.snapshot.status,
                    view.observed.permissions.len()
                )
            },
        );
        let transcript = view
            .as_ref()
            .map(|view| {
                view.blocks.iter().fold(String::new(), |mut text, block| {
                    use std::fmt::Write as _;
                    writeln!(text, "{} · {}\n{}", block.role, block.key, block.text)
                        .expect("String formatting");
                    text
                })
            })
            .unwrap_or_default();
        let start = transcript.ceil_char_boundary(transcript.len().saturating_sub(8192));
        let mut status = if self.uncertain() {
            format!(
                "Send outcome unknown; unchanged text cannot be resent. {}",
                self.status
            )
        } else {
            self.status.clone()
        };
        status.truncate(status.floor_char_boundary(8192));
        Ok(Scene::from(ApplicationScene {
            title,
            explanation,
            revision: self.revision,
            detail: self
                .detail
                .clone()
                .or_else(|| (!self.menu).then(|| transcript[start..].into())),
            detail_offset: if self.detail.is_some() {
                self.offset
            } else {
                usize::MAX
            },
            items: if self.menu {
                self.items.iter().map(|(label, _)| label.clone()).collect()
            } else {
                vec![]
            },
            selected: if self.menu { self.selected } else { 0 },
            field: (!self.menu && self.detail.is_none()).then(|| "Message".into()),
            input: Draft::capture(&self.editor)?,
            status,
            hint: if self.attention_menu {
                "Enter selects request · Esc back"
            } else {
                "Enter select/send · Ctrl+P permissions and controls · Ctrl+C cancel · Esc back"
            }
            .into(),
            progress: self
                .work
                .as_ref()
                .map(|_| "Waiting for observed outcome…".into()),
            ..ApplicationScene::default()
        }))
    }
    pub async fn shutdown(&mut self) {
        self.active = false;
        while self.work.is_some() {
            self.next().await;
        }
        if let Some(controller) = self.controller.take() {
            controller.retire().await;
        }
        self.changes = None;
    }
}

fn external_editor(text: String) -> editor::Editor {
    let mut editor = editor::Editor::with_text(text, drafts::MAXIMUM_TEXT);
    editor.set_retention_limit(2 * drafts::MAXIMUM_TEXT);
    editor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deferred_navigation_hides_editor_and_settles_submitted_revision() {
        for destination in 0..3 {
            let runtime = rsi_meta::Runtime::default();
            let mut ui = Ui::new(&runtime.root());
            let id = ConversationId::new("pending").unwrap();
            ui.drafts.bind(id.clone());
            ui.active = true;
            ui.menu = false;
            ui.paste("retained prompt");
            let attempt = ui.drafts.begin(&id).unwrap();
            let (send, receive) = tokio::sync::oneshot::channel();
            ui.work = Some(Box::pin(async move {
                receive.await.unwrap();
                Ok(Event::Submitted(attempt, Err("unknown result".into())))
            }));
            match destination {
                0 => ui.open(),
                1 => ui.open_attention(),
                _ => ui.open_conversation(ConversationId::new("missing").unwrap()),
            }
            assert!(ui.menu && ui.drafts.bound().is_none());
            assert!(ui.editor.text().is_empty());
            ui.paste("must not edit old conversation");
            assert_eq!(ui.drafts.text(&id), "retained prompt");
            let scene =
                serde_json::from_slice::<serde_json::Value>(&ui.scene().unwrap().encode().unwrap())
                    .unwrap();
            assert!(!scene.to_string().contains("retained prompt"));
            send.send(()).unwrap();
            ui.next().await;
            assert!(ui.drafts.uncertain(&id));
            ui.next().await;
            assert!(ui.menu && ui.controller.is_none() && ui.drafts.bound().is_none());
            assert!(!ui.status.is_empty());
            assert!(
                !String::from_utf8(ui.scene().unwrap().encode().unwrap())
                    .unwrap()
                    .contains("Message")
            );
        }
    }

    #[tokio::test]
    async fn failed_direct_attachment_after_escape_has_no_dead_message_field() {
        let runtime = rsi_meta::Runtime::default();
        let mut ui = Ui::new(&runtime.root());
        ui.active = true;
        ui.menu = false;
        ui.drafts.bind(ConversationId::new("old").unwrap());
        ui.key(termina::event::KeyEvent::new(
            KeyCode::Escape,
            Modifiers::empty(),
        ));
        ui.open_conversation(ConversationId::new("missing").unwrap());
        ui.next().await;
        assert!(ui.menu && ui.controller.is_none());
        assert!(
            !String::from_utf8(ui.scene().unwrap().encode().unwrap())
                .unwrap()
                .contains("Message")
        );
    }

    #[tokio::test]
    async fn composed_status_keeps_unknown_warning_within_scene_bound() {
        let runtime = rsi_meta::Runtime::default();
        let mut ui = Ui::new(&runtime.root());
        let id = ConversationId::new("unknown-status").unwrap();
        ui.drafts.bind(id.clone());
        ui.menu = false;
        ui.paste("retained");
        let attempt = ui.drafts.begin(&id).unwrap();
        ui.drafts.finish(&attempt, false);
        for status in ["x".repeat(8192), "中🙂".repeat(2000)] {
            ui.status = status;
            let bytes = ui.scene().unwrap().encode().unwrap();
            let scene = Scene::decode(&bytes).unwrap();
            scene.render(42, 12).unwrap();
            let text = String::from_utf8(bytes).unwrap();
            assert!(text.contains("Send outcome unknown; unchanged text cannot be resent."));
            assert!(ui.uncertain());
            assert_eq!(ui.editor.text(), "retained");
        }
    }

    #[tokio::test]
    async fn leaving_external_conversation_clears_the_editor_even_when_navigation_fails() {
        for attention in [false, true] {
            let runtime = rsi_meta::Runtime::default();
            let mut ui = Ui::new(&runtime.root());
            let id = ConversationId::new("retained-unknown").unwrap();
            ui.drafts.bind(id.clone());
            ui.menu = false;
            ui.paste("unconfirmed text");
            let attempt = ui.drafts.begin(&id).unwrap();
            ui.drafts.finish(&attempt, false);
            if attention {
                ui.open_attention();
            } else {
                ui.open();
            }
            if ui.work.is_some() {
                ui.next().await;
            }
            assert!(ui.controller.is_none());
            assert!(ui.drafts.bound().is_none());
            assert!(ui.editor.text().is_empty());
            assert!(!ui.uncertain());
            let scene = String::from_utf8(ui.scene().unwrap().encode().unwrap()).unwrap();
            assert!(!scene.contains("unconfirmed text"));
            assert!(!scene.contains("Send outcome unknown"));
            assert_eq!(ui.drafts.bind(id.clone()), "unconfirmed text");
            assert!(ui.drafts.uncertain(&id));
            ui.open_conversation(ConversationId::new("missing").unwrap());
            if ui.work.is_some() {
                ui.next().await;
            }
            assert!(ui.drafts.bound().is_none());
            assert!(ui.editor.text().is_empty());
        }
    }

    #[tokio::test]
    async fn cursor_noop_and_rejected_insert_preserve_unknown_revision() {
        let runtime = rsi_meta::Runtime::default();
        let mut ui = Ui::new(&runtime.root());
        let id = ConversationId::new("unknown-draft").unwrap();
        ui.drafts.bind(id.clone());
        ui.menu = false;
        ui.paste("original");
        let attempt = ui.drafts.begin(&id).unwrap();
        ui.drafts.finish(&attempt, false);
        assert!(ui.uncertain());
        ui.edit(|editor| {
            editor.key(termina::event::KeyEvent::new(
                KeyCode::Left,
                Modifiers::empty(),
            ))
        });
        assert!(ui.uncertain());
        ui.paste("");
        assert!(ui.uncertain());
        ui.paste("\0");
        assert!(ui.uncertain());
        assert_eq!(ui.editor.text(), "original");
        ui.paste(&"x".repeat(128 * 1024));
        assert!(ui.uncertain());
        assert_eq!(ui.editor.text(), "original");
        ui.paste("edit");
        assert!(!ui.uncertain());
        let accepted = ui.editor.text().to_owned();
        assert_eq!(ui.drafts.text(&id), accepted);
        ui.key(termina::event::KeyEvent::new(
            KeyCode::Escape,
            Modifiers::empty(),
        ));
        assert_eq!(ui.drafts.bind(id), accepted);
    }

    #[tokio::test]
    async fn external_editor_bounds_journal_and_detach_clears_binding() {
        let runtime = rsi_meta::Runtime::default();
        let mut ui = Ui::new(&runtime.root());
        let id = ConversationId::new("active-draft").unwrap();
        ui.drafts.bind(id.clone());
        ui.menu = false;
        for i in 0..64 {
            let text = if i % 2 == 0 { "x" } else { "y" }.repeat(drafts::MAXIMUM_TEXT);
            ui.edit(|editor| editor.replace_text(&text));
            assert_eq!(ui.drafts.text(&id), text);
            assert!(ui.editor.retained_bytes() <= 2 * drafts::MAXIMUM_TEXT);
        }
        let attempt = ui.drafts.begin(&id).unwrap();
        ui.drafts.finish(&attempt, false);
        assert!(ui.uncertain());
        ui.detach();
        assert!(!ui.uncertain());
        assert!(ui.drafts.bound().is_none());
        assert!(ui.editor.text().is_empty());
        assert_eq!(ui.drafts.bind(id.clone()).len(), drafts::MAXIMUM_TEXT);
        assert!(ui.uncertain());
    }
}
