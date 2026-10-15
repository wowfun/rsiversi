//! Bounded source proofs; ordinary model prose never enters this state.

use super::*;
use rsi_agent_session_protocol::ModelSelection;
use rsi_ai_protocol::{ContentDelta, ContentStart, LanguageEvent, ModelRef, ToolCallKind};

const CHUNK_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub(super) struct ToolSource {
    effect_id: EffectId,
    selection: Option<ModelSelection>,
    completed: bool,
    bytes: usize,
    calls: BTreeMap<u32, Arc<Call>>,
}

#[derive(Clone)]
struct Call {
    id: Arc<str>,
    name: Arc<str>,
    kind: ToolCallKind,
    arguments: Arguments,
}

#[derive(Clone)]
enum Arguments {
    Building {
        chunks: Vec<ArgumentChunk>,
        bytes: usize,
    },
    // Invalid model JSON is still valid stream evidence, but cannot authorize a Tool.
    Complete(Option<[u8; 32]>),
    Consumed,
}

// Each snapshot owns a visible prefix length; only bytes beyond it may be appended.
#[derive(Clone)]
struct ArgumentChunk {
    data: Arc<Mutex<Vec<u8>>>,
    len: usize,
}
impl ArgumentChunk {
    fn new() -> Self {
        Self {
            data: Arc::new(Mutex::new(Vec::new())),
            len: 0,
        }
    }
    fn append(&mut self, remaining: &[u8]) -> usize {
        let length = remaining.len().min(CHUNK_BYTES - self.len);
        let mut data = self
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if data.len() == self.len {
            data.extend_from_slice(&remaining[..length]);
        } else {
            // Another speculative branch already appended here. Preserve our prefix.
            let mut branch = data[..self.len].to_vec();
            branch.extend_from_slice(&remaining[..length]);
            drop(data);
            self.data = Arc::new(Mutex::new(branch));
        }
        self.len += length;
        length
    }
}

// This fingerprint is never persisted. Both admission and recovery recompute it
// with the same typed Value hash, which sorts object keys under preserve_order.
fn argument_digest(value: &serde_json::Value) -> [u8; 32] {
    use sha2::Digest;
    use std::hash::{Hash, Hasher};
    struct Fingerprint(sha2::Sha256);
    impl Hasher for Fingerprint {
        fn write(&mut self, bytes: &[u8]) {
            self.0.update(bytes);
        }
        fn finish(&self) -> u64 {
            let digest = self.0.clone().finalize();
            u64::from_le_bytes(digest[..8].try_into().expect("eight digest bytes"))
        }
    }
    let mut state = Fingerprint(sha2::Sha256::new());
    value.hash(&mut state);
    state.0.finalize().into()
}

fn invalid(message: &str) -> TurnError {
    TurnError::Invalid(format!("Tool model origin: {message}"))
}

impl ToolSource {
    pub(super) fn new(
        effect_id: EffectId,
        snapshot: &rsi_ai_protocol::PreparedCallSnapshot,
    ) -> TurnResult<Self> {
        let selection = snapshot
            .language_settings
            .as_ref()
            .map(|settings| {
                ModelRef::new(snapshot.deployment_id.clone(), snapshot.model.clone())
                    .map(|model| ModelSelection {
                        model,
                        reasoning_effort: settings.effective_reasoning_effort.clone(),
                    })
                    .map_err(|error| invalid(&error.to_string()))
            })
            .transpose()?;
        Ok(Self {
            effect_id,
            selection,
            completed: false,
            bytes: 0,
            calls: BTreeMap::new(),
        })
    }

    pub(super) fn observe_shared(source: &mut Arc<Self>, event: &LanguageEvent) -> TurnResult<()> {
        let changes_source = match event {
            LanguageEvent::ContentStarted {
                content: ContentStart::ToolCall { .. },
                ..
            }
            | LanguageEvent::ContentDelta {
                delta: ContentDelta::ToolArguments(_),
                ..
            }
            | LanguageEvent::Finished { .. } => true,
            LanguageEvent::ContentFinished { index } => source.calls.contains_key(index),
            _ => false,
        };
        if changes_source {
            Arc::make_mut(source).observe(event)?;
        }
        Ok(())
    }

