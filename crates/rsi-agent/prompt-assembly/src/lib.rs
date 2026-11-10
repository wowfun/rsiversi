//! Native named prompt assembly using caller-owned Meta scope effects.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod plugin;
mod render;
pub use plugin::PromptAssemblyFactory;
pub use render::TOOL_ORDER_REST;

use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContributionError, ContributionResult, PreparedPrompt, PromptAssembler, PromptInput,
};
use rsi_agent_session_protocol::{ContributionId, StepInstruction};
use rsi_meta::{LocalContract, RegistrationLease};
use rsi_meta_scope::{
    AnonymousEntries, LayerContext, NamedEntries, ScopeKey, ScopeLayer, ScopeRoot, ScopedLayers,
};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

fn invalid(message: impl Into<String>) -> ContributionError {
    ContributionError::Invalid(message.into())
}

/// A synchronous native provider; only presentation middleware requires awaiting.
pub trait TextProvider: fmt::Debug + Send + Sync + 'static {
    /// Samples the explicit request without holding registry locks.
    ///
    /// # Errors
    /// Returns the provider's failure to resolve this request.
    fn resolve(&self, input: &PromptInput<'_>) -> ContributionResult<Option<String>>;
}

/// Literal text or an ordinary typed provider.
#[derive(Clone, Debug)]
pub enum PromptText {
    /// Static text retained by a plugin registration.
    Literal(Arc<str>),
    /// Request-time text; `None` is an unresolved variable.
    Provider(Arc<dyn TextProvider>),
}
impl PromptText {
    fn resolve(&self, input: &PromptInput<'_>) -> ContributionResult<Option<String>> {
        match self {
            Self::Literal(text) => Ok(Some(text.to_string())),
            Self::Provider(provider) => provider.resolve(input),
        }
    }
}

/// Named text declaration shared by sections and runtime contexts.
#[derive(Clone, Debug)]
pub struct TextDeclaration {
    /// Stable name and durable producer identity.
    pub name: ContributionId,
    /// Finite presentation order.
    pub order: f64,
    /// Literal or request-time text.
    pub text: PromptText,
    /// Resolve strict variable references when true.
    pub interpolate: bool,
}
impl TextDeclaration {
    fn resolve(&self, input: &PromptInput<'_>) -> ContributionResult<PresentedText> {
        Ok(PresentedText {
            name: self.name.to_string(),
            text: self
                .text
                .resolve(input)?
                .ok_or_else(|| invalid(format!("unresolved prompt text {}", self.name)))?,
            interpolate: self.interpolate,
        })
    }
    fn validate(&self) -> ContributionResult<()> {
        if !self.order.is_finite() {
            return Err(invalid("prompt declaration order must be finite"));
        }
        Ok(())
    }
}

/// One system section; complete sections supersede ordinary sections after transforms.
#[derive(Clone, Debug)]
pub struct PromptSection {
    /// Named text and placement.
    pub declaration: TextDeclaration,
    /// Whether this is the complete system prompt.
    pub complete: bool,
}

/// One request-time runtime context.
#[derive(Clone, Debug)]
pub struct PromptContext {
    /// Named text and placement.
    pub declaration: TextDeclaration,
    /// Preserve this operational context under optional-context suppression.
    pub required: bool,
}

/// Resolved but not yet interpolated presentation text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresentedText {
    /// Name retained through presentation transforms.
    pub name: String,
    /// Provider output or transform-authored text.
    pub text: String,
    /// Whether strict interpolation applies.
    pub interpolate: bool,
}

/// Mutable presentation without schemas or execution authority.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PromptPresentation {
    /// Ordered system sections.
    pub sections: Vec<PresentedText>,
    /// Ordered runtime contexts.
    pub contexts: Vec<PresentedText>,
    /// Accepted values; refresh does not sample them again.
    pub variables: BTreeMap<String, Option<String>>,
}

/// Accepted presentation and separate admitted Tool order.
#[derive(Debug)]
pub struct PromptAssembly {
    /// Awaited presentation result, with complete-section enforcement.
    pub presentation: PromptPresentation,
    /// Admitted Tool names only.
    pub tool_order: Vec<String>,
}

/// Agent-owned asynchronous around-presentation contract.
///
/// The original complete section replaces all section edits after the chain.
/// At request admission, registered contexts are refreshed after transforms,
/// replacing their text edits and restoring missing required contexts. Text of
/// transform-only contexts survives unless optional context is suppressed.
#[async_trait]
pub trait PresentationTransform: fmt::Debug + Send + Sync + 'static {
    /// Mutates presentation and optionally consumes the rest of the chain once.
    async fn transform(
        &self,
        presentation: PromptPresentation,
        input: &PromptInput<'_>,
        next: PromptContinuation<'_>,
    ) -> ContributionResult<PromptPresentation>;
}

