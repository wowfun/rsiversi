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

enum Event {
    Attention(Vec<(String, Choice)>, bool),
    Native(
        rsi_navigation_api::attention::Position,
        Option<rsi_session_protocol::ActivityRequest>,
    ),
    Catalog(Vec<(String, Choice)>),
    Attached(Arc<ExternalController>, Option<String>),
    Done,
    Submitted(String),
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "Activity, menu visibility and submission receipts are independent UI state"
)]
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
    draft_id: Option<ConversationId>,
    drafts: BTreeMap<ConversationId, String>,
    detail: Option<String>,
    next_source: Option<Choice>,
    offset: usize,
    status: String,
    uncertain: bool,
    pending_submit: bool,
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
            editor: editor::Editor::with_text(String::new(), 128 * 1024),
            draft_id: None,
            drafts: BTreeMap::new(),
            detail: None,
            next_source: None,
            offset: 0,
            status: String::new(),
            uncertain: false,
            pending_submit: false,
            revision: 0,
        }
    }
    fn remember(&mut self) {
        if let Some(id) = &self.draft_id {
            self.drafts
                .insert(id.clone(), self.editor.text().to_owned());
        }
        while self.drafts.len() > 16
            || self.drafts.values().map(String::len).sum::<usize>() > 1024 * 1024
        {
            self.drafts.pop_first();
        }
    }
    pub fn open(&mut self) {
        self.active = true;
        if self.work.is_some() {
            self.deferred_open = Some(Choice::Catalog(None));
            return;
        }
        self.attention_menu = false;
        self.catalog(None);
    }
    pub fn open_conversation(&mut self, id: ConversationId) {
        self.active = true;
        if self.work.is_some() {
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
        let Some(service) = self.service.clone() else {
            self.status = "External conversations unavailable on this connection".into();
            return;
        };
        self.remember();
        self.menu = true;
        self.detail = None;
        self.work = Some(Box::pin(async move {
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
            let text = if let ExternalCommand::Submit { text } = &command {
                Some(text.clone())
            } else {
                None
            };
            self.pending_submit = text.is_some();
            let command = controller.command(command);
            self.work = Some(Box::pin(async move {
                command.await.map_err(|error| error.to_string())?;
                Ok(text.map_or(Event::Done, Event::Submitted))
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
                let Some(service) = self.service.clone() else {
                    return;
                };
                let (id, endpoint) = match choice {
                    Choice::Start(endpoint, id) => (id, Some(endpoint)),
                    Choice::Open(id) => (id, None),
                    _ => unreachable!(),
                };
                self.remember();
                let old = self.controller.take();
                self.changes = None;
                let execution = self.execution.clone();
                self.work = Some(Box::pin(async move {
                    if let Some(old) = old {
                        old.retire().await;
                    }
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
            self.uncertain = false;
            if let Err(error) = self.editor.insert(text) {
                self.status = error.into();
            }
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
            self.remember();
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
            if self.work.is_none() && !self.uncertain && !self.editor.text().trim().is_empty() {
                self.control(ExternalCommand::Submit {
                    text: self.editor.text().into(),
                });
            }
        } else {
            self.uncertain = false;
            if let Err(error) = self.editor.key(key) {
                self.status = error.into();
            }
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
    fn detach(&mut self) {
        self.changes = None;
        if self.work.is_none()
            && let Some(controller) = self.controller.take()
        {
            self.work = Some(Box::pin(async move {
                controller.retire().await;
                Ok(Event::Done)
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
                    Ok(Event::Attached(controller, notice))=>{if self.active{self.draft_id=Some(controller.id().clone());self.editor=editor::Editor::with_text(self.drafts.get(controller.id()).cloned().unwrap_or_default(),128*1024);self.changes=Some(controller.changes());self.controller=Some(controller);self.menu=false;self.status=notice.unwrap_or_default();self.uncertain=false;}else{controller.retire().await;}}
                    Ok(Event::Done)=>{if !self.uncertain{self.status="Operation observed".into();}}
                    Ok(Event::Submitted(text))=>{if self.editor.text()==text{self.editor.take();self.remember();}self.status="Prompt admitted; following local observations".into();}
                    Ok(Event::Source(source,start,bytes))=>{let length=bytes.len().min(8000);let text=String::from_utf8_lossy(&bytes[..length]);self.detail=Some(format!("Observed source · bytes {start}–{}\n{}",start+length,text));self.next_source=(bytes.len()>length || bytes.len()==64*1024).then_some(Choice::Source(source,start+length));self.offset=0;}
                    Err(error)=>{self.uncertain|=self.pending_submit;self.status=error;}
                }
                if self.controller.is_some() && let Some(target) = self.permission_focus.take() {self.focus_permission(&target);}
                self.pending_submit=false;
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
            status: self.status.clone(),
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