    pub(super) fn observe(&mut self, event: &LanguageEvent) -> TurnResult<()> {
        match event {
            LanguageEvent::ContentStarted {
                index,
                content: ContentStart::ToolCall { id, name, kind },
            } => {
                if self.completed
                    || self.calls.len() == rsi_ai_protocol::MAX_CONTENT_BLOCKS
                    || self.calls.contains_key(index)
                    || self.calls.values().any(|call| call.id.as_ref() == id)
                {
                    return Err(invalid("duplicate or excessive call"));
                }
                self.calls.insert(
                    *index,
                    Arc::new(Call {
                        id: Arc::from(id.as_str()),
                        name: Arc::from(name.as_str()),
                        kind: *kind,
                        arguments: Arguments::Building {
                            chunks: Vec::new(),
                            bytes: 0,
                        },
                    }),
                );
            }
            LanguageEvent::ContentDelta {
                index,
                delta: ContentDelta::ToolArguments(delta),
            } => {
                let call = self
                    .calls
                    .get_mut(index)
                    .ok_or_else(|| invalid("arguments have no call"))?;
                let call = Arc::make_mut(call);
                let Arguments::Building { chunks, bytes } = &mut call.arguments else {
                    return Err(invalid("arguments follow call completion"));
                };
                self.bytes = self
                    .bytes
                    .checked_add(delta.len())
                    .ok_or_else(|| invalid("argument size overflow"))?;
                *bytes = bytes
                    .checked_add(delta.len())
                    .ok_or_else(|| invalid("argument size overflow"))?;
                if self.bytes > rsi_ai_protocol::MAX_LANGUAGE_OUTPUT_BYTES
                    || *bytes > rsi_tools_protocol::MAXIMUM_TOOL_JSON_BYTES
                {
                    return Err(invalid("argument byte budget exceeded"));
                }
                let mut remaining = delta.as_bytes();
                while !remaining.is_empty() {
                    if chunks.last().is_none_or(|chunk| chunk.len == CHUNK_BYTES) {
                        chunks.push(ArgumentChunk::new());
                    }
                    let length = chunks.last_mut().expect("chunk inserted").append(remaining);
                    remaining = &remaining[length..];
                }
            }
            LanguageEvent::ContentFinished { index } => {
                if let Some(call) = self.calls.get_mut(index) {
                    let call = Arc::make_mut(call);
                    let Arguments::Building { chunks, bytes } = &call.arguments else {
                        return Err(invalid("duplicate call completion"));
                    };
                    let mut raw = Vec::with_capacity(*bytes);
                    for chunk in chunks {
                        let data = chunk
                            .data
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        raw.extend_from_slice(&data[..chunk.len]);
                    }
                    let text =
                        String::from_utf8(raw).map_err(|_| invalid("invalid argument UTF-8"))?;
                    let arguments = match call.kind {
                        ToolCallKind::Function => {
                            rsi_tools_protocol::parse_tool_arguments(&text).ok()
                        }
                        ToolCallKind::Freeform => Some(serde_json::Value::String(text)),
                    };
                    call.arguments = Arguments::Complete(arguments.as_ref().map(argument_digest));
                }
            }
            LanguageEvent::Finished { reason, .. } => {
                if self
                    .calls
                    .values()
                    .any(|call| matches!(call.arguments, Arguments::Building { .. }))
                {
                    return Err(invalid("model finished with an open call"));
                }
                self.completed = matches!(reason, rsi_ai_protocol::FinishReason::ToolCalls);
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn pending_source(&self) -> Option<&EffectId> {
        (self.completed
            && self
                .calls
                .values()
                .any(|call| matches!(call.arguments, Arguments::Complete(_))))
        .then_some(&self.effect_id)
    }

    pub(super) fn supersede(&mut self, source: &EffectId) -> TurnResult<()> {
        if self.pending_source() != Some(source) {
            return Err(invalid("supersession has no outstanding completed source"));
        }
        for call in self.calls.values_mut() {
            if matches!(call.arguments, Arguments::Complete(_)) {
                Arc::make_mut(call).arguments = Arguments::Consumed;
            }
        }
        Ok(())
    }

    pub(super) fn reject(
        &mut self,
        source: &EffectId,
        id: &str,
        name: &str,
        arguments: &serde_json::Value,
    ) -> TurnResult<()> {
        self.consume_call(source, id, name, arguments)
    }

    pub(super) fn consume(
        &mut self,
        source: &EffectId,
        id: &str,
        name: &str,
        arguments: &serde_json::Value,
    ) -> TurnResult<ModelSelection> {
        let selection = self
            .selection
            .clone()
            .ok_or_else(|| invalid("source has no prepared language settings"))?;
        self.consume_call(source, id, name, arguments)?;
        Ok(selection)
    }

    fn consume_call(
        &mut self,
        source: &EffectId,
        id: &str,
        name: &str,
        arguments: &serde_json::Value,
    ) -> TurnResult<()> {
        if &self.effect_id != source || !self.completed {
            return Err(invalid("source is not the completed Conversation"));
        }
        let call = self
            .calls
            .values_mut()
            .find(|call| call.id.as_ref() == id)
            .ok_or_else(|| invalid("call ID does not occur in source"))?;
        if call.name.as_ref() != name
            || !matches!(&call.arguments, Arguments::Complete(Some(actual)) if *actual == argument_digest(arguments))
        {
            return Err(invalid(
                "call name or arguments disagree, or call was already consumed",
            ));
        }
        Arc::make_mut(call).arguments = Arguments::Consumed;
        Ok(())
    }
}

pub(super) fn validate_caller(turn: &TurnControl, caller: &AgentCallerAuthority) -> TurnResult<()> {
    if let Some(effect) = caller.tool_effect_id() {
        match turn.effects.get(effect) {
            Some(ActiveEffect::Tool {
                started: true,
                source_selection,
                ..
            }) if Some(source_selection.as_ref()) == caller.source_selection() => {}
            _ => return Err(TurnError::StaleClaim),
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(crate) fn source(calls: &[(&str, &str)]) -> ToolSource {
        let high = rsi_ai_protocol::ReasoningEffortId::new("high").unwrap();
        let profile = rsi_ai_protocol::LanguageProfile::new(
            100_000,
            1_000,
            10_000,
            rsi_ai_protocol::ToolDialect::Responses,
            true,
            rsi_ai_protocol::ImageToolResultCapability::No,
            vec![],
        )
        .unwrap()
        .with_reasoning_efforts(
            rsi_ai_protocol::ReasoningEffortProfile::new(vec![high.clone()], Some(high)).unwrap(),
        );
        let snapshot = rsi_ai_protocol::PreparedCallSnapshot {
            call_id: "request".into(),
            deployment_id: "actual".into(),
            model: "source-model".into(),
            provider_family: "fixture".into(),
            capability: rsi_ai_protocol::AiCapability::Language,
            protocol: "fixture".into(),
            transport: "memory".into(),
            endpoint_fingerprint: "fixture".into(),
            config_generation: 1,
            credential_source: None,
            retry_policy: rsi_ai_protocol::RetryPolicy::default(),
            request_sha256: "a".repeat(64),
            language_settings: Some(
                rsi_ai_protocol::PreparedLanguageSettings::new(profile, None).unwrap(),
            ),
        };
        let mut source =
            ToolSource::new(EffectId::new("source-model").unwrap(), &snapshot).unwrap();
        for (index, (id, name)) in calls.iter().enumerate() {
            let index = u32::try_from(index).unwrap();
            source
                .observe(&LanguageEvent::ContentStarted {
                    index,
                    content: ContentStart::ToolCall {
                        id: (*id).into(),
                        name: (*name).into(),
                        kind: ToolCallKind::Function,
                    },
                })
                .unwrap();
            source
                .observe(&LanguageEvent::ContentDelta {
                    index,
                    delta: ContentDelta::ToolArguments("{}".into()),
                })
                .unwrap();
            source
                .observe(&LanguageEvent::ContentFinished { index })
                .unwrap();
        }
        source
            .observe(&LanguageEvent::Finished {
                reason: rsi_ai_protocol::FinishReason::ToolCalls,
                replay: None,
            })
            .unwrap();
        source
    }

    #[test]
    fn source_checks_exact_completed_request_and_consumes_each_call_once() {
        let original = source(&[("call-first", "tool_first"), ("call-second", "tool_second")]);
        let effect = EffectId::new("source-model").unwrap();
        let args = serde_json::json!({});
        for (source_id, id, name, arguments) in [
            (
                EffectId::new("other").unwrap(),
                "call-first",
                "tool_first",
                args.clone(),
            ),
            (effect.clone(), "unknown", "tool_first", args.clone()),
            (effect.clone(), "call-first", "wrong", args.clone()),
            (
                effect.clone(),
                "call-first",
                "tool_first",
                serde_json::json!({"extra":1}),
            ),
        ] {
            assert!(
                original
                    .clone()
                    .consume(&source_id, id, name, &arguments)
                    .is_err()
            );
        }
        let mut incomplete = original.clone();
        incomplete.completed = false;
        assert!(
            incomplete
                .consume(&effect, "call-first", "tool_first", &args)
                .is_err()
        );
        let mut ready = original;
        let first = ready
            .consume(&effect, "call-first", "tool_first", &args)
            .unwrap();
        assert_eq!(first.model.deployment(), "actual");
        assert_eq!(first.reasoning_effort.unwrap().as_str(), "high");
        assert!(
            ready
                .consume(&effect, "call-first", "tool_first", &args)
                .is_err()
        );
        ready
            .consume(&effect, "call-second", "tool_second", &args)
            .unwrap();
    }

    #[test]
    fn supersession_requires_an_exact_completed_source_and_only_consumes_remaining_calls() {
        let effect = EffectId::new("source-model").unwrap();
        let original = source(&[("first", "read"), ("second", "read")]);
        let mut incomplete = original.clone();
        incomplete.completed = false;
        assert!(incomplete.supersede(&effect).is_err());
        assert!(
            original
                .clone()
                .supersede(&EffectId::new("wrong").unwrap())
                .is_err()
        );
        let mut rejected = original.clone();
        rejected
            .reject(
                &EffectId::new("source-model").unwrap(),
                "first",
                "read",
                &serde_json::json!({}),
            )
            .unwrap();
        rejected
            .reject(
                &EffectId::new("source-model").unwrap(),
                "second",
                "read",
                &serde_json::json!({}),
            )
            .unwrap();
        assert!(rejected.pending_source().is_none());
        assert!(rejected.supersede(&effect).is_err());
        let mut remaining = original;
        remaining
            .consume(&effect, "first", "read", &serde_json::json!({}))
            .unwrap();
        // Invalid model arguments can be superseded without preparing a Tool.
        Arc::make_mut(remaining.calls.get_mut(&1).unwrap()).arguments = Arguments::Complete(None);
        assert!(
            remaining
                .reject(
                    &EffectId::new("source-model").unwrap(),
                    "second",
                    "read",
                    &serde_json::json!({})
                )
                .is_err(),
            "invalid JSON cannot establish a different typed rejection identity"
        );
        assert_eq!(remaining.pending_source(), Some(&effect));
        remaining.supersede(&effect).unwrap();
        assert!(remaining.pending_source().is_none());
        assert!(remaining.supersede(&effect).is_err());
        assert!(
            remaining
                .consume(&effect, "second", "read", &serde_json::json!({}))
                .is_err()
        );
    }

    #[test]
    fn rejecting_a_proven_call_does_not_require_execution_settings() {
        let mut source = source(&[("call", "read")]);
        source.selection = None;
        assert!(
            source
                .consume(
                    &EffectId::new("source-model").unwrap(),
                    "call",
                    "read",
                    &serde_json::json!({})
                )
                .is_err()
        );
        source
            .reject(
                &EffectId::new("source-model").unwrap(),
                "call",
                "read",
                &serde_json::json!({}),
            )
            .unwrap();
        assert!(source.pending_source().is_none());
        assert!(
            source
                .reject(
                    &EffectId::new("source-model").unwrap(),
                    "call",
                    "read",
                    &serde_json::json!({})
                )
                .is_err()
        );
    }

    #[test]
    fn simultaneous_speculative_appends_preserve_both_branches() {
        let mut base = ArgumentChunk::new();
        base.append(b"prefix");
        let gate = Arc::new(std::sync::Barrier::new(3));
        let threads: Vec<_> = [b"-first".as_slice(), b"-second".as_slice()]
            .into_iter()
            .map(|suffix| {
                let mut branch = base.clone();
                let gate = gate.clone();
                std::thread::spawn(move || {
                    gate.wait();
                    branch.append(suffix);
                    let data = branch.data.lock().unwrap();
                    assert_eq!(&data[..branch.len], [b"prefix".as_slice(), suffix].concat());
                })
            })
            .collect();
        gate.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(&base.data.lock().unwrap()[..base.len], b"prefix");
    }

    #[test]
    fn speculative_chunk_prefixes_are_isolated_and_divergent_writes_do_not_overwrite() {
        let mut original = ArgumentChunk::new();
        original.append(b"prefix");
        let mut first = original.clone();
        first.append(b"-first");
        assert!(Arc::ptr_eq(&original.data, &first.data));
        let mut second = original.clone();
        second.append(b"-second");
        assert!(!Arc::ptr_eq(&first.data, &second.data));
        for (chunk, expected) in [
            (&original, &b"prefix"[..]),
            (&first, &b"prefix-first"[..]),
            (&second, &b"prefix-second"[..]),
        ] {
            let data = chunk.data.lock().unwrap();
            assert_eq!(&data[..chunk.len], expected);
            assert!(data.len() <= CHUNK_BYTES);
        }
        let mut source = Arc::new(source(&[("call-first", "tool_first")]));
        let previous = Arc::clone(&source);
        for event in [
            LanguageEvent::ContentDelta {
                index: 20,
                delta: ContentDelta::Text("text".into()),
            },
            LanguageEvent::ContentDelta {
                index: 20,
                delta: ContentDelta::Reasoning("thinking".into()),
            },
            LanguageEvent::ContentFinished { index: 20 },
        ] {
            ToolSource::observe_shared(&mut source, &event).unwrap();
            assert!(Arc::ptr_eq(&source, &previous));
        }
    }

    #[test]
    fn argument_chunks_preserve_speculative_copy_and_bound_fragment_growth() {
        let mut source = source(&[]);
        source.completed = false;
        source
            .observe(&LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::ToolCall {
                    id: "call".into(),
                    name: "tool".into(),
                    kind: ToolCallKind::Freeform,
                },
            })
            .unwrap();
        let delta = "字".repeat(10_000);
        for _ in 0..16 {
            source
                .observe(&LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::ToolArguments(delta.clone()),
                })
                .unwrap();
        }
        let before = source.clone();
        source
            .observe(&LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::ToolArguments("suffix".into()),
            })
            .unwrap();
        if let Arguments::Building { chunks, .. } = &source.calls[&0].arguments {
            let Arguments::Building {
                chunks: previous, ..
            } = &before.calls[&0].arguments
            else {
                unreachable!()
            };
            assert!(
                Arc::ptr_eq(&chunks.last().unwrap().data, &previous.last().unwrap().data),
                "appending must not copy the shared 64 KiB tail"
            );
        }
        if let Arguments::Building { chunks, bytes } = &before.calls[&0].arguments {
            assert_eq!(*bytes, delta.len() * 16);
            assert_eq!(chunks.len(), bytes.div_ceil(CHUNK_BYTES));
            assert!(chunks.iter().all(|chunk| chunk.len <= CHUNK_BYTES));
        } else {
            panic!("building source expected");
        }
        source
            .observe(&LanguageEvent::ContentFinished { index: 0 })
            .unwrap();
        source
            .observe(&LanguageEvent::Finished {
                reason: rsi_ai_protocol::FinishReason::ToolCalls,
                replay: None,
            })
            .unwrap();
        let arguments = serde_json::Value::String(format!("{}suffix", delta.repeat(16)));
        source
            .consume(
                &EffectId::new("source-model").unwrap(),
                "call",
                "tool",
                &arguments,
            )
            .unwrap();
    }
}

#[cfg(test)]
mod digest_tests {
    use super::*;
    #[test]
    fn argument_fingerprint_ignores_nested_object_order_but_preserves_array_order() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{"z":1,"a":{"y":2,"x":[3,4]}}"#).unwrap();
        let b: serde_json::Value =
            serde_json::from_str(r#"{"a":{"x":[3,4],"y":2},"z":1}"#).unwrap();
        assert_eq!(argument_digest(&a), argument_digest(&b));
        let changed: serde_json::Value =
            serde_json::from_str(r#"{"a":{"x":[4,3],"y":2},"z":1}"#).unwrap();
        assert_ne!(argument_digest(&a), argument_digest(&changed));
    }
}
