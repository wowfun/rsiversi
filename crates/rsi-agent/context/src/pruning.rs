//! Pure, coordinate-preserving views of original interaction units.

use crate::{ContextError, Result, encoded_message_bytes};
use rsi_agent_session_protocol::TurnId;
use rsi_ai_protocol::{Message, MessageContent, MessageRole};
use std::{borrow::Cow, collections::BTreeMap};
pub(crate) type ProjectedTurns<'a> = BTreeMap<TurnId, Cow<'a, [Message]>>;

const THRESHOLD: usize = 8192;
const HEAD: usize = 4096;
const TAIL: usize = 1024;
const MARKER: &str = "\n\n[... tool result middle pruned ...]\n\n";

pub(crate) struct InteractionUnit {
    pub turn: TurnId,
    pub first: usize,
    pub count: usize,
    pub bytes: usize,
    pub complete: bool,
}

struct UnitShape {
    first: usize,
    count: usize,
    complete: bool,
}

pub(crate) fn units(
    turn: &TurnId,
    messages: &[Message],
    allow_incomplete: bool,
) -> Result<Vec<InteractionUnit>> {
    shapes(messages, allow_incomplete)?
        .into_iter()
        .map(|shape| {
            Ok(InteractionUnit {
                turn: turn.clone(),
                first: shape.first,
                count: shape.count,
                complete: shape.complete,
                bytes: messages[shape.first..shape.first + shape.count]
                    .iter()
                    .try_fold(0usize, |sum, message| {
                        sum.checked_add(encoded_message_bytes(message)?)
                            .ok_or(ContextError::TooLarge)
                    })?,
            })
        })
        .collect()
}

/// Missing results remain evidence; an orphan or reordered result never does.
fn shapes(messages: &[Message], allow_incomplete: bool) -> Result<Vec<UnitShape>> {
    let invalid = |message: &str| ContextError::Invalid(message.into());
    let mut result = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        let first = index;
        let message = &messages[index];
        if message.role() == MessageRole::Tool {
            return Err(invalid("orphan Tool result in compaction input"));
        }
        index += 1;
        let mut complete = true;
        for call in message
            .content()
            .iter()
            .filter_map(|content| match content {
                MessageContent::ToolCall(call) => Some(call.id.as_str()),
                _ => None,
            })
        {
            if messages.get(index).is_some_and(|message| message.content().iter().any(|content| matches!(content, MessageContent::ToolResult { call_id, .. } if call_id == call))) {
                index += 1;
            } else if allow_incomplete {
                complete = false;
            } else {
                return Err(invalid("unfinished or misordered live Tool batch in compaction input"));
            }
        }
        result.push(UnitShape {
            first,
            count: index - first,
            complete,
        });
    }
    Ok(result)
}

pub(crate) fn project<'a>(
    turns: impl IntoIterator<Item = (&'a TurnId, &'a [Message])>,
) -> Result<ProjectedTurns<'a>> {
    let mut all_units = Vec::new();
    let mut projected = BTreeMap::new();
    for (id, messages) in turns {
        all_units.extend(shapes(messages, true)?.into_iter().map(|shape| (id, shape)));
        projected.insert(id.clone(), Cow::Borrowed(messages));
    }
    let latest = all_units.iter().rposition(|(_, unit)| unit.complete);
    for (index, (turn, unit)) in all_units.iter().enumerate() {
        if !unit.complete || Some(index) == latest {
            continue;
        }
        let messages = projected.get_mut(turn).expect("captured Turn");
        for index in unit.first..unit.first + unit.count {
            if let Some(pruned) = prune_message(&messages[index])? {
                messages.to_mut()[index] = pruned;
            }
        }
    }
    Ok(projected)
}