/// Consuming continuation over an owned request snapshot of actual transforms.
#[derive(Debug)]
pub struct PromptContinuation<'a> {
    remaining: &'a [Arc<dyn PresentationTransform>],
    cancellation: &'a CancellationToken,
}
impl PromptContinuation<'_> {
    /// Caller cancellation for this explicit request only.
    pub fn cancellation(&self) -> &CancellationToken {
        self.cancellation
    }

    /// Consumes the continuation; rechecks cancellation after awaited wrappers.
    ///
    /// # Errors
    /// Returns cancellation or a downstream transform failure.
    pub async fn run(
        self,
        presentation: PromptPresentation,
        input: &PromptInput<'_>,
    ) -> ContributionResult<PromptPresentation> {
        if self.cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        match self.remaining.split_first() {
            Some((callback, remaining)) => {
                callback
                    .transform(
                        presentation,
                        input,
                        Self {
                            remaining,
                            cancellation: self.cancellation,
                        },
                    )
                    .await
            }
            None => Ok(presentation),
        }
    }
}

#[derive(Debug, Default)]
struct Layer {
    sections: NamedEntries<PromptSection>,
    contexts: NamedEntries<PromptContext>,
    variables: NamedEntries<PromptText>,
    suppressors: AnonymousEntries<()>,
    transforms: AnonymousEntries<(u64, Arc<dyn PresentationTransform>)>,
}
impl ScopeLayer for Layer {
    fn is_empty(&self) -> bool {
        self.sections.is_empty()
            && self.contexts.is_empty()
            && self.variables.is_empty()
            && self.suppressors.is_empty()
            && self.transforms.is_empty()
    }
}

/// Registry whose registrations are owned by each caller's explicit Context.
#[derive(Debug)]
pub struct PromptRegistry {
    layers: ScopedLayers<Layer>,
    tool_order: Option<Vec<String>>,
    next_transform: AtomicU64,
}

/// Native authoring service in the selected Agent generation.
#[derive(Debug)]
pub struct PromptRegistryContract;
impl LocalContract for PromptRegistryContract {
    const KEY: &'static str = "rsi.agent.prompt-registry";
    type Service = PromptRegistry;
}

impl PromptRegistry {
    /// Uses the existing composition root and validates configuration once.
    ///
    /// # Errors
    /// Rejects invalid or duplicate configured Tool-order names.
    pub fn new(
        root: ScopeRoot,
        maximum_scoped_layers: usize,
        tool_order: Option<Vec<String>>,
    ) -> ContributionResult<Self> {
        render::validate_order(tool_order.as_deref())?;
        Ok(Self {
            layers: ScopedLayers::new(
                root,
                maximum_scoped_layers,
                |_| Layer::default(),
                || async { Ok(()) },
            ),
            tool_order,
            next_transform: AtomicU64::new(0),
        })
    }

    /// Registers one exact named section and its caller-owned removal effect.
    ///
    /// # Errors
    /// Rejects invalid declarations, duplicate names in this layer, or unavailable owner scopes.
    pub async fn section<C: LayerContext + ?Sized>(
        &self,
        owner: &C,
        section: PromptSection,
    ) -> ContributionResult<RegistrationLease> {
        section.declaration.validate()?;
        self.layers
            .register(owner, "prompt section", |layer| {
                layer
                    .sections
                    .insert(section.declaration.name.to_string(), section)
                    .map_err(|section| {
                        format!("duplicate prompt section {}", section.declaration.name)
                    })
            })
            .await
            .map_err(|error| invalid(error.to_string()))
    }

    /// Registers one exact named runtime context.
    ///
    /// # Errors
    /// Rejects invalid declarations, duplicate names in this layer, or unavailable owner scopes.
    pub async fn context<C: LayerContext + ?Sized>(
        &self,
        owner: &C,
        context: PromptContext,
    ) -> ContributionResult<RegistrationLease> {
        context.declaration.validate()?;
        self.layers
            .register(owner, "prompt runtime context", |layer| {
                layer
                    .contexts
                    .insert(context.declaration.name.to_string(), context)
                    .map_err(|context| {
                        format!("duplicate prompt context {}", context.declaration.name)
                    })
            })
            .await
            .map_err(|error| invalid(error.to_string()))
    }

    /// Registers a variable; unresolved values fail only when referenced.
    ///
    /// # Errors
    /// Rejects invalid or duplicate names and unavailable owner scopes.
    pub async fn variable<C: LayerContext + ?Sized>(
        &self,
        owner: &C,
        name: String,
        provider: PromptText,
    ) -> ContributionResult<RegistrationLease> {
        if !render::variable_name(&name) {
            return Err(invalid("invalid prompt variable name"));
        }
        self.layers
            .register(owner, "prompt variable", |layer| {
                layer
                    .variables
                    .insert(name, provider)
                    .map_err(|_| "duplicate prompt variable")
            })
            .await
            .map_err(|error| invalid(error.to_string()))
    }

