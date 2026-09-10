use crate::{
    compatibility, error,
    exchange::{self, Exchange},
    unsupported,
};
use rsi_ai_protocol::{
    AiError, DispatchStatus, ErrorKind, ErrorPhase, ImageEvent, ImageRequest, LanguageEvent,
    LanguageModelLimits, LanguageModelProfiles, LanguageProfile, LanguageRequest, MediaDescriptor,
    portable::{
        self, ControlRequest, ControlResponse, Description, ImageHeader, ImageInput, Kind,
        LanguageInput,
    },
};
use rsi_ai_provider::{
    AbortSignal, AdapterFuture, ImageAdapter, ImageAdapterStream, LanguageAdapter,
    LanguageAdapterStream, PrepareContext, Prepared,
};
use rsi_meta::Capability;
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) struct Adapter {
    capability: Capability,
    pub(crate) description: Arc<Description>,
    models: Arc<LanguageModelProfiles>,
}
impl Adapter {
    pub(crate) async fn load(capability: Capability) -> Result<Self, AiError> {
        let ControlResponse::Description { description } =
            exchange::metadata(&capability, &ControlRequest::Describe {}).await?
        else {
            return Err(invalid_prepare());
        };
        description.validate().map_err(|_| invalid_prepare())?;
        let mut models = LanguageModelProfiles::default();
        for model in &description.language {
            let p = &model.profile;
            let limits = LanguageModelLimits::new(
                p.context_window_tokens(),
                p.default_output_reserve_tokens(),
                p.max_output_reserve_tokens(),
            )
            .map_err(|_| invalid_prepare())?;
            models
                .insert(&model.model, limits)
                .map_err(|_| invalid_prepare())?;
        }
        Ok(Self {
            capability,
            description: Arc::from(description),
            models: Arc::new(models),
        })
    }
}
impl LanguageAdapter for Adapter {
    fn models(&self) -> &LanguageModelProfiles {
        &self.models
    }
    fn describe(&self, model: &str) -> Result<LanguageProfile, AiError> {
        self.description
            .language
            .iter()
            .find(|m| m.model == model)
            .map(|m| m.profile.clone())
            .ok_or_else(unsupported)
    }
    fn validate_request(&self, model: &str, request: &LanguageRequest) -> Result<(), AiError> {
        compatibility::language(
            self.description
                .language
                .iter()
                .find(|m| m.model == model)
                .ok_or_else(unsupported)?,
            request,
        )
    }
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: LanguageRequest,
    ) -> AdapterFuture<Result<Prepared<LanguageAdapterStream>, AiError>> {
        let validation = LanguageAdapter::validate_request(self, &model, &request);
        let capability = self.capability.clone();
        Box::pin(async move {
            validation?;
            let allowed = compatibility::language_media(&request);
            let input = Box::new(LanguageInput {
                model,
                request,
                snapshot: context.snapshot().clone(),
            });
            let prepare = ControlRequest::PrepareLanguage { input };
            let state = prepare_state(&capability, &prepare, &context).await?;
            let ControlRequest::PrepareLanguage { input } = prepare else {
                unreachable!()
            };
            Ok(Prepared::new(context.snapshot().clone(), move |abort| {
                Box::pin(async move {
                    let request = ControlRequest::StartLanguage { input, state };
                    let exchange = start(&capability, &request, abort).await?;
                    Ok(language_stream(exchange, context, allowed))
                })
            }))
        })
    }
}
impl ImageAdapter for Adapter {
    fn validate_request(&self, model: &str, request: &ImageRequest) -> Result<(), AiError> {
        compatibility::image(
            self.description
                .image
                .iter()
                .find(|m| m.model == model)
                .ok_or_else(unsupported)?,
            request,
        )
    }
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: ImageRequest,
    ) -> AdapterFuture<Result<Prepared<ImageAdapterStream>, AiError>> {
        let validation = ImageAdapter::validate_request(self, &model, &request);
        let capability = self.capability.clone();
        Box::pin(async move {
            validation?;
            let mut allowed = request.inputs().to_vec();
            if let Some(mask) = request.mask() {
                allowed.push(mask.clone());
            }
            let input = Box::new(ImageInput {
                model,
                request,
                snapshot: context.snapshot().clone(),
            });
            let prepare = ControlRequest::PrepareImage { input };
            let state = prepare_state(&capability, &prepare, &context).await?;
            let ControlRequest::PrepareImage { input } = prepare else {
                unreachable!()
            };
            Ok(Prepared::new(context.snapshot().clone(), move |abort| {
                Box::pin(async move {
                    let request = ControlRequest::StartImage { input, state };
                    let exchange = start(&capability, &request, abort).await?;
                    Ok(image_stream(exchange, context, allowed))
                })
            }))
        })
    }
}
fn invalid_prepare() -> AiError {
    error(
        ErrorKind::Protocol,
        ErrorPhase::Prepare,
        DispatchStatus::NotDispatched,
    )
}
async fn prepare_state(
    capability: &Capability,
    request: &ControlRequest,
    context: &PrepareContext,
) -> Result<Value, AiError> {
    match exchange::metadata(capability, request).await? {
        ControlResponse::Prepared { snapshot, state }
            if snapshot.as_ref() == context.snapshot() =>
        {
            portable::validate_prepared_state(&state).map_err(|_| invalid_prepare())?;
            Ok(state)
        }
        ControlResponse::Failed { kind, .. } => Err(error(
            kind,
            ErrorPhase::Prepare,
            DispatchStatus::NotDispatched,
        )),
        _ => Err(invalid_prepare()),
    }
}
async fn start(
    capability: &Capability,
    request: &ControlRequest,
    abort: AbortSignal,
) -> Result<Exchange, AiError> {
    let mut exchange = Exchange::open(capability, abort, false)?;
    if let Err(error) = exchange.send_request(request, true).await {
        exchange.cancel_and_drain().await;
        return Err(error);
    }
    Ok(exchange)
}

