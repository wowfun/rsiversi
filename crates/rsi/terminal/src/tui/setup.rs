//! Application commands and redacted setup interaction, independent of a Session.
use super::{editor::Editor, *};
use futures_util::future::BoxFuture;
use rsi_ai_protocol::{DiscoveredModel, LanguageModelLimits, ModelRef};
use rsi_configuration_api::{DiscoveryRequest, ManagedProvider, ProviderKind};
use rsi_credentials_protocol::{CredentialAvailability, CredentialStatus, SecretValue};
use rsi_terminal_ui::scene::{ApplicationScene, Scene};
use rsi_workbench_ui::{SetupCommand, SetupFeature, SetupView};
use serde_json::json;
use termina::event::KeyEvent;
use zeroize::{Zeroize, Zeroizing};

type Outcome<T> = std::result::Result<T, String>;
const SECRET_BYTES: usize = 64 * 1024;

pub(super) async fn configured_models(
    catalog: &dyn rsi_ai_protocol::LanguageModels,
    until: Option<&ModelRef>,
) -> Outcome<Vec<ModelRef>> {
    let mut routes = vec![];
    // Pages are validated by the catalog owner/remote adapter. Bound the entire
    // scan as well, including catalogs changing between page requests.
    for _ in 0..16 {
        let page = catalog
            .list_models(routes.last(), 256)
            .await
            .map_err(|error| error.to_string())?;
        routes.extend(page.models);
        if !page.has_more || until.is_some_and(|model| routes.contains(model)) {
            return Ok(routes);
        }
    }
    Err("Configured model list exceeds the 4096-entry scan bound".into())
}
#[derive(Clone, Debug)]
pub(super) enum Command {
    Login(Option<ProviderKind>),
    Models,
    Effort,
    Help,
    Plugins,
    New,
    Resume(Option<SessionId>),
    Reference(Option<SessionId>),
    Quit,
    Invalid,
}
pub(super) fn command(text: &str) -> Option<Command> {
    if text.contains(['\r', '\n']) {
        return None;
    }
    let words: Vec<_> = text.split_whitespace().collect();
    match words.as_slice() {
        ["/plugins"] => Some(Command::Plugins),
        ["/help"] => Some(Command::Help),
        ["/new"] => Some(Command::New),
        ["/quit" | "/exit"] => Some(Command::Quit),
        ["/resume"] => Some(Command::Resume(None)),
        ["/resume", id] => {
            Some(SessionId::new(*id).map_or(Command::Invalid, |id| Command::Resume(Some(id))))
        }
        ["/reference"] => Some(Command::Reference(None)),
        ["/reference", id] => {
            Some(SessionId::new(*id).map_or(Command::Invalid, |id| Command::Reference(Some(id))))
        }
        ["/model"] => Some(Command::Models),
        ["/effort"] => Some(Command::Effort),
        ["/login"] => Some(Command::Login(None)),
        ["/login", provider] => Some(match *provider {
            "deepseek" => Command::Login(Some(ProviderKind::Deepseek)),
            "openai" => Command::Login(Some(ProviderKind::Openai)),
            "openai-compatible" => Command::Login(Some(ProviderKind::OpenaiCompatible)),
            _ => Command::Invalid,
        }),
        [
            "/effort" | "/model" | "/login" | "/help" | "/new" | "/quit" | "/exit" | "/resume"
            | "/reference" | "/plugins",
            ..,
        ] => Some(Command::Invalid),
        _ => None,
    }
}
#[derive(Clone)]
enum Choice {
    Provider(ProviderKind),
    Existing(ManagedProvider),
    DiscoverExisting(ManagedProvider),
    New(ProviderKind),
    Route(ModelRef),
    Effort(rsi_agent_session_protocol::ModelSelection),
    EffortDefault(Option<rsi_ai_protocol::ReasoningEffortId>),
    Candidate(DiscoveredModel),
    Discover,
    Manual,
    Login,
    Refresh,
    Credentials,
    Connection,
}
#[derive(Clone, Copy)]
enum Field {
    Name,
    Endpoint,
    Model,
    Context,
    Output,
    Efforts,
}
#[derive(Clone)]
enum Stage {
    Menu(Vec<(String, Choice)>),
    Text(Field),
    Secret,
}
/// Exact supported integration credential targets; no arbitrary owner/slot input.
#[derive(Clone, Debug)]
pub(super) enum IntegrationCredential {
    Mcp(rsi_configuration_api::McpCredentialTarget),
    Exa,
}
impl IntegrationCredential {
    fn title(&self) -> String {
        match self {
            Self::Mcp(target) => format!(
                "MCP credential · {} · {}",
                target.server, target.reference.slot
            ),
            Self::Exa => "Exa search credential".into(),
        }
    }
    fn read(&self) -> rsi_workbench_ui::PluginsCommand {
        match self {
            Self::Mcp(target) => rsi_workbench_ui::PluginsCommand::McpCredentialStatus {
                target: target.clone(),
            },
            Self::Exa => rsi_workbench_ui::PluginsCommand::ExaStatus,
        }
    }
    fn write(self, secret: SecretValue) -> rsi_workbench_ui::PluginsCommand {
        match self {
            Self::Mcp(target) => {
                rsi_workbench_ui::PluginsCommand::McpCredentialSet { target, secret }
            }
            Self::Exa => rsi_workbench_ui::PluginsCommand::ExaSet { secret },
        }
    }
    fn observed(&self, view: &rsi_workbench_ui::PluginsView) -> Option<CredentialStatus> {
        match self {
            Self::Mcp(target) => view
                .mcp_credential
                .as_ref()
                .filter(|status| status.target == *target)
                .map(|status| CredentialStatus {
                    availability: status.availability.clone(),
                    editable: status.editable,
                    store_path: None,
                }),
            Self::Exa => view.exa_credential.as_ref().map(|status| CredentialStatus {
                availability: status.availability.clone(),
                editable: status.editable,
                store_path: None,
            }),
        }
    }
    fn saved(&self) -> &'static str {
        match self {
            Self::Mcp(_) => {
                "Credential saved. Close this screen and refresh the MCP connection to verify access."
            }
            Self::Exa => {
                "Exa credential saved. No search was submitted. Enable web_search separately for new conversations."
            }
        }
    }
}
enum Update {
    Open(Command, Vec<ModelRef>),
    Credential,
    CredentialStatus,
    IntegrationCredential(Option<CredentialStatus>, bool),
    Discovered(Vec<DiscoveredModel>),
    Saved(ModelRef),
    Efforts(ModelRef, rsi_ai_protocol::LanguageProfile),
    Selected(rsi_agent_session_protocol::ModelSelection),
}
struct Screen {
    composer_prefix: Option<String>,
    declare_effort: bool,
    stage: Stage,
    title: String,
    editor: Editor,
    selected: usize,
    candidate: Option<DiscoveredModel>,
    definition: Option<ManagedProvider>,
    existing: bool,
    editing_connection: bool,
}
#[allow(clippy::struct_excessive_bools)] // Visibility, attachment, selection intent and retained-write state are independent.
pub(super) struct Ui {
    composer_prefix: Option<String>,
    declare_effort: bool,
    feature: Option<Arc<SetupFeature>>,
    catalog: Arc<dyn rsi_ai_protocol::LanguageModels>,
    integration: Option<(IntegrationCredential, Arc<rsi_workbench_ui::PluginsFeature>)>,
    pub active: bool,
    attached: bool,
    stage: Stage,
    title: String,
    status: String,
    selected: usize,
    editor: Editor,
    secret: Zeroizing<String>,
    view: SetupView,
    deployments: BTreeSet<String>,
    definition: Option<ManagedProvider>,
    candidate: Option<DiscoveredModel>,
    save_default: bool,
    effort_selection: Option<rsi_agent_session_protocol::ModelSelection>,
    pending: Option<BoxFuture<'static, Outcome<Update>>>,
    mutation: bool,
    followup: bool,
    attempt: u64,
    menu_revision: u64,
    back: Vec<Screen>,
    credential: Option<CredentialStatus>,
    existing: bool,
    editing_connection: bool,
    pub chosen: Option<rsi_agent_session_protocol::ModelSelection>,
}
impl Ui {
    pub fn new(
        feature: Option<Arc<SetupFeature>>,
        catalog: Arc<dyn rsi_ai_protocol::LanguageModels>,
    ) -> Self {
        Self {
            composer_prefix: None,
            declare_effort: false,
            feature,
            catalog,
            integration: None,
            active: false,
            attached: false,
            stage: Stage::Menu(vec![]),
            title: String::new(),
            status: String::new(),
            selected: 0,
            editor: Editor::default(),
            secret: Zeroizing::new(String::new()),
            view: SetupView::default(),
            deployments: BTreeSet::new(),
            definition: None,
            candidate: None,
            save_default: false,
            effort_selection: None,
            pending: None,
            mutation: false,
            followup: true,
            attempt: 0,
            menu_revision: 0,
            back: vec![],
            credential: None,
            existing: false,
            editing_connection: false,
            chosen: None,
        }
    }
    pub(super) fn open_integration(
        &mut self,
        target: IntegrationCredential,
        feature: Arc<rsi_workbench_ui::PluginsFeature>,
    ) {
        if self.pending.is_some() && self.mutation {
            self.active = true;
            self.status = "Waiting for the current credential result".into();
            return;
        }
        self.pending = None;
        self.secret.zeroize();
        self.editor = Editor::default();
        self.back.clear();
        self.definition = None;
        self.candidate = None;
        self.active = true;
        self.attached = true;
        self.followup = true;
        self.mutation = false;
        self.composer_prefix = None;
        self.view = SetupView::default();
        self.title = target.title();
        self.status = "Reading current credential status…".into();
        self.stage = Stage::Secret;
        self.credential = None;
        self.integration = Some((target.clone(), feature.clone()));
        self.pending = Some(Box::pin(async move {
            feature.command(target.read()).await?;
            Ok(Update::IntegrationCredential(
                target.observed(&feature.snapshot()),
                false,
            ))
        }));
    }
    fn submit_integration_secret(&mut self) -> Outcome<()> {
        let (target, feature) = self
            .integration
            .clone()
            .ok_or("Integration credential screen is unavailable")?;
        if !self
            .credential
            .as_ref()
            .is_some_and(|status| status.editable)
        {
            return Err("Read an editable credential status before saving".into());
        }
        let secret = SecretValue::new(std::mem::take(&mut *self.secret))
            .map_err(|_| "Enter a nonempty API key within 64 KiB".to_owned())?;
        self.pending = Some(Box::pin(async move {
            feature.command(target.write(secret)).await?;
            Ok(Update::IntegrationCredential(None, true))
        }));
        self.mutation = true;
        self.status = "Saving credential…".into();
        Ok(())
    }
    pub fn open_effort(&mut self, selection: rsi_agent_session_protocol::ModelSelection) {
        self.open(Command::Effort, true);
        if !self.mutation {
            self.effort_selection = Some(selection);
        }
    }
    pub fn open(&mut self, command: Command, attached: bool) {
        self.active = true;
        if self.pending.is_some() && self.mutation {
            self.title = "Setup save in progress".into();
            self.status = "Waiting for the independent result; Ctrl+C closes.".into();
            return;
        }
        self.pending = None;
        self.integration = None;
        self.effort_selection = None;
        self.composer_prefix = match command {
            Command::Models => Some("/model ".into()),
            Command::Effort => Some("/effort ".into()),
            _ => None,
        };
        self.save_default = false;
        self.followup = true;
        self.attempt += 1;
        self.back.clear();
        self.credential = None;
        self.existing = false;
        self.editing_connection = false;
        self.declare_effort = false;
        self.attached = attached;
        self.definition = None;
        self.candidate = None;
        self.secret.zeroize();
        self.editor = Editor::default();
        self.selected = 0;
        self.stage = Stage::Menu(vec![]);
        self.title = "Model setup".into();
        let Some(feature) = self.feature.clone() else {
            self.status = "This application profile does not provide model setup. Use the built-in rsi tui profile.".into();
            return;
        };
        self.status = "Loading configuration…".into();
        let catalog = self.catalog.clone();
        self.pending = Some(Box::pin(async move {
            feature.command(SetupCommand::Refresh).await?;
            // Model catalog is supplementary to login, never an authentication gate.
            let routes = if matches!(command, Command::Effort) {
                vec![]
            } else {
                match configured_models(catalog.as_ref(), None).await {
                    Ok(routes) => routes,
                    Err(_) if matches!(command, Command::Login(_)) => vec![],
                    Err(problem) => return Err(problem),
                }
            };
            Ok(Update::Open(command, routes))
        }));
        self.mutation = false;
    }
    #[allow(clippy::too_many_lines)] // Exhaustive operation results and closed-workflow fence.
    pub async fn next(&mut self) {
        let result = match &mut self.pending {
            Some(work) => work.await,
            None => std::future::pending().await,
        };
        self.pending = None;
        let was_mutation = std::mem::take(&mut self.mutation);
        if let Some((target, _)) = &self.integration {
            self.status = match result {
                Ok(Update::IntegrationCredential(status, saved)) => {
                    self.credential = status;
                    if saved {
                        target.saved().into()
                    } else {
                        "Enter a key and press Enter to save. Esc returns to Plugins.".into()
                    }
                }
                Err(problem) => {
                    self.credential = None;
                    format!(
                        "Credential: {problem}. Close and reopen this screen to reconcile before another write."
                    )
                }
                _ => "Credential response changed; close and reopen this screen".into(),
            };
            return;
        }
        if let Some(feature) = &self.feature {
            self.view = feature.snapshot();
        }
        if !self.followup {
            self.reconcile();
            self.status = match result {
                Ok(_) => {
                    "Setup operation completed. Refresh to continue; no follow-up was started."
                        .into()
                }
                Err(problem) => {
                    format!("Setup operation: {problem}. Refresh before another write.")
                }
            };
            if self.active {
                let status = self.status.clone();
                self.menu(
                    "Setup result",
                    vec![("Refresh configuration…".into(), Choice::Refresh)],
                );
                self.status = status;
            }
            return;
        }
        match result {
            Ok(Update::IntegrationCredential(_, _)) => {
                self.status = "Integration credential screen is no longer active".into();
            }
            Err(problem) => {
                let composer_prefix = self.composer_prefix.clone();
                if was_mutation {
                    self.reconcile();
                    self.menu(
                        "Setup write requires reconciliation",
                        vec![("Refresh configuration…".into(), Choice::Refresh)],
                    );
                } else if self.definition.is_some() {
                    self.discovery_menu(vec![]);
                } else {
                    self.menu(
                        "Setup unavailable",
                        vec![("Refresh configuration…".into(), Choice::Refresh)],
                    );
                }
                self.status = format!("{problem}. Esc back · Ctrl+C close");
                self.composer_prefix = composer_prefix;
            }
            Ok(Update::Open(command, routes)) => {
                self.deployments = routes
                    .iter()
                    .map(|model| model.deployment().to_owned())
                    .collect();
                self.status = if self.view.allowed {
                    "Enter selects · Esc cancels"
                } else {
                    "Configuration access is read only"
                }
                .into();
                match command {
                    Command::Login(Some(provider)) => self.deployments(provider),
                    Command::Login(None) => self.menu("Log in · choose provider", vec![
                        ("DeepSeek".into(), Choice::Provider(ProviderKind::Deepseek)),
                        ("OpenAI".into(), Choice::Provider(ProviderKind::Openai)),
                        ("OpenAI-compatible".into(), Choice::Provider(ProviderKind::OpenaiCompatible)),
                    ]),
                    Command::Effort => {
                        if let Some(selection) = &self.effort_selection {
                            self.describe_efforts(selection.model.clone());
                        } else {
                            self.status = "Choose a model with /model first".into();
                        }
                    }
                    Command::Models => {
                        let mut items = routes.into_iter().map(|model| (format!("{}/{}", model.deployment(), model.model()), Choice::Route(model))).collect::<Vec<_>>();
                        if let Some(providers) = &self.view.providers {
                            items.extend(providers.deployments.iter().map(|entry| (format!("Discover / refresh · {}", entry.config["deployment"].as_str().unwrap_or("provider")), Choice::DiscoverExisting(entry.clone()))));
                        }
                        items.push(("Log in / add provider…".into(), Choice::Login));
                        self.menu(if self.attached { "Model for next request" } else { "Set default and start" }, items);
                        self.composer_prefix = Some("/model ".into());
                    }
                    _ => self.status = "Usage: /login [deepseek|openai|openai-compatible] or /model. Enter keys only in the masked field.".into(),
                }
            }
            Ok(Update::CredentialStatus) => {
                self.credential = self
                    .view
                    .credential
                    .as_ref()
                    .filter(|view| {
                        self.definition.as_ref().is_some_and(|definition| {
                            view.provider == definition.provider
                                && definition.config["credential"]["slot"].as_str()
                                    == Some(view.slot.as_str())
                        })
                    })
                    .map(|view| view.status.clone());
                self.status.clear();
                self.stage = Stage::Secret;
                self.composer_prefix = None;
                self.title = "API key · masked".into();
                self.editor = Editor::default();
            }
            Ok(Update::Credential) => {
                self.credential = self
                    .view
                    .credential
                    .as_ref()
                    .map(|view| view.status.clone());
                self.status.clear();
                self.discover();
            }
            Ok(Update::Discovered(models)) => self.discovery_menu(models),
            Ok(Update::Saved(model)) => self.describe_efforts(model),
            Ok(Update::Efforts(model, profile)) => {
                let prefix = if self.effort_selection.is_some() {
                    "/effort ".into()
                } else {
                    format!("/model {} ", model.model())
                };
                let label = profile.reasoning_efforts().default_effort().map_or_else(
                    || "Provider default".into(),
                    |effort| format!("Provider default ({effort})"),
                );
                let mut choices = vec![(
                    label,
                    Choice::Effort(rsi_agent_session_protocol::ModelSelection {
                        model: model.clone(),
                        reasoning_effort: None,
                    }),
                )];
                choices.extend(
                    profile
                        .reasoning_efforts()
                        .supported()
                        .iter()
                        .map(|effort| {
                            (
                                effort.to_string(),
                                Choice::Effort(rsi_agent_session_protocol::ModelSelection {
                                    model: model.clone(),
                                    reasoning_effort: Some(effort.clone()),
                                }),
                            )
                        }),
                );
                let selected = self.effort_selection.as_ref().and_then(|current| choices.iter().position(|(_, choice)| matches!(choice, Choice::Effort(selection) if selection == current))).unwrap_or(0);
                self.menu("Reasoning effort", choices);
                self.composer_prefix = Some(prefix);
                self.selected = selected;
            }
            Ok(Update::Selected(selection)) => {
                self.active = false;
                self.chosen = Some(selection);
                self.status.clear();
            }
        }
    }
    fn menu(&mut self, title: &str, items: Vec<(String, Choice)>) {
        self.composer_prefix = None;
        self.menu_revision = self.menu_revision.wrapping_add(1);
        self.status.clear();
        self.title = title.into();
        self.stage = Stage::Menu(items);
        self.selected = 0;
        self.editor = Editor::with_text(String::new(), 512);
    }
    fn field(&mut self, title: &str, field: Field, value: String) {
        self.composer_prefix = None;
        self.status.clear();
        self.title = title.into();
        self.stage = Stage::Text(field);
        self.editor = Editor::with_text(value, 2048);
    }
    fn deployments(&mut self, provider: ProviderKind) {
        let mut items = self
            .view
            .providers
            .as_ref()
            .map(|view| {
                view.deployments
                    .iter()
                    .filter(|entry| entry.provider == provider)
                    .map(|entry| {
                        (
                            format!(
                                "Use {}",
                                entry.config["deployment"].as_str().unwrap_or("deployment")
                            ),
                            Choice::Existing(entry.clone()),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if items.is_empty() {
            if let Err(problem) = self.choose(Choice::New(provider), false) {
                self.status = problem;
            }
            return;
        }
        items.push(("New deployment…".into(), Choice::New(provider)));
        self.menu("Log in · choose deployment", items);
    }
    fn request(&self) -> Outcome<DiscoveryRequest> {
        let definition = self.definition.as_ref().ok_or("Choose a provider")?;
        let request = DiscoveryRequest {
            provider: definition.provider,
            endpoint: definition.config["endpoint"]
                .as_str()
                .ok_or("Missing endpoint")?
                .into(),
            slot: definition.config["credential"]["slot"]
                .as_str()
                .ok_or("Missing credential slot")?
                .into(),
        };
        request.validate().map_err(|error| error.to_string())?;
        Ok(request)
    }
    fn discover(&mut self) {
        let result = self.request();
        let Some(feature) = self.feature.clone() else {
            return;
        };
        self.menu("Discover models", vec![]);
        self.composer_prefix = Some("/model ".into());
        self.pending = Some(Box::pin(async move {
            Ok(Update::Discovered(feature.discover(result?).await?.models))
        }));
        self.mutation = false;
    }
    fn discovery_menu(&mut self, models: Vec<DiscoveredModel>) {
        let mut items = models
            .into_iter()
            .map(|model| (model.id.clone(), Choice::Candidate(model)))
            .collect::<Vec<_>>();
        items.push(("Discover / refresh online…".into(), Choice::Discover));
        items.push(("Enter model manually…".into(), Choice::Manual));
        items.push(("Change API key…".into(), Choice::Credentials));
        items.push(("Edit connection settings…".into(), Choice::Connection));
        self.menu(
            if self.attached {
                "Choose model"
            } else {
                "Set default and start · choose model"
            },
            items,
        );
        self.composer_prefix = Some("/model ".into());
    }
    #[allow(clippy::too_many_lines)] // Finite wizard actions keep save intent and authority together.
    fn choose(&mut self, choice: Choice, default: bool) -> Outcome<()> {
        self.save_default = default
            || !self.attached
            || (matches!(choice, Choice::Effort(_) | Choice::EffortDefault(_))
                && self.save_default);
        match choice {
            Choice::Provider(provider) => self.deployments(provider),
            Choice::New(provider) => {
                let name = match provider {
                    ProviderKind::Deepseek => "deepseek",
                    ProviderKind::Openai => "openai",
                    ProviderKind::OpenaiCompatible => "compatible",
                };
                let endpoint = match provider {
                    ProviderKind::Deepseek => "https://api.deepseek.com",
                    ProviderKind::Openai => "https://api.openai.com",
                    ProviderKind::OpenaiCompatible => "",
                };
                let mut config = json!({"deployment":name,"endpoint":endpoint,"credential":{"owner":provider.owner(),"slot":name},"language_models":{}});
                match provider {
                    ProviderKind::Openai => {
                        config["language"] = json!(true);
                        config["image"] = json!(false);
                    }
                    ProviderKind::OpenaiCompatible => {
                        config["path"] = json!("/chat/completions");
                        config["allow_image_input"] = json!(false);
                    }
                    ProviderKind::Deepseek => {
                        config["protocol"] = json!("responses");
                    }
                }
                self.definition = Some(ManagedProvider { provider, config });
                self.existing = false;
                if self.name_exists(name) {
                    self.field("Deployment name · must be unique", Field::Name, name.into());
                } else {
                    self.finish_name(name)?;
                }
            }
            Choice::DiscoverExisting(definition) => {
                self.existing = true;
                self.definition = Some(definition);
                self.discovery_menu(vec![]);
            }
            Choice::Existing(definition) => {
                self.definition = Some(definition);
                self.existing = true;
                self.credentials()?;
            }
            Choice::Login => self.open(Command::Login(None), self.attached),
            Choice::Refresh => self.open(Command::Models, self.attached),
            Choice::Discover => self.discover(),
            Choice::Credentials => self.credentials()?,
            Choice::Connection => {
                self.editing_connection = true;
                let definition = self.definition.as_ref().ok_or("Choose provider")?;
                self.field(
                    "Deployment name",
                    Field::Name,
                    definition.config["deployment"]
                        .as_str()
                        .unwrap_or_default()
                        .into(),
                );
            }
            Choice::Manual => {
                self.declare_effort = true;
                self.field("Exact model ID", Field::Model, String::new());
            }
            Choice::Candidate(mut model) => {
                let request = self.request()?;
                if let Some(limits) = self
                    .definition
                    .as_ref()
                    .and_then(|entry| entry.config["language_models"].get(&model.id))
                    .cloned()
                {
                    let limits: LanguageModelLimits =
                        serde_json::from_value(limits).map_err(|error| error.to_string())?;
                    self.save(&model.id, limits)?;
                } else {
                    rsi_workbench_ui::supplement_model(&request, &mut model);
                    self.candidate = Some(model);
                    self.limits()?;
                }
            }
            Choice::Route(model) => self.describe_efforts(model),
            Choice::Effort(selection) => self.finish_selection(selection)?,
            Choice::EffortDefault(default) => {
                let id = self.candidate.as_ref().ok_or("Choose model")?.id.clone();
                let definition = self.definition.as_mut().ok_or("Choose provider")?;
                let old: rsi_ai_protocol::ReasoningEffortProfile =
                    serde_json::from_value(definition.config["reasoning_efforts"][&id].clone())
                        .map_err(|error| error.to_string())?;
                let profile =
                    rsi_ai_protocol::ReasoningEffortProfile::new(old.supported().to_vec(), default)
                        .map_err(|error| error.to_string())?;
                definition.config["reasoning_efforts"][&id] =
                    serde_json::to_value(profile).map_err(|error| error.to_string())?;
                self.save_declared_model()?;
            }
        }
        Ok(())
    }
    fn describe_efforts(&mut self, model: ModelRef) {
        let catalog = self.catalog.clone();
        if self.effort_selection.is_none() {
            self.composer_prefix = Some(format!("/model {} ", model.model()));
            self.editor = Editor::default();
            self.stage = Stage::Menu(vec![]);
            self.title = "Reasoning effort".into();
        }
        self.status = "Loading model capabilities…".into();
        self.pending = Some(Box::pin(async move {
            let profile = catalog
                .describe_model(&model)
                .await
                .map_err(|error| error.to_string())?;
            Ok(Update::Efforts(model, profile.into_profile()))
        }));
        self.mutation = false;
    }
    fn finish_selection(
        &mut self,
        selection: rsi_agent_session_protocol::ModelSelection,
    ) -> Outcome<()> {
        if self.save_default {
            let feature = self.feature.clone().ok_or("Setup unavailable")?;
            let ticket = self.view.ticket.clone();
            self.pending = Some(Box::pin(async move {
                feature
                    .command(SetupCommand::DefaultModel {
                        ticket,
                        model: selection.model.clone(),
                        reasoning_effort: selection.reasoning_effort.clone(),
                    })
                    .await?;
                Ok(Update::Selected(selection))
            }));
            self.mutation = true;
        } else {
            self.chosen = Some(selection);
            self.active = false;
        }
        Ok(())
    }
    fn limits(&mut self) -> Outcome<()> {
        let model = self.candidate.as_ref().ok_or("Choose model")?;
        match (model.context_window_tokens, model.max_output_tokens) {
            (None, _) => self.field(
                "Context window tokens · positive integer",
                Field::Context,
                String::new(),
            ),
            (_, None) => self.field(
                "Maximum output tokens · less than context window",
                Field::Output,
                String::new(),
            ),
            (Some(context), Some(output)) if output >= context => self.field(
                "Maximum output tokens · less than context window",
                Field::Output,
                String::new(),
            ),
            (Some(context), Some(output)) => {
                let limits = LanguageModelLimits::new(context, output.min(4096), output)
                    .map_err(|error| error.to_string())?;
                let id = model.id.clone();
                self.save(&id, limits)?;
            }
        }
        Ok(())
    }
    fn save(&mut self, id: &str, limits: LanguageModelLimits) -> Outcome<()> {
        if self.declare_effort {
            self.declare_effort = false;
            self.definition.as_mut().ok_or("Choose provider")?.config["language_models"][id] =
                serde_json::to_value(limits).map_err(|error| error.to_string())?;
            self.candidate = Some(DiscoveredModel {
                id: id.into(),
                name: None,
                context_window_tokens: None,
                max_output_tokens: None,
            });
            self.field(
                "Effort IDs · comma-separated · optional",
                Field::Efforts,
                String::new(),
            );
            return Ok(());
        }
        let mut definition = self.definition.clone().ok_or("Choose provider")?;
        let model = ModelRef::new(
            definition.config["deployment"]
                .as_str()
                .ok_or("Deployment name")?,
            id,
        )
        .map_err(|error| error.to_string())?;
        definition.config["language_models"][id] =
            serde_json::to_value(limits).map_err(|error| error.to_string())?;
        let feature = self.feature.clone().ok_or("Setup unavailable")?;
        let view = self.view.clone();
        self.pending = Some(Box::pin(async move {
            feature
                .save_model(view, definition, model.clone(), false)
                .await?;
            Ok(Update::Saved(model))
        }));
        self.mutation = true;
        self.status = "Saving provider configuration…".into();
        Ok(())
    }
    fn save_declared_model(&mut self) -> Outcome<()> {
        let id = self.candidate.as_ref().ok_or("Choose model")?.id.clone();
        let limits = serde_json::from_value(
            self.definition.as_ref().ok_or("Choose provider")?.config["language_models"][&id]
                .clone(),
        )
        .map_err(|error| error.to_string())?;
        self.save(&id, limits)
    }
    #[allow(clippy::too_many_lines)] // Each field validates before advancing the same setup transaction.
    fn submit_field(&mut self, field: Field) -> Outcome<()> {
        let value = self.editor.text().trim().to_owned();
        match field {
            Field::Efforts => {
                if value.is_empty() {
                    self.save_declared_model()?;
                    return Ok(());
                }
                let choices = value
                    .split(',')
                    .map(|id| rsi_ai_protocol::ReasoningEffortId::new(id.trim()))
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|error| error.to_string())?;
                let profile = rsi_ai_protocol::ReasoningEffortProfile::new(choices, None)
                    .map_err(|error| error.to_string())?;
                let id = self.candidate.as_ref().ok_or("Choose model")?.id.clone();
                self.definition.as_mut().ok_or("Choose provider")?.config["reasoning_efforts"]
                    [&id] = serde_json::to_value(&profile).map_err(|error| error.to_string())?;
                let mut items = vec![("Default unknown".into(), Choice::EffortDefault(None))];
                items.extend(
                    profile
                        .supported()
                        .iter()
                        .map(|id| (id.to_string(), Choice::EffortDefault(Some(id.clone())))),
                );
                self.menu("Declared default effort", items);
            }
            Field::Name => {
                ModelRef::new(&value, "model").map_err(|error| error.to_string())?;
                let current = self
                    .definition
                    .as_ref()
                    .and_then(|d| d.config["deployment"].as_str())
                    .unwrap_or_default();
                if self.name_exists(&value) && !(self.existing && value == current) {
                    return Err("Deployment name already exists; enter a different name".into());
                }
                self.finish_name(&value)?;
            }
            Field::Endpoint => {
                let mut request = self.request().or_else(|_| {
                    let d = self.definition.as_ref().ok_or("Choose provider")?;
                    Ok::<_, String>(DiscoveryRequest {
                        provider: d.provider,
                        endpoint: value.clone(),
                        slot: d.config["credential"]["slot"]
                            .as_str()
                            .unwrap_or("default")
                            .into(),
                    })
                })?;
                request.endpoint.clone_from(&value);
                request.validate().map_err(|error| error.to_string())?;
                self.definition.as_mut().ok_or("Choose provider")?.config["endpoint"] =
                    json!(value);
                self.credentials()?;
            }
            Field::Model => {
                ModelRef::new("model", &value).map_err(|error| error.to_string())?;
                self.choose(
                    Choice::Candidate(DiscoveredModel {
                        id: value,
                        name: None,
                        context_window_tokens: None,
                        max_output_tokens: None,
                    }),
                    self.save_default,
                )?;
            }
            Field::Context | Field::Output => {
                let number = value
                    .parse::<u32>()
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or("Enter a positive 32-bit integer")?;
                let mut model = self.candidate.clone().ok_or("Choose model")?;
                if matches!(field, Field::Context) {
                    if number < 2 {
                        return Err(
                            "Context must be at least 2 tokens and greater than output".into()
                        );
                    }
                    model.context_window_tokens = Some(number);
                    // Editing context keeps the output editable before final validation.
                    self.candidate = Some(model);
                    self.field(
                        "Maximum output tokens · less than context window",
                        Field::Output,
                        self.candidate
                            .as_ref()
                            .and_then(|m| m.max_output_tokens)
                            .map_or(String::new(), |n| n.to_string()),
                    );
                    return Ok(());
                }
                let context = model
                    .context_window_tokens
                    .ok_or("Enter context window first")?;
                LanguageModelLimits::new(context, number.min(4096), number)
                    .map_err(|error| error.to_string())?;
                model.max_output_tokens = Some(number);
                self.candidate = Some(model);
                self.limits()?;
            }
        }
        Ok(())
    }
    fn name_exists(&self, name: &str) -> bool {
        self.deployments.contains(name)
            || self.view.providers.as_ref().is_some_and(|v| {
                v.deployments
                    .iter()
                    .any(|d| d.config["deployment"].as_str() == Some(name))
            })
    }
    fn finish_name(&mut self, name: &str) -> Outcome<()> {
        let definition = self.definition.as_mut().ok_or("Choose provider")?;
        let old = definition.config["deployment"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        definition.config["deployment"] = json!(name);
        if !self.existing || name != old {
            definition.config["credential"]["slot"] =
                json!(if self.view.providers.as_ref().is_some_and(|view| view
                    .deployments
                    .iter()
                    .any(|d| d.provider == definition.provider))
                {
                    name
                } else {
                    "default"
                });
        }
        if definition.provider == ProviderKind::OpenaiCompatible
            || self.existing
            || self.editing_connection
        {
            let endpoint = definition.config["endpoint"]
                .as_str()
                .unwrap_or_default()
                .into();
            self.field(
                "API base URL · compatible services include /v1 if needed",
                Field::Endpoint,
                endpoint,
            );
        } else {
            self.credentials()?;
        }
        Ok(())
    }
    fn credentials(&mut self) -> Outcome<()> {
        self.secret.zeroize();
        self.credential = None;
        self.status.clear();
        let request = self.request()?;
        let feature = self.feature.clone().ok_or("Setup unavailable")?;
        self.stage = Stage::Secret;
        self.title = "API key · masked".into();
        self.composer_prefix = None;
        self.editor = Editor::default();
        self.pending = Some(Box::pin(async move {
            feature
                .command(SetupCommand::CredentialStatus {
                    provider: request.provider,
                    slot: request.slot,
                })
                .await?;
            Ok(Update::CredentialStatus)
        }));
        self.mutation = false;
        Ok(())
    }
    fn reconcile(&mut self) {
        self.back.clear();
        self.definition = None;
        self.candidate = None;
        self.credential = None;
    }
    pub fn attachment_changed(&mut self) {
        self.close();
    }
    fn screen(&self) -> Screen {
        Screen {
            composer_prefix: self.composer_prefix.clone(),
            declare_effort: self.declare_effort,
            stage: self.stage.clone(),
            title: self.title.clone(),
            editor: self.editor.clone(),
            selected: self.selected,
            candidate: self.candidate.clone(),
            definition: self.definition.clone(),
            existing: self.existing,
            editing_connection: self.editing_connection,
        }
    }
    fn close(&mut self) {
        self.followup = false;
        self.chosen = None;
        if !(self.mutation && self.pending.is_some()) {
            self.status.clear();
        }
        if !self.mutation {
            self.pending = None;
        }
        self.secret.zeroize();
        self.editor = Editor::default();
        self.back.clear();
        self.active = false;
    }
    fn back(&mut self) {
        if self.integration.is_some() {
            self.close();
            return;
        }
        if self.mutation && self.pending.is_some() {
            self.close();
            return;
        }
        self.pending = None;
        self.secret.zeroize();
        self.status.clear();
        if let Some(screen) = self.back.pop() {
            self.composer_prefix = screen.composer_prefix;
            self.menu_revision = self.menu_revision.wrapping_add(1);
            self.declare_effort = screen.declare_effort;
            self.stage = screen.stage;
            self.title = screen.title;
            self.editor = screen.editor;
            self.selected = screen.selected;
            self.candidate = screen.candidate;
            self.definition = screen.definition;
            self.existing = screen.existing;
            self.editing_connection = screen.editing_connection;
            self.credential = None;
            if matches!(self.stage, Stage::Secret)
                && let Err(problem) = self.credentials()
            {
                self.status = problem;
            }
        } else {
            self.close();
        }
    }
    fn retrying_credential_status(&self) -> bool {
        matches!(self.stage, Stage::Secret)
            && self.credential.as_ref().is_some_and(|status| {
                matches!(
                    status.availability,
                    CredentialAvailability::Unavailable { .. }
                )
            })
    }
    fn indices(&self) -> Vec<usize> {
        let query = self.editor.text().to_lowercase();
        match &self.stage {
            Stage::Menu(items) => items
                .iter()
                .enumerate()
                .filter(|(_, (label, _))| label.to_lowercase().contains(&query))
                .map(|(index, _)| index)
                .collect(),
            _ => vec![],
        }
    }
    #[allow(clippy::too_many_lines)] // Focused-stage key handling and its backstack advance must agree.
    pub fn key(&mut self, key: KeyEvent) {
        let control = key.modifiers.contains(Modifiers::CONTROL);
        if control && key.code == KeyCode::Char('c') {
            self.close();
            return;
        }
        if key.code == KeyCode::Escape {
            self.back();
            return;
        }
        if self.pending.is_some() {
            return;
        }
        if control && key.code == KeyCode::Char('e') && self.definition.is_some() {
            let screen = self.screen();
            self.secret.zeroize();
            if let Err(problem) = self.choose(Choice::Connection, false) {
                self.status = problem;
            } else if self.back.len() < 16 {
                self.back.push(screen);
            }
            return;
        }
        let indices = self.indices();
        let advance = key.code == KeyCode::Enter
            || (key.code == KeyCode::Tab
                && matches!(&self.stage, Stage::Menu(items)
                if indices.get(self.selected).and_then(|i| items.get(*i)).is_some_and(|(_, choice)| matches!(choice, Choice::Route(_)))))
            || (control && matches!(key.code, KeyCode::Char('s' | 'S')));
        let screen = (advance && !self.retrying_credential_status()).then(|| self.screen());
        if !advance {
            self.status.clear();
        }
        let result = match &mut self.stage {
            Stage::Menu(items) => match key.code {
                KeyCode::Tab => {
                    let selected = indices
                        .get(self.selected)
                        .and_then(|index| items.get(*index))
                        .cloned();
                    match selected {
                        Some((_, choice @ Choice::Route(_))) => self.choose(choice, false),
                        Some((label, _)) => {
                            self.selected = 0;
                            self.editor.replace_text(&label).map_err(str::to_owned)
                        }
                        None => Ok(()),
                    }
                }
                KeyCode::Up => {
                    self.selected = self.selected.saturating_sub(1);
                    Ok(())
                }
                KeyCode::Down => {
                    self.selected = (self.selected + 1).min(indices.len().saturating_sub(1));
                    Ok(())
                }
                KeyCode::Enter | KeyCode::Char('s' | 'S')
                    if key.code == KeyCode::Enter || control =>
                {
                    let choice = indices
                        .get(self.selected)
                        .and_then(|index| items.get(*index))
                        .map(|(_, choice)| choice.clone());
                    choice.map_or(Ok(()), |choice| self.choose(choice, control))
                }
                _ => {
                    self.selected = 0;
                    self.editor.key(key).map_err(str::to_owned)
                }
            },
            Stage::Text(field) => {
                if key.code == KeyCode::Enter {
                    let field = *field;
                    self.submit_field(field)
                } else {
                    self.editor.key(key).map_err(str::to_owned)
                }
            }
            Stage::Secret => match key.code {
                KeyCode::Enter => self.submit_secret(),
                KeyCode::Char('u' | 'U') if control => {
                    self.secret.zeroize();
                    Ok(())
                }
                KeyCode::Backspace => {
                    self.secret.pop();
                    Ok(())
                }
                KeyCode::Char(ch) if !control && !ch.is_control() => {
                    let mut bytes = Zeroizing::new([0; 4]);
                    self.secret_insert(ch.encode_utf8(&mut *bytes))
                }
                _ => Ok(()),
            },
        };
        if let Err(problem) = result {
            self.status = problem;
        } else if let Some(screen) = screen
            && self.back.len() < 16
        {
            self.back.push(screen);
        }
    }
    fn submit_secret(&mut self) -> Outcome<()> {
        if self.integration.is_some() {
            return self.submit_integration_secret();
        }
        if self.credential.as_ref().is_some_and(|status| {
            matches!(
                status.availability,
                CredentialAvailability::Unavailable { .. }
            )
        }) {
            return self.credentials();
        }
        if self.secret.is_empty() {
            if !self.credential.as_ref().is_some_and(|status| {
                matches!(
                    status.availability,
                    CredentialAvailability::Configured { .. }
                )
            }) {
                return Err(
                    "No available credential. Enter a key or edit connection settings (Ctrl+E)."
                        .into(),
                );
            }
            self.discover();
            return Ok(());
        }
        let request = self.request()?;
        let secret = SecretValue::new(std::mem::take(&mut *self.secret))
            .map_err(|_| "Invalid API key".to_owned())?;
        let feature = self.feature.clone().ok_or("Setup unavailable")?;
        self.pending = Some(Box::pin(async move {
            feature
                .command(SetupCommand::CredentialSet {
                    provider: request.provider,
                    slot: request.slot,
                    secret,
                })
                .await?;
            Ok(Update::Credential)
        }));
        self.mutation = true;
        self.status = "Saving credential…".into();
        Ok(())
    }
    fn secret_insert(&mut self, value: &str) -> Outcome<()> {
        if self.integration.is_some()
            && !self
                .credential
                .as_ref()
                .is_some_and(|status| status.editable)
        {
            return Err("Read an editable credential status before entering a key".into());
        }
        if self
            .credential
            .as_ref()
            .is_some_and(|status| !status.editable)
        {
            return Err(
                "Credential is read only; edit its environment source or connection settings."
                    .into(),
            );
        }
        if self.secret.len().saturating_add(value.len()) > SECRET_BYTES
            || value.contains(char::is_control)
        {
            return Err("API key must be a single line, at most 64 KiB".into());
        }
        // Also re-reserve after submit_secret moves the allocation into SecretValue.
        // Reserve while empty, before there are secret bytes to leave in an old buffer.
        if self.secret.is_empty() {
            self.secret.reserve(SECRET_BYTES);
        }
        self.secret.push_str(value);
        Ok(())
    }
    pub fn paste(&mut self, mut text: String) {
        if self.pending.is_some() {
            self.status = "Paste ignored while an operation is pending; wait for completion before entering text.".into();
        } else {
            self.status.clear();
            self.selected = 0;
            let result = if matches!(self.stage, Stage::Secret) {
                self.secret_insert(text.trim())
            } else if matches!(self.stage, Stage::Text(_) | Stage::Menu(_)) {
                self.editor.insert(&text).map_err(str::to_owned)
            } else {
                Ok(())
            };
            if let Err(problem) = result {
                self.status = problem;
            }
        }
        text.zeroize();
    }
    pub fn notice(&self) -> String {
        if self.pending.is_some() && self.mutation {
            "Save pending; reopen setup to observe.".into()
        } else if !self.followup {
            self.status.clone()
        } else {
            String::new()
        }
    }
    fn revision(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.attempt.hash(&mut hash);
        self.menu_revision.hash(&mut hash);
        self.title.hash(&mut hash);
        self.editor.text().hash(&mut hash);
        self.selected.hash(&mut hash);
        hash.finish()
    }
    pub fn mouse(
        &mut self,
        mouse: termina::event::MouseEvent,
        view: &rsi_terminal_ui::render::View,
    ) {
        if self.pending.is_some()
            || !matches!(self.stage, Stage::Menu(_))
            || view.choice_revision != self.revision()
            || !view.choices_area.contains((mouse.column, mouse.row).into())
        {
            return;
        }
        let indices = self.indices();
        match mouse.kind {
            MouseEventKind::ScrollUp => self.selected = self.selected.saturating_sub(1),
            MouseEventKind::ScrollDown => {
                self.selected = (self.selected + 1).min(indices.len().saturating_sub(1));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = view.choice_at(mouse.column, mouse.row) {
                    let from = self
                        .selected
                        .saturating_sub(128)
                        .min(indices.len().saturating_sub(256));
                    self.selected = (from + index).min(indices.len().saturating_sub(1));
                }
            }
            _ => {}
        }
    }
    #[allow(clippy::too_many_lines)] // Redacts independent status, receipt, editor and bounded menu sources.
    pub fn scene(&self) -> std::result::Result<Scene, &'static str> {
        let input = if matches!(self.stage, Stage::Secret) {
            let text = if self.secret.is_empty() {
                String::new()
            } else {
                "•".repeat(8)
            };
            rsi_terminal_ui::scene::Draft {
                cursor: text.len(),
                text,
            }
        } else {
            rsi_terminal_ui::scene::Draft::capture(&self.editor)?
        };
        let indices = self.indices();
        let from = self
            .selected
            .saturating_sub(128)
            .min(indices.len().saturating_sub(256));
        let items = if let Stage::Menu(items) = &self.stage {
            indices
                .iter()
                .skip(from)
                .take(256)
                .map(|index| items[*index].0.clone())
                .collect()
        } else {
            vec![]
        };
        let selected = self.selected.saturating_sub(from);
        let summary = self
            .definition
            .as_ref()
            .map(|d| {
                format!(
                    "{} · {}",
                    d.config["deployment"].as_str().unwrap_or_default(),
                    d.config["endpoint"].as_str().unwrap_or_default()
                )
            })
            .unwrap_or_default();
        let credential = self.credential.as_ref().map_or_else(
            || "Checking credential…".into(),
            |status| match &status.availability {
                CredentialAvailability::Missing => "Missing credential · enter key".into(),
                CredentialAvailability::Unavailable { reason } => format!("{reason}.\n{}\nEnter retries; Esc returns.", match reason {
                    rsi_credentials_protocol::CredentialStoreFailure::Permissions => "Check ownership; the directory needs mode 0700 and the file needs 0600.",
                    rsi_credentials_protocol::CredentialStoreFailure::Corrupt => "Repair the credential JSON; the original file has been preserved.",
                    rsi_credentials_protocol::CredentialStoreFailure::UnsafePath => "Use an owned directory and regular file without links.",
                    _ => "Resolve the file problem before saving or using this credential.",
                }),
                CredentialAvailability::Configured { source } => match source {
                    rsi_credentials_protocol::CredentialSource::Environment { .. } => {
                        "Environment key · Enter reuses; typing saves a replacement".into()
                    }
                    rsi_credentials_protocol::CredentialSource::File | rsi_credentials_protocol::CredentialSource::Keyring => if status.editable {
                        "Saved key · Enter reuses; typing replaces"
                    } else {
                        "Saved key · Enter reuses (read only)"
                    }
                    .into(),
                },
            },
        );
        let credential = if let Some(path) = self
            .credential
            .as_ref()
            .and_then(|status| status.store_path.as_deref())
        {
            format!("{credential}\nSaves on Host: {path}")
        } else {
            credential
        };
        let credential = if self.integration.is_some() {
            match &self.credential {
                Some(status) if status.editable => format!(
                    "{} · enter a nonempty key to save; tool settings are separate",
                    if matches!(
                        status.availability,
                        CredentialAvailability::Configured { .. }
                    ) {
                        "Credential configured"
                    } else {
                        "Credential missing"
                    }
                ),
                Some(_) => {
                    "Credential is read only or unavailable; close and reopen to refresh status"
                        .into()
                }
                None if self.pending.is_some() => {
                    "Reading or saving the selected credential…".into()
                }
                None => "Credential status needs an explicit refresh; close and reopen this screen"
                    .into(),
            }
        } else {
            credential
        };
        let receipts = self
            .view
            .receipts
            .iter()
            .map(|receipt| format!("{}: {}", receipt.operation, receipt.outcome))
            .collect();

        Ok(Scene::from(ApplicationScene {
            composer_prefix: self.composer_prefix.clone(),
            revision: self.revision(),
            title: if self.composer_prefix.is_some() {
                if indices.len() > 8 {
                    format!("{} · {}/{}", self.title, self.selected + 1, indices.len())
                } else {
                    format!("{} · {}", self.title, indices.len())
                }
            } else {
                self.title.clone()
            },
            explanation: if let Some(CredentialStatus {
                availability:
                    CredentialAvailability::Configured {
                        source: rsi_credentials_protocol::CredentialSource::Environment { variable },
                    },
                ..
            }) = &self.credential
            {
                format!("{summary} · {variable}")
            } else {
                summary
            },
            items,
            selected,
            input,
            status: self.status.clone(),
            detail: matches!(self.stage, Stage::Secret).then_some(credential),
            field: match self.stage {
                Stage::Secret => self
                    .credential
                    .as_ref()
                    .filter(|status| status.editable)
                    .map(|_| "API key".into()),
                Stage::Menu(_) => Some("Filter choices".into()),
                Stage::Text(_) => Some(String::new()),
            },
            progress: self.pending.as_ref().map(|_| {
                format!(
                    "{} · attempt {}",
                    if self.mutation {
                        "Saving; Ctrl+C closes while result is retained"
                    } else {
                        "Loading…"
                    },
                    self.attempt
                )
            }),
            receipts,
            hint: match self.stage {
                Stage::Secret if self.integration.is_some() => {
                    "Enter save · Esc returns to Plugins · Ctrl+C close"
                }
                Stage::Secret
                    if self.credential.as_ref().is_some_and(|status| {
                        matches!(
                            status.availability,
                            CredentialAvailability::Unavailable { .. }
                        )
                    }) =>
                {
                    "Enter retry · Esc back · Ctrl+C close"
                }
                Stage::Secret => "Enter continue · Esc back · Ctrl+E edit",
                Stage::Menu(_) => "Enter select · ^S default · Esc back",
                Stage::Text(_) => "Enter next · Esc back · ^C close",
            }
            .into(),
            ..ApplicationScene::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inline_count_describes_the_filtered_catalog_not_the_display_window() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.menu(
            "Models",
            (0..300)
                .map(|i| {
                    (
                        format!("model-{i:03}"),
                        Choice::Route(ModelRef::new("test", format!("model-{i}")).unwrap()),
                    )
                })
                .collect(),
        );
        ui.composer_prefix = Some("/model ".into());
        ui.selected = 280;
        let scene = ui.scene().unwrap();
        let rsi_terminal_ui::scene::Surface::Application(scene) = scene.surface else {
            panic!("application")
        };
        assert_eq!(scene.title, "Models · 281/300");
        assert_eq!(scene.items.len(), 256);
        ui.editor.replace_text("model-29").unwrap();
        ui.selected = 0;
        let scene = ui.scene().unwrap();
        let rsi_terminal_ui::scene::Surface::Application(scene) = scene.surface else {
            panic!("application")
        };
        assert_eq!(scene.title, "Models · 1/10");
    }
    #[tokio::test]
    async fn direct_effort_picker_preserves_model_highlights_choice_and_clears_override() {
        let model = ModelRef::new("test", "current").unwrap();
        let high = rsi_ai_protocol::ReasoningEffortId::new("high").unwrap();
        for supported in [vec![], vec![high.clone()]] {
            let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
            ui.active = true;
            ui.attached = true;
            ui.effort_selection = Some(rsi_agent_session_protocol::ModelSelection {
                model: model.clone(),
                reasoning_effort: if supported.is_empty() {
                    None
                } else {
                    Some(high.clone())
                },
            });
            let profile = rsi_ai_protocol::LanguageProfile::new(
                8192,
                1024,
                4096,
                rsi_ai_protocol::ToolDialect::ChatCompletions,
                false,
                rsi_ai_protocol::ImageToolResultCapability::No,
                vec![],
            )
            .unwrap()
            .with_reasoning_efforts(
                rsi_ai_protocol::ReasoningEffortProfile::new(
                    supported.clone(),
                    supported.first().cloned(),
                )
                .unwrap(),
            );
            let route = model.clone();
            ui.pending = Some(Box::pin(async move { Ok(Update::Efforts(route, profile)) }));
            ui.next().await;
            let Stage::Menu(choices) = &ui.stage else {
                panic!("effort choices")
            };
            assert_eq!(choices.len(), supported.len() + 1);
            assert_eq!(ui.selected, supported.len());
            assert!(
                ui.chosen.is_none(),
                "reading effort choices must not mutate selection"
            );
            let label = choices[ui.selected].0.clone();
            let default = choices[0].1.clone();
            ui.key(KeyCode::Tab.into());
            assert_eq!(ui.editor.text(), label);
            assert!(
                ui.chosen.is_none(),
                "Tab fills a choice without applying it"
            );
            assert_eq!(ui.composer_prefix, Some("/effort ".into()));
            ui.choose(default, false).unwrap();
            let selected = ui.chosen.take().unwrap();
            assert_eq!(selected.model, model);
            assert_eq!(selected.reasoning_effort, None);
            assert!(ui.pending.is_none());
            assert!(!ui.active);
        }
    }

    #[tokio::test]
    async fn model_selection_requires_effort_and_escape_restores_model_filter() {
        for key in [KeyCode::Enter, KeyCode::Tab] {
            for supported in [
                vec![],
                vec![rsi_ai_protocol::ReasoningEffortId::new("high").unwrap()],
            ] {
                let model = ModelRef::new("test", "model-a").unwrap();
                let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
                ui.active = true;
                ui.attached = true;
                ui.menu(
                    "Choose model",
                    vec![("model-a".into(), Choice::Route(model.clone()))],
                );
                ui.composer_prefix = Some("/model ".into());
                ui.editor.replace_text("model-a").unwrap();
                ui.key(key.into());
                assert!(ui.pending.is_some());
                assert!(ui.chosen.is_none());
                assert_eq!(ui.composer_prefix.as_deref(), Some("/model model-a "));
                let profile = rsi_ai_protocol::LanguageProfile::new(
                    8192,
                    1024,
                    4096,
                    rsi_ai_protocol::ToolDialect::ChatCompletions,
                    false,
                    rsi_ai_protocol::ImageToolResultCapability::No,
                    vec![],
                )
                .unwrap()
                .with_reasoning_efforts(
                    rsi_ai_protocol::ReasoningEffortProfile::new(supported.clone(), None).unwrap(),
                );
                let route = model.clone();
                ui.pending = Some(Box::pin(async move { Ok(Update::Efforts(route, profile)) }));
                ui.next().await;
                assert!(ui.chosen.is_none());
                assert_eq!(ui.indices().len(), supported.len() + 1);
                ui.key(KeyCode::Tab.into());
                assert!(ui.chosen.is_none());
                ui.key(KeyCode::Escape.into());
                assert_eq!(ui.title, "Choose model");
                assert_eq!(ui.editor.text(), "model-a");
                assert_eq!(ui.composer_prefix.as_deref(), Some("/model "));
                assert!(ui.chosen.is_none());
                ui.key(KeyCode::Escape.into());
                assert!(!ui.active);
            }
        }
    }

    #[test]
    fn full_context_output_capacity_requires_an_explicit_smaller_execution_limit() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.candidate = Some(DiscoveredModel {
            id: "model".into(),
            name: None,
            context_window_tokens: Some(10),
            max_output_tokens: Some(10),
        });
        ui.limits().unwrap();
        assert!(matches!(ui.stage, Stage::Text(Field::Output)));
        assert!(ui.title.contains("less than context"));
        assert!(ui.editor.text().is_empty());
        assert!(ui.pending.is_none());
        assert_eq!(ui.candidate.unwrap().max_output_tokens, Some(10));
    }
    #[test]
    fn unavailable_credentials_show_recovery_without_an_editable_cursor() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.stage = Stage::Secret;
        ui.credential = Some(CredentialStatus {
            availability: CredentialAvailability::Unavailable {
                reason: rsi_credentials_protocol::CredentialStoreFailure::Permissions,
            },
            editable: false,
            store_path: Some("/host/config/credentials/credentials.json".into()),
        });
        let rsi_terminal_ui::scene::Surface::Application(scene) = ui.scene().unwrap().surface
        else {
            panic!("application")
        };
        assert!(scene.field.is_none());
        assert!(scene.detail.as_ref().unwrap().contains("permissions"));
        assert!(scene.hint.contains("retry"));
        ui.key(KeyCode::Enter.into());
        assert!(!ui.status.contains("Enter a key"));
        assert!(ui.pending.is_none());
        assert!(!ui.mutation);
        assert!(ui.secret.is_empty());
    }
    #[tokio::test]
    async fn closed_write_is_observed_on_reopen_without_followup_or_replay() {
        for fail in [false, true] {
            let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
            ui.active = true;
            ui.mutation = true;
            let (send, receive) = tokio::sync::oneshot::channel();
            ui.pending = Some(Box::pin(async move {
                receive.await.unwrap();
                if fail {
                    Err("Outcome unknown".into())
                } else {
                    Ok(Update::Saved(ModelRef::new("test", "model").unwrap()))
                }
            }));
            let mut close: KeyEvent = KeyCode::Char('c').into();
            close.modifiers = Modifiers::CONTROL;
            if fail {
                ui.attachment_changed();
            } else {
                ui.key(close);
            }
            assert!(!ui.active);
            assert!(ui.pending.is_some());
            assert!(ui.notice().contains("Save pending"));
            ui.open(Command::Models, true);
            assert!(ui.active);
            assert!(ui.pending.is_some());
            send.send(()).unwrap();
            ui.next().await;
            assert!(ui.chosen.is_none());
            assert!(ui.pending.is_none());
            assert!(
                matches!(&ui.stage,Stage::Menu(items) if matches!(items.as_slice(),[(_,Choice::Refresh)]))
            );
            assert!(ui.status.contains(if fail {
                "Outcome unknown"
            } else {
                "no follow-up"
            }));
            assert_eq!(ui.notice(), ui.status);
        }
    }
    #[test]
    fn closing_setup_without_a_pending_save_does_not_repeat_screen_status() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.active = true;
        ui.status = "Loading configuration…".into();
        ui.close();
        assert!(ui.notice().is_empty());
    }
    #[test]
    fn connection_shortcut_returns_to_secret_without_recovering_unsaved_key() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.definition = Some(ManagedProvider {
            provider: ProviderKind::Deepseek,
            config: json!({"deployment":"deepseek","endpoint":"https://api.deepseek.com","credential":{"slot":"default"}}),
        });
        ui.stage = Stage::Secret;
        ui.active = true;
        ui.secret_insert("unsaved-fixture-key").unwrap();
        let mut edit: KeyEvent = KeyCode::Char('e').into();
        edit.modifiers = Modifiers::CONTROL;
        ui.key(edit);
        assert!(matches!(ui.stage, Stage::Text(Field::Name)));
        assert!(ui.secret.is_empty());
        ui.key(KeyCode::Escape.into());
        assert!(matches!(ui.stage, Stage::Secret));
        assert!(ui.secret.is_empty());
        assert!(ui.credential.is_none());
    }
    #[test]
    fn back_from_renaming_an_existing_connection_restores_its_definition() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        let definition = ManagedProvider {
            provider: ProviderKind::OpenaiCompatible,
            config: json!({"deployment":"original","endpoint":"http://127.0.0.1/v1","credential":{"slot":"default"}}),
        };
        ui.view.providers = Some(rsi_configuration_api::ProvidersSnapshot {
            desired_revision: "0".into(),
            applied_revision: "0".into(),
            applying: false,
            diagnostic: None,
            deployments: vec![definition.clone()],
        });
        ui.definition = Some(definition.clone());
        ui.existing = true;
        ui.editing_connection = true;
        ui.field("Deployment name", Field::Name, "renamed".into());
        ui.key(KeyCode::Enter.into());
        assert_eq!(
            ui.definition.as_ref().unwrap().config["deployment"],
            "renamed"
        );
        ui.key(KeyCode::Escape.into());
        assert_eq!(ui.definition.as_ref().unwrap().config, definition.config);
        ui.editor.replace_text("original").unwrap();
        ui.key(KeyCode::Enter.into());
        assert!(matches!(ui.stage, Stage::Text(Field::Endpoint)));
        assert!(ui.status.is_empty());
        assert_eq!(
            ui.definition.as_ref().unwrap().config["credential"]["slot"],
            "default"
        );
    }
    #[test]
    fn credential_availability_and_secret_paste_follow_effective_source() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.stage = Stage::Secret;
        ui.credential = Some(CredentialStatus {
            availability: CredentialAvailability::Missing,
            editable: true,
            store_path: Some("/host/config/credentials/credentials.json".into()),
        });
        assert!(ui.submit_secret().unwrap_err().contains("No available"));
        ui.paste("  fixture-key\n".into());
        assert_eq!(&*ui.secret, "fixture-key");
        ui.secret.zeroize();
        ui.paste("bad\nkey".into());
        assert!(ui.secret.is_empty());
        ui.credential = Some(CredentialStatus {
            availability: CredentialAvailability::Unavailable {
                reason: rsi_credentials_protocol::CredentialStoreFailure::Permissions,
            },
            editable: false,
            store_path: Some("/host/config/credentials/credentials.json".into()),
        });
        ui.paste("unaccepted".into());
        assert!(ui.secret.is_empty());
        assert!(ui.status.contains("read only"));
    }
    #[test]
    fn capacity_error_can_return_to_context_and_does_not_mutate_invalid_output() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.candidate = Some(DiscoveredModel {
            id: "unknown".into(),
            name: None,
            context_window_tokens: None,
            max_output_tokens: None,
        });
        ui.field("Context", Field::Context, "10".into());
        ui.key(KeyCode::Enter.into());
        ui.paste("11".into());
        ui.key(KeyCode::Enter.into());
        assert!(ui.candidate.as_ref().unwrap().max_output_tokens.is_none());
        assert!(!ui.status.is_empty());
        ui.key(KeyCode::Escape.into());
        assert!(matches!(ui.stage, Stage::Text(Field::Context)));
        assert_eq!(ui.editor.text(), "10");
        assert!(ui.status.is_empty());
        ui.editor.replace_text("100").unwrap();
        ui.key(KeyCode::Enter.into());
        assert_eq!(
            ui.candidate.as_ref().unwrap().context_window_tokens,
            Some(100)
        );
    }
    #[test]
    fn regression_secret_clear_and_field_error_lifetime() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.stage = Stage::Secret;
        ui.paste("fixture-private-key".into());
        let mut key: KeyEvent = KeyCode::Char('u').into();
        key.modifiers = Modifiers::CONTROL;
        ui.key(key);
        assert!(ui.secret.is_empty(), "Ctrl+U must erase the secret");
        ui.status = "old URL error".into();
        ui.field("Model", Field::Model, String::new());
        assert!(ui.status.is_empty(), "errors must not cross steps");
    }
    #[test]
    fn regression_multiline_application_command_is_literal() {
        assert!(command("/login\ndeepseek").is_none());
        assert!(command("/model\n").is_none());
    }

    #[test]
    fn manual_effort_declaration_is_optional_bounded_and_keeps_exact_ids() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.attached = true;
        ui.save_default = true;
        ui.declare_effort = true;
        ui.definition = Some(ManagedProvider {
            provider: ProviderKind::OpenaiCompatible,
            config: json!({"deployment":"manual","language_models":{}}),
        });
        ui.save(
            "unknown",
            LanguageModelLimits::new(8192, 1024, 2048).unwrap(),
        )
        .unwrap();
        assert!(matches!(ui.stage, Stage::Text(Field::Efforts)));
        assert!(ui.pending.is_none());
        for text in [
            "low,low",
            "高",
            "bad effort",
            &"e".repeat(33),
            &(0..17)
                .map(|i| format!("e{i}"))
                .collect::<Vec<_>>()
                .join(","),
        ] {
            ui.editor.replace_text(text).unwrap();
            assert!(ui.submit_field(Field::Efforts).is_err());
        }
        ui.editor.replace_text("off, max").unwrap();
        ui.submit_field(Field::Efforts).unwrap();
        let Stage::Menu(items) = &ui.stage else {
            panic!("declared defaults")
        };
        assert_eq!(
            items
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["Default unknown", "off", "max"]
        );
        let result = ui.choose(
            Choice::EffortDefault(Some(
                rsi_ai_protocol::ReasoningEffortId::new("max").unwrap(),
            )),
            false,
        );
        assert!(
            result.unwrap_err().contains("Setup unavailable"),
            "this isolated form has no write capability"
        );
        assert!(ui.save_default);
        let profile: rsi_ai_protocol::ReasoningEffortProfile = serde_json::from_value(
            ui.definition.as_ref().unwrap().config["reasoning_efforts"]["unknown"].clone(),
        )
        .unwrap();
        assert_eq!(profile.default_effort().unwrap().as_str(), "max");
        assert!(
            profile
                .resolve(Some(
                    &rsi_ai_protocol::ReasoningEffortId::new("xhigh").unwrap()
                ))
                .is_err()
        );
    }
    #[tokio::test]
    async fn reopening_and_entering_a_manual_model_does_not_reuse_default_save_intent() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.active = true;
        ui.attached = true;
        ui.discovery_menu(vec![]);
        ui.selected = 1;
        let mut save: KeyEvent = KeyCode::Char('s').into();
        save.modifiers = Modifiers::CONTROL;
        ui.key(save);
        assert!(ui.save_default);
        ui.key(KeyCode::Escape.into());
        ui.open(Command::Models, true);
        ui.view.providers = Some(rsi_configuration_api::ProvidersSnapshot {
            desired_revision: "0".into(),
            applied_revision: "0".into(),
            applying: false,
            diagnostic: None,
            deployments: vec![ManagedProvider {
                provider: ProviderKind::OpenaiCompatible,
                config: json!({"deployment":"test", "endpoint":"http://127.0.0.1/v1", "credential":{"slot":"default"}}),
            }],
        });
        ui.pending = Some(Box::pin(async {
            Ok(Update::Open(Command::Models, vec![]))
        }));
        ui.next().await;
        ui.key(KeyCode::Enter.into()); // Discover existing deployment.
        ui.key(KeyCode::Down.into());
        ui.key(KeyCode::Enter.into()); // Manual entry with ordinary Enter.
        ui.paste("unknown-model".into());
        ui.key(KeyCode::Enter.into());
        assert!(matches!(ui.stage, Stage::Text(Field::Context)));
        assert!(!ui.save_default);
    }
    #[test]
    fn a_replaced_menu_rejects_hits_from_the_previous_contents() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.menu(
            "Models",
            vec![
                ("old-a".into(), Choice::Refresh),
                ("old-b".into(), Choice::Refresh),
            ],
        );
        let (_, old_view) = ui.scene().unwrap().render(42, 12).unwrap();
        let row = old_view
            .choices
            .iter()
            .find(|(_, index)| *index == 1)
            .unwrap()
            .0;
        ui.menu(
            "Models",
            vec![
                ("new-a".into(), Choice::Refresh),
                ("new-b".into(), Choice::Refresh),
            ],
        );
        ui.mouse(
            termina::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row,
                modifiers: Modifiers::empty(),
            },
            &old_view,
        );
        assert_eq!(
            ui.selected, 0,
            "a stale rendered row must not select the replacement menu"
        );
        let (_, current_view) = ui.scene().unwrap().render(42, 12).unwrap();
        ui.mouse(
            termina::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row,
                modifiers: Modifiers::empty(),
            },
            &current_view,
        );
        assert_eq!(ui.selected, 1);
    }
    #[tokio::test]
    async fn full_configured_catalog_plus_deployment_actions_fits_application_scene() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.view.providers = Some(rsi_configuration_api::ProvidersSnapshot {
            desired_revision: "0".into(),
            applied_revision: "0".into(),
            applying: false,
            diagnostic: None,
            deployments: (0..64)
                .map(|i| ManagedProvider {
                    provider: ProviderKind::Deepseek,
                    config: json!({"deployment":format!("provider-{i}")}),
                })
                .collect(),
        });
        let routes = (0..4096)
            .map(|i| ModelRef::new("test", format!("model-{i}")).unwrap())
            .collect();
        ui.pending = Some(Box::pin(async {
            Ok(Update::Open(Command::Models, routes))
        }));
        ui.next().await;
        let scene = ui.scene().unwrap();
        assert!(matches!(&ui.stage, Stage::Menu(items) if items.len() == 4161));
        assert!(
            matches!(&scene.surface, rsi_terminal_ui::scene::Surface::Application(app) if app.items.len() <= 256)
        );
        Scene::decode(&scene.encode().unwrap()).unwrap();
    }
    #[tokio::test]
    async fn maximum_length_routes_and_provider_actions_remain_visible_within_scene_budget() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        let id = |prefix: char, n| format!("{}{n:04}", prefix.to_string().repeat(251));
        ui.view.providers = Some(rsi_configuration_api::ProvidersSnapshot {
            desired_revision: "0".into(),
            applied_revision: "0".into(),
            applying: false,
            diagnostic: None,
            deployments: (0..64)
                .map(|i| ManagedProvider {
                    provider: ProviderKind::Deepseek,
                    config: json!({"deployment":id('p', i)}),
                })
                .collect(),
        });
        let routes = (0..4096)
            .map(|i| ModelRef::new(id('d', i / 256), id('m', i)).unwrap())
            .collect();
        ui.pending = Some(Box::pin(async {
            Ok(Update::Open(Command::Models, routes))
        }));
        ui.next().await;
        for selected in [0, 2048, 4095, 4160] {
            ui.selected = selected;
            let scene = Scene::decode(&ui.scene().unwrap().encode().unwrap())
                .expect("valid catalog must fit the scene budget");
            let rsi_terminal_ui::scene::Surface::Application(scene) = scene.surface else {
                panic!("setup scene")
            };
            let Stage::Menu(items) = &ui.stage else {
                panic!("setup menu")
            };
            assert_eq!(items.len(), 4161);
            assert_eq!(scene.items[scene.selected], items[selected].0);
            let (_, view) = ui.scene().unwrap().render(42, 12).unwrap();
            let (row, index) = view.choices[0];
            let expected = scene.items[index].clone();
            ui.mouse(
                termina::event::MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 2,
                    row,
                    modifiers: Modifiers::NONE,
                },
                &view,
            );
            let Stage::Menu(items) = &ui.stage else {
                panic!("setup menu")
            };
            assert_eq!(
                items[ui.indices()[ui.selected]].0,
                expected,
                "the renderer index is relative to the captured 256-item slice"
            );
        }
    }
    #[test]
    fn secret_entry_reserves_its_bound_before_storing_any_characters() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        for _ in 0..2 {
            ui.secret_insert("a").unwrap();
            assert!(ui.secret.capacity() >= 64 * 1024);
            let allocation = ui.secret.as_ptr();
            ui.secret_insert(&"x".repeat(64 * 1024 - 1)).unwrap();
            assert_eq!(allocation, ui.secret.as_ptr());
            assert!(ui.secret_insert("overflow").is_err());
            let moved = SecretValue::new(std::mem::take(&mut *ui.secret)).unwrap();
            drop(moved);
        }
    }
    #[derive(Debug)]
    struct EmptyCatalog;
    #[async_trait::async_trait]
    impl rsi_ai_protocol::LanguageModels for EmptyCatalog {
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
            _: Option<&ModelRef>,
            _: usize,
        ) -> std::result::Result<rsi_ai_protocol::LanguageModelPage, rsi_ai_protocol::ModelsError>
        {
            Ok(rsi_ai_protocol::LanguageModelPage {
                models: vec![],
                has_more: false,
            })
        }
    }
    #[test]
    fn application_commands_are_exact_and_never_accept_a_key_argument() {
        assert!(matches!(
            command(" /login openai "),
            Some(Command::Login(Some(ProviderKind::Openai)))
        ));
        assert!(matches!(
            command("/login openai secret"),
            Some(Command::Invalid)
        ));
        assert!(matches!(command("/model"), Some(Command::Models)));
        assert!(matches!(command(" /effort "), Some(Command::Effort)));
        assert!(matches!(command("/effort high"), Some(Command::Invalid)));
        assert!(command("/efforts").is_none());
        for text in ["/models", "/model-extra", "/compact", "ordinary message"] {
            assert!(command(text).is_none());
        }
    }
    #[tokio::test]
    async fn failed_write_requires_refresh_but_failed_discovery_allows_manual_entry() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.active = true;
        ui.mutation = true;
        ui.definition = Some(ManagedProvider {
            provider: ProviderKind::Deepseek,
            config: json!({"deployment":"stale"}),
        });
        ui.back.push(ui.screen());
        ui.pending = Some(Box::pin(async { Err("Outcome unknown".into()) }));
        ui.next().await;
        assert!(
            matches!(&ui.stage, Stage::Menu(items) if matches!(items.as_slice(), [(_, Choice::Refresh)]))
        );
        assert!(ui.back.is_empty());
        assert!(ui.definition.is_none());
        assert!(ui.pending.is_none());
        assert!(ui.chosen.is_none());
        ui.definition = Some(ManagedProvider {
            provider: ProviderKind::Deepseek,
            config: json!({}),
        });
        ui.pending = Some(Box::pin(async { Err("HTTP 401".into()) }));
        ui.next().await;
        assert!(
            matches!(&ui.stage, Stage::Menu(items) if items.iter().any(|(_, choice)| matches!(choice, Choice::Manual)))
        );
        ui.definition = None;
        ui.pending = Some(Box::pin(async { Err("Catalog unavailable".into()) }));
        ui.next().await;
        assert!(
            matches!(&ui.stage, Stage::Menu(items) if matches!(items.as_slice(), [(_, Choice::Refresh)]))
        );
    }
    #[test]
    fn secret_entry_never_enters_scene_editor_or_clipboard_shortcuts() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.active = true;
        ui.stage = Stage::Secret;
        ui.paste("private-secret-marker".into());
        assert!(ui.editor.text().is_empty());
        let bytes = ui.scene().unwrap().encode().unwrap();
        assert!(
            !String::from_utf8(bytes)
                .unwrap()
                .contains("private-secret-marker")
        );
        ui.secret.zeroize();
        assert!(ui.secret.is_empty());
        assert!(ui.secret_insert("key\nmalformed").is_err());
    }
    #[test]
    fn nonempty_secret_scenes_do_not_disclose_key_length() {
        let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
        ui.stage = Stage::Secret;
        ui.paste("a".into());
        let one = ui.scene().unwrap().encode().unwrap();
        ui.key(KeyCode::Char('界').into());
        ui.paste("a-much-longer-placeholder-key".into());
        assert_eq!(one, ui.scene().unwrap().encode().unwrap());
        assert!(ui.editor.text().is_empty());
        ui.pending = Some(Box::pin(std::future::pending()));
        ui.paste("ignored-secret".into());
        assert!(ui.status.contains("Paste ignored"));
        assert!(!ui.secret.contains("ignored-secret"));
    }
    #[tokio::test]
    async fn control_s_accepts_either_terminal_case_encoding() {
        for ch in ['s', 'S'] {
            let mut ui = Ui::new(None, Arc::new(EmptyCatalog));
            ui.attached = true;
            ui.stage = Stage::Menu(vec![(
                "route".into(),
                Choice::Route(ModelRef::new("test", "model").unwrap()),
            )]);
            let mut key: KeyEvent = KeyCode::Char(ch).into();
            key.modifiers = Modifiers::CONTROL;
            ui.key(key);
            assert!(ui.save_default);
            assert_eq!(ui.status, "Loading model capabilities…");
            ui.next().await;
            assert!(
                ui.status.starts_with(
                    "invalid model catalog request: test catalog has no model profile"
                )
            );
            assert!(ui.chosen.is_none());
        }
    }
}