    /// Suppresses optional contexts without changing the services enforcing them.
    ///
    /// # Errors
    /// Fails when the caller's scope cannot accept a registration.
    pub async fn suppress_runtime_context<C: LayerContext + ?Sized>(
        &self,
        owner: &C,
    ) -> ContributionResult<RegistrationLease> {
        self.layers
            .register(owner, "suppress optional prompt context", |layer| {
                Ok::<_, String>(layer.suppressors.append(()))
            })
            .await
            .map_err(|error| invalid(error.to_string()))
    }

    /// Registers an awaited presentation transform at the caller's scope.
    ///
    /// # Errors
    /// Fails when the caller's scope cannot accept a registration.
    pub async fn transform<C: LayerContext + ?Sized>(
        &self,
        owner: &C,
        transform: Arc<dyn PresentationTransform>,
    ) -> ContributionResult<RegistrationLease> {
        self.layers
            .register(owner, "prompt presentation transform", |layer| {
                Ok::<_, String>(layer.transforms.append((
                    self.next_transform.fetch_add(1, Ordering::Relaxed),
                    transform,
                )))
            })
            .await
            .map_err(|error| invalid(error.to_string()))
    }

    fn layers(&self, scope: Option<&ScopeKey>) -> ContributionResult<Vec<Arc<Layer>>> {
        let mut layers = vec![self.layers.global()];
        if let Some(scope) = scope {
            layers.extend(
                self.layers
                    .chain_layers(scope)
                    .map_err(|error| invalid(error.to_string()))?,
            );
        }
        Ok(layers)
    }

    fn contexts(&self, scope: Option<&ScopeKey>) -> ContributionResult<Vec<PromptContext>> {
        Ok(self
            .layers
            .effective_named(scope, |layer| &layer.contexts)
            .map_err(|error| invalid(error.to_string()))?
            .into_iter()
            .map(|(_, value)| value)
            .collect())
    }

    /// Assembles providers once, then awaits presentation and enforces complete sections.
    ///
    /// # Errors
    /// Returns scope, provider, transform, ordering, complete-section, or cancellation failures.
    pub async fn presentation(
        &self,
        input: &PromptInput<'_>,
        cancellation: &CancellationToken,
    ) -> ContributionResult<PromptAssembly> {
        self.prepare_presentation(input, cancellation)
            .await
            .map(|(assembly, _)| assembly)
    }

    async fn prepare_presentation(
        &self,
        input: &PromptInput<'_>,
        cancellation: &CancellationToken,
    ) -> ContributionResult<(PromptAssembly, bool)> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let layers = self.layers(input.scope)?;
        let suppressed = layers.iter().any(|layer| !layer.suppressors.is_empty());
        let mut presentation = PromptPresentation::default();
        for layer in &layers {
            for (name, provider) in layer.variables.snapshot() {
                presentation
                    .variables
                    .insert(name, provider.resolve(input)?);
            }
        }
        let mut sections = defaults(input);
        for (name, section) in self
            .layers
            .effective_named(input.scope, |layer| &layer.sections)
            .map_err(|error| invalid(error.to_string()))?
        {
            if let Some(index) = sections
                .iter()
                .position(|previous| previous.declaration.name.as_str() == name)
            {
                sections[index] = section;
            } else {
                sections.push(section);
            }
        }
        sections.sort_by(|a, b| {
            a.declaration
                .order
                .total_cmp(&b.declaration.order)
                .then_with(|| {
                    render::names(a.declaration.name.as_str(), b.declaration.name.as_str())
                })
        });
        if sections.iter().filter(|section| section.complete).count() > 1 {
            return Err(invalid("multiple complete prompt sections"));
        }
        let mut complete = None;
        for section in sections {
            let resolved = section.declaration.resolve(input)?;
            if section.complete {
                complete = Some(resolved.clone());
            }
            presentation.sections.push(resolved);
        }
        let mut contexts = self.contexts(input.scope)?;
        contexts.sort_by(|a, b| a.declaration.order.total_cmp(&b.declaration.order));
        for context in &contexts {
            if !suppressed || context.required {
                presentation
                    .contexts
                    .push(context.declaration.resolve(input)?);
            }
        }
        let tool_order = render::ordered_tools(
            input.tools.iter().cloned(),
            input.known_tools,
            self.tool_order.as_deref(),
        )?;
        let mut registered: Vec<_> = layers
            .iter()
            .flat_map(|layer| layer.transforms.snapshot())
            .collect();
        registered.sort_by_key(|(order, _)| *order);
        let transforms: Vec<_> = registered
            .into_iter()
            .map(|(_, callback)| callback)
            .collect();
        let mut presentation = PromptContinuation {
            remaining: &transforms,
            cancellation,
        }
        .run(presentation, input)
        .await?;
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        if let Some(complete) = complete {
            presentation.sections = vec![complete];
        }
        if suppressed {
            presentation.contexts.retain(|text| {
                contexts.iter().any(|context| {
                    context.required && context.declaration.name.as_str() == text.name
                })
            });
        }
        Ok((
            PromptAssembly {
                presentation,
                tool_order,
            },
            !transforms.is_empty(),
        ))
    }

    /// Refreshes runtime context without rerunning accepted presentation or other providers.
    ///
    /// # Errors
    /// Returns scope or runtime-context provider failures.
    pub fn refresh_context(
        &self,
        assembly: &mut PromptAssembly,
        input: &PromptInput<'_>,
    ) -> ContributionResult<()> {
        let providers = self.contexts(input.scope)?;
        let suppressed = self
            .layers(input.scope)?
            .iter()
            .any(|layer| !layer.suppressors.is_empty());
        let mut contexts = Vec::new();
        for accepted in std::mem::take(&mut assembly.presentation.contexts) {
            let provider = providers
                .iter()
                .find(|provider| provider.declaration.name.as_str() == accepted.name);
            if suppressed && !provider.is_some_and(|provider| provider.required) {
                continue;
            }
            contexts.push(match provider {
                Some(provider) => provider.declaration.resolve(input)?,
                None => accepted,
            });
        }
        for provider in providers.iter().filter(|provider| provider.required) {
            if contexts
                .iter()
                .any(|context| context.name == provider.declaration.name.as_str())
            {
                continue;
            }
            let index = contexts
                .iter()
                .position(|context| {
                    providers.iter().any(|next| {
                        next.declaration.name.as_str() == context.name
                            && next.declaration.order > provider.declaration.order
                    })
                })
                .unwrap_or(contexts.len());
            contexts.insert(index, provider.declaration.resolve(input)?);
        }
        assembly.presentation.contexts = contexts;
        Ok(())
    }
}