fn prune_message(message: &Message) -> Result<Option<Message>> {
    let [
        MessageContent::ToolResult {
            call_id,
            content,
            is_error,
        },
    ] = message.content()
    else {
        return Ok(None);
    };
    let lengths: Vec<_> = content
        .iter()
        .map(|block| match block {
            MessageContent::Text { text } => text.chars().count(),
            _ => 0,
        })
        .collect();
    let length: usize = lengths.iter().sum();
    if length <= THRESHOLD {
        return Ok(None);
    }
    let tail_start = length - TAIL;
    let mut position = 0;
    let mut marked = false;
    let mut blocks = Vec::new();
    for (block, length) in content.iter().zip(lengths) {
        let MessageContent::Text { text } = block else {
            blocks.push(block.clone());
            continue;
        };
        let head = HEAD.saturating_sub(position).min(length);
        let tail = (position + length).saturating_sub(tail_start).min(length);
        let head_end = text
            .char_indices()
            .nth(head)
            .map_or(text.len(), |(at, _)| at);
        let tail_begin = text
            .char_indices()
            .rev()
            .nth(tail.saturating_sub(1))
            .filter(|_| tail > 0)
            .map_or(text.len(), |(at, _)| at);
        let mut retained = String::from(&text[..head_end]);
        if head + tail < length && !marked {
            retained.push_str(MARKER);
            marked = true;
        }
        retained.push_str(&text[tail_begin..]);
        position += length;
        if !retained.is_empty() {
            blocks.push(MessageContent::Text { text: retained });
        }
    }
    Message::tool_result(call_id, blocks, *is_error)
        .map(Some)
        .map_err(|error| ContextError::Invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_ai_protocol::{ToolCall, ToolCallKind};

    fn batch(text: String) -> Vec<Message> {
        vec![
            Message::assistant(vec![MessageContent::ToolCall(ToolCall {
                id: "call".into(),
                name: "read".into(),
                arguments: "{}".into(),
                kind: ToolCallKind::Function,
            })])
            .unwrap(),
            Message::tool_result("call", vec![MessageContent::Text { text }], true).unwrap(),
        ]
    }

    #[test]
    fn unchanged_projection_borrows_messages_and_only_pruned_turns_are_owned() {
        let id = TurnId::new("first").unwrap();
        let tail = TurnId::new("last").unwrap();
        let plain = vec![Message::user_text("plain").unwrap()];
        let long = batch("x".repeat(THRESHOLD + 1));
        let unchanged = project([(&id, plain.as_slice()), (&tail, long.as_slice())]).unwrap();
        assert!(
            unchanged
                .values()
                .all(|messages| matches!(messages, Cow::Borrowed(_)))
        );
        let changed = project([(&id, long.as_slice()), (&tail, plain.as_slice())]).unwrap();
        assert!(matches!(changed[&id], Cow::Owned(_)));
        assert!(matches!(changed[&tail], Cow::Borrowed(_)));
    }

    #[test]
    fn latest_unit_moves_without_destroying_original_unicode_or_json_fallback() {
        let id = TurnId::new("turn").unwrap();
        let original = batch(format!("{{\"data\":\"{}\"}}", "界🦀".repeat(6000)));
        assert_eq!(
            project([(&id, original.as_slice())]).unwrap()[&id],
            original
        );
        let mut advanced = original.clone();
        advanced.push(Message::user_text("continue").unwrap());
        let view = project([(&id, advanced.as_slice())]).unwrap();
        let [
            MessageContent::ToolResult {
                content,
                call_id,
                is_error,
            },
        ] = view[&id][1].content()
        else {
            panic!()
        };
        let [MessageContent::Text { text }] = &content[..] else {
            panic!()
        };
        assert_eq!(call_id, "call");
        assert!(*is_error);
        assert_eq!(text.chars().count(), HEAD + MARKER.chars().count() + TAIL);
        assert!(text.contains(MARKER));
        assert_eq!(&advanced[..2], &original);
        assert_eq!(project([(&id, advanced.as_slice())]).unwrap(), view);
    }

    #[test]
    fn threshold_is_codepoints_and_multiple_text_blocks_preserve_media_order() {
        use rsi_ai_protocol::{MediaDescriptor, MediaKind};
        let image = MessageContent::Image(
            MediaDescriptor::new(MediaKind::Image, "image/png", 4, "a".repeat(64)).unwrap(),
        );
        let blocks = vec![
            MessageContent::Text {
                text: "界".repeat(4096),
            },
            image.clone(),
            MessageContent::Text {
                text: "🦀".repeat(4096),
            },
        ];
        let exact = Message::tool_result("call", blocks.clone(), false).unwrap();
        assert!(prune_message(&exact).unwrap().is_none());
        let mut blocks = blocks;
        blocks.push(MessageContent::Text { text: "尾".into() });
        let above = Message::tool_result("call", blocks, false).unwrap();
        let pruned = prune_message(&above).unwrap().unwrap();
        let [MessageContent::ToolResult { content, .. }] = pruned.content() else {
            panic!()
        };
        assert_eq!(
            content[0],
            MessageContent::Text {
                text: "界".repeat(HEAD)
            }
        );
        assert_eq!(content[1], image);
        let texts: String = content
            .iter()
            .filter_map(|c| match c {
                MessageContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            format!("{}{}{}尾", "界".repeat(HEAD), MARKER, "🦀".repeat(TAIL - 1))
        );
    }

    #[test]
    fn head_and_tail_match_across_multibyte_block_boundaries() {
        let text = "a界🦀".repeat(4000);
        let characters: Vec<_> = text.chars().collect();
        let expected = format!(
            "{}{}{}",
            characters[..HEAD].iter().collect::<String>(),
            MARKER,
            characters[characters.len() - TAIL..]
                .iter()
                .collect::<String>()
        );
        for split in [
            0,
            1,
            HEAD - 1,
            HEAD,
            HEAD + 1,
            THRESHOLD,
            characters.len() - TAIL,
            characters.len(),
        ] {
            let blocks = [&characters[..split], &characters[split..]]
                .into_iter()
                .filter(|part| !part.is_empty())
                .map(|part| MessageContent::Text {
                    text: part.iter().collect(),
                })
                .collect();
            let message = Message::tool_result("call", blocks, false).unwrap();
            let pruned = prune_message(&message).unwrap().unwrap();
            let [MessageContent::ToolResult { content, .. }] = pruned.content() else {
                panic!()
            };
            let actual: String = content
                .iter()
                .filter_map(|block| match block {
                    MessageContent::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(actual, expected, "split at {split}");
        }
    }

    #[test]
    fn partial_units_stay_intact_and_misordered_results_are_rejected() {
        let id = TurnId::new("turn").unwrap();
        let mut messages = batch("x".repeat(9000));
        let MessageContent::ToolCall(call) = &messages[0].content()[0] else {
            panic!()
        };
        let mut missing = call.clone();
        missing.id = "missing".into();
        messages[0] = Message::assistant(vec![
            MessageContent::ToolCall(call.clone()),
            MessageContent::ToolCall(missing),
        ])
        .unwrap();
        messages.push(Message::user_text("continue").unwrap());
        assert_eq!(
            project([(&id, messages.as_slice())]).unwrap()[&id],
            messages
        );
        assert!(units(&id, &messages, false).is_err());
        messages.push(
            Message::tool_result(
                "call",
                vec![MessageContent::Text {
                    text: "orphan".into(),
                }],
                false,
            )
            .unwrap(),
        );
        assert!(project([(&id, messages.as_slice())]).is_err());
    }
}