fn language_stream(
    mut exchange: Exchange,
    context: PrepareContext,
    allowed: Vec<MediaDescriptor>,
) -> LanguageAdapterStream {
    Box::pin(async_stream::stream! {
        let mut events = 0;
        loop {
            let result = next_language(&mut exchange, &context, &allowed, events).await;
            match result {
                Ok((event, terminal)) => {
                    events += 1;
                    yield Ok(event);
                    if terminal { break; }
                }
                Err(error) => { exchange.cancel_and_drain().await; yield Err(error); break; }
            }
        }
    })
}
async fn next_control(
    exchange: &mut Exchange,
    context: &PrepareContext,
    allowed: &[MediaDescriptor],
    seen_output: bool,
) -> Result<ControlResponse, AiError> {
    loop {
        let response = exchange.control().await?;
        if exchange.dependency(&response, context, allowed).await? {
            continue;
        }
        if let ControlResponse::Failed { kind, dispatch } = response {
            exchange.terminal().await?;
            return Err(error(
                kind,
                ErrorPhase::Stream,
                if seen_output {
                    DispatchStatus::Unknown
                } else {
                    dispatch
                },
            ));
        }
        return Ok(response);
    }
}
async fn next_language(
    exchange: &mut Exchange,
    context: &PrepareContext,
    allowed: &[MediaDescriptor],
    events: usize,
) -> Result<(LanguageEvent, bool), AiError> {
    if events >= portable::MAXIMUM_STREAM_EVENTS {
        return Err(exchange.invalid());
    }
    let ControlResponse::Language { event } =
        next_control(exchange, context, allowed, events > 0).await?
    else {
        return Err(exchange.invalid());
    };
    event.validate().map_err(|_| exchange.invalid())?;
    let terminal = matches!(
        *event,
        LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. }
    );
    let event = match *event {
        LanguageEvent::Failed {
            error: failure,
            replay,
        } => LanguageEvent::Failed {
            error: error(
                failure.kind(),
                ErrorPhase::Stream,
                if events > 0 {
                    DispatchStatus::Unknown
                } else {
                    failure.dispatch_status()
                },
            ),
            replay,
        },
        event => event,
    };
    if terminal {
        exchange.terminal().await?;
    }
    Ok((event, terminal))
}
fn image_stream(
    mut exchange: Exchange,
    context: PrepareContext,
    allowed: Vec<MediaDescriptor>,
) -> ImageAdapterStream {
    Box::pin(async_stream::stream! {
        let mut events = 0;
        loop {
            let result = next_image(&mut exchange, &context, &allowed, events).await;
            match result {
                Ok(event) => {
                    events += 1;
                    let terminal = matches!(event, ImageEvent::Finished);
                    yield Ok(event);
                    if terminal { break; }
                }
                Err(error) => { exchange.cancel_and_drain().await; yield Err(error); break; }
            }
        }
    })
}
async fn next_image(
    exchange: &mut Exchange,
    context: &PrepareContext,
    allowed: &[MediaDescriptor],
    events: usize,
) -> Result<ImageEvent, AiError> {
    // The same finite event ceiling bounds both bridge stream envelopes; output
    // byte/count grammar remains the Image assembler's contract.
    if events >= portable::MAXIMUM_STREAM_EVENTS {
        return Err(exchange.invalid());
    }
    let ControlResponse::Image { header } =
        next_control(exchange, context, allowed, events > 0).await?
    else {
        return Err(exchange.invalid());
    };
    Ok(match header {
        ImageHeader::OutputStarted { index, mime_type } => {
            ImageEvent::OutputStarted { index, mime_type }
        }
        ImageHeader::OutputChunk { index, sequence } => {
            let packet = exchange.packet().await?.ok_or_else(|| exchange.invalid())?;
            if packet.kind != Kind::Binary || packet.bytes.is_empty() {
                return Err(exchange.invalid());
            }
            ImageEvent::OutputChunk {
                index,
                sequence,
                bytes: packet.bytes.as_bytes().to_vec(),
            }
        }
        ImageHeader::OutputFinished { index } => ImageEvent::OutputFinished { index },
        ImageHeader::Usage { usage } => ImageEvent::Usage { usage },
        ImageHeader::Finished {} => {
            exchange.terminal().await?;
            ImageEvent::Finished
        }
    })
}