#[allow(clippy::cast_precision_loss)] // The admitted contribution count is far below f64's exact integer range.
fn defaults(input: &PromptInput<'_>) -> Vec<PromptSection> {
    let mut sections = Vec::with_capacity(input.instructions.len() + 2);
    let mut add = |name: ContributionId, order, text: &str| {
        sections.push(PromptSection {
            declaration: TextDeclaration {
                name,
                order,
                text: PromptText::Literal(text.into()),
                interpolate: false,
            },
            complete: false,
        });
    };
    add(
        ContributionId::new("rsi.header").expect("static name"),
        -2000.0,
        input.header.settings().system_prompt(),
    );
    if let Some(persona) = input
        .header
        .delegation_policy()
        .and_then(|policy| policy.persona())
    {
        add(
            ContributionId::new("rsi.delegation").expect("static name"),
            -1000.0,
            persona,
        );
    }
    for (index, instruction) in input.instructions.iter().enumerate() {
        add(
            instruction.producer.clone(),
            index as f64,
            &instruction.text,
        );
    }
    sections
}

/// Renders accepted presentation once; temporary variables never enter durable state.
///
/// # Errors
/// Rejects unresolved interpolation and invalid durable producer identities or text.
pub fn render(assembly: PromptAssembly) -> ContributionResult<PreparedPrompt> {
    let mut sections = Vec::new();
    for mut section in assembly.presentation.sections {
        let text = render::interpolate(&mut section, &assembly.presentation.variables)?;
        if text.is_empty() {
            continue;
        }
        sections.push(StepInstruction {
            producer: ContributionId::new(section.name)
                .map_err(|error| invalid(error.to_string()))?,
            text,
        });
    }
    let mut rendered_contexts = Vec::new();
    for mut context in assembly.presentation.contexts {
        let text = render::interpolate(&mut context, &assembly.presentation.variables)?;
        if !text.is_empty() {
            rendered_contexts.push(text);
        }
    }
    let runtime_context = if rendered_contexts.is_empty() {
        String::new()
    } else {
        format!(
            "Current runtime context. This snapshot supersedes earlier runtime-context snapshots.\n\n{}",
            rendered_contexts.join("\n\n")
        )
    };
    Ok(PreparedPrompt {
        sections,
        runtime_context,
        tool_order: assembly.tool_order,
    })
}

#[async_trait]
impl PromptAssembler for PromptRegistry {
    async fn assemble(
        &self,
        input: PromptInput<'_>,
        cancellation: CancellationToken,
    ) -> ContributionResult<PreparedPrompt> {
        let (mut assembly, transformed) = self.prepare_presentation(&input, &cancellation).await?;
        if transformed {
            self.refresh_context(&mut assembly, &input)?;
        }
        render(assembly)
    }
}
