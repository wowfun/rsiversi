use crate::unsupported;
use rsi_ai_protocol::{
    AiError, ImageRequest, ImageToolResultCapability, LanguageRequest, LanguageSettings,
    MediaDescriptor, MessageContent, ResponseFormat, ToolCallKind,
    portable::{ImageFeature, ImageModel, LanguageFeature, LanguageModel},
};

pub(crate) fn language(model: &LanguageModel, request: &LanguageRequest) -> Result<(), AiError> {
    let features = &model.features;
    let profile = &model.profile;
    if (!request.hosted_tools().is_empty() && !features.contains(&LanguageFeature::HostedTools))
        || (!matches!(request.response_format(), ResponseFormat::Text)
            && !features.contains(&LanguageFeature::StructuredOutput))
        || (request.settings() != &LanguageSettings::default()
            && !features.contains(&LanguageFeature::GenerationSettings))
        || request
            .settings()
            .max_output_tokens()
            .is_some_and(|value| value > profile.max_output_reserve_tokens())
        || request
            .tools()
            .iter()
            .any(|tool| tool.freeform().is_some() && !profile.supports_freeform_tools())
        || request.extensions().iter().any(|e| {
            !model.request_extensions.iter().any(|format| {
                format.namespace() == e.namespace() && format.version() == e.version()
            })
        })
    {
        return Err(unsupported());
    }
    for content in request
        .messages()
        .iter()
        .flat_map(rsi_ai_protocol::Message::content)
    {
        validate_content(model, content, false)?;
    }
    Ok(())
}
fn validate_content(
    model: &LanguageModel,
    content: &MessageContent,
    tool_result: bool,
) -> Result<(), AiError> {
    let supported = match content {
        MessageContent::Image(_) if tool_result => matches!(
            model.profile.image_tool_result(),
            ImageToolResultCapability::Yes(_)
        ),
        MessageContent::Image(_) => model.features.contains(&LanguageFeature::InputImages),
        MessageContent::Audio(_) => model.features.contains(&LanguageFeature::InputAudio),
        MessageContent::ToolCall(call) => {
            call.kind != ToolCallKind::Freeform || model.profile.supports_freeform_tools()
        }
        MessageContent::Reasoning {
            evidence: Some(e), ..
        } => model.profile.accepts_extension(e),
        MessageContent::ToolResult { content, .. } => {
            for child in content {
                validate_content(model, child, true)?;
            }
            true
        }
        _ => true,
    };
    if supported {
        Ok(())
    } else {
        Err(unsupported())
    }
}
pub(crate) fn image(model: &ImageModel, request: &ImageRequest) -> Result<(), AiError> {
    if request.count() > model.maximum_count
        || (!request.inputs().is_empty() && !model.features.contains(&ImageFeature::Inputs))
        || (request.mask().is_some() && !model.features.contains(&ImageFeature::Mask))
    {
        Err(unsupported())
    } else {
        Ok(())
    }
}
pub(crate) fn language_media(request: &LanguageRequest) -> Vec<MediaDescriptor> {
    fn collect(content: &MessageContent, result: &mut Vec<MediaDescriptor>) {
        match content {
            MessageContent::Image(descriptor) | MessageContent::Audio(descriptor) => {
                if !result.contains(descriptor) {
                    result.push(descriptor.clone());
                }
            }
            MessageContent::ToolResult { content, .. } => {
                for child in content {
                    collect(child, result);
                }
            }
            _ => {}
        }
    }
    let mut result = Vec::new();
    for content in request
        .messages()
        .iter()
        .flat_map(rsi_ai_protocol::Message::content)
    {
        collect(content, &mut result);
    }
    result
}
