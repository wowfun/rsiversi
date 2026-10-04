//! Target-dependent request projection; canonical history and summary proofs stay rich.

use crate::{ContextError, ContextLimits, Result};
use rsi_ai_protocol::{
    ImageToolResultCapability, LanguageProfile, LanguageRequest, LanguageRequestOptions, Message,
    MessageContent,
};

pub(crate) fn project_tool_images(
    messages: Vec<Message>,
    options: LanguageRequestOptions,
    profile: &LanguageProfile,
    limits: ContextLimits,
) -> Result<LanguageRequest> {
    if matches!(
        profile.image_tool_result(),
        ImageToolResultCapability::Yes(_)
    ) || !messages.iter().any(|message| {
        message.content().iter().any(|block| {
            matches!(block, MessageContent::ToolResult { content, .. }
                    if content.iter().any(|part| matches!(part, MessageContent::Image(_))))
        })
    }) {
        return LanguageRequest::new_with_options(messages, options)
            .map_err(|error| ContextError::Invalid(error.to_string()));
    }
    let messages = messages
        .into_iter()
        .map(|message| {
            let [MessageContent::ToolResult { call_id, content, is_error }] = message.content() else {
                return Ok(message);
            };
            if !content.iter().any(|part| matches!(part, MessageContent::Image(_))) {
                return Ok(message);
            }
            let content = content.iter().map(|part| match part {
                MessageContent::Image(media) => MessageContent::Text {
                    text: format!(
                        "[Tool image retained: {}; {} bytes; sha256={}. The selected model does not declare image Tool results.]",
                        media.mime_type(), media.byte_len(), media.sha256(),
                    ),
                },
                other => other.clone(),
            }).collect();
            Message::tool_result(call_id, content, *is_error)
                .map_err(|error| ContextError::Invalid(error.to_string()))
        })
        .collect::<Result<Vec<_>>>()?;
    let limits = crate::emission_limits(limits, &options)?;
    if messages.len() > limits.max_messages || crate::encoded_bytes(&messages)? > limits.max_bytes {
        return Err(ContextError::TooLarge);
    }
    LanguageRequest::new_with_options(messages, options)
        .map_err(|error| ContextError::Invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_fallback_moves_unaffected_messages_and_preserves_result_semantics() {
        let image = rsi_media_protocol::MediaDescriptor::new(
            rsi_media_protocol::MediaKind::Image,
            "image/png",
            120,
            "a".repeat(64),
        )
        .unwrap()
        .with_image_dimensions(2, 3)
        .unwrap();
        let text = |text: &str| MessageContent::Text { text: text.into() };
        let request = LanguageRequest::new(vec![
            Message::user_text("large history ".repeat(4096)).unwrap(),
            Message::assistant(
                ["image", "text"]
                    .map(|id| {
                        MessageContent::ToolCall(rsi_ai_protocol::ToolCall {
                            id: id.into(),
                            name: "read".into(),
                            arguments: "{}".into(),
                            kind: rsi_ai_protocol::ToolCallKind::Function,
                        })
                    })
                    .to_vec(),
            )
            .unwrap(),
            Message::tool_result(
                "image",
                vec![text("before"), MessageContent::Image(image), text("after")],
                true,
            )
            .unwrap(),
            Message::tool_result("text", vec![text("unchanged")], false).unwrap(),
        ])
        .unwrap();
        let pointers = [0, 1, 3].map(|index| request.messages()[index].content().as_ptr());
        let profile = LanguageProfile::new(
            128_000,
            4096,
            32768,
            rsi_ai_protocol::ToolDialect::Responses,
            true,
            ImageToolResultCapability::No,
            vec![],
        )
        .unwrap();
        let projected = project_tool_images(
            request.into_messages().unwrap(),
            LanguageRequestOptions::default(),
            &profile,
            ContextLimits::default(),
        )
        .unwrap();
        for (index, pointer) in [0, 1, 3].into_iter().zip(pointers) {
            assert_eq!(
                projected.messages()[index].content().as_ptr(),
                pointer,
                "unaffected message {index} must retain its allocation"
            );
        }
        let [
            MessageContent::ToolResult {
                call_id,
                content,
                is_error,
            },
        ] = projected.messages()[2].content()
        else {
            panic!("missing result")
        };
        assert_eq!(call_id, "image");
        assert!(*is_error);
        assert!(matches!(&content[0], MessageContent::Text {text} if text == "before"));
        assert!(
            matches!(&content[1], MessageContent::Text {text} if text.contains("120 bytes") && text.contains(&"a".repeat(64)))
        );
        assert!(matches!(&content[2], MessageContent::Text {text} if text == "after"));
    }
}
