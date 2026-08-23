use rsi_ai_protocol::{
    AiError, ContentBlock, ContentDelta, ContentStart, DispatchStatus, ErrorKind, ErrorPhase,
    FinishReason, LanguageAssembler, LanguageAssemblyError, LanguageEvent, LanguageOutput,
    ProviderExtension, Source, TokenUsage, ToolCall, Warning,
};

fn usage() -> TokenUsage {
    TokenUsage {
        input_tokens: 12,
        output_tokens: 7,
        cache_read_tokens: Some(3),
        cache_write_tokens: None,
        reasoning_tokens: Some(2),
    }
}

#[test]
fn language_stream_bounds_and_validates_sources_and_warnings() {
    let mut invalid_source = LanguageAssembler::new();
    let error = invalid_source
        .push(&LanguageEvent::Source {
            source: Source {
                id: "source-1".to_owned(),
                title: Some("unsafe\0title".to_owned()),
                url: Some("https://example.test".to_owned()),
            },
        })
        .expect_err("unsafe source metadata");
    assert_eq!(error.code(), "stream.invalid_source");

    let mut invalid_warning = LanguageAssembler::new();
    let error = invalid_warning
        .push(&LanguageEvent::Warning {
            warning: Warning {
                code: "not a token".to_owned(),
                message: "warning".to_owned(),
            },
        })
        .expect_err("invalid warning code");
    assert_eq!(error.code(), "stream.invalid_warning");

    let mut too_many_sources = LanguageAssembler::new();
    for index in 0..256 {
        too_many_sources
            .push(&LanguageEvent::Source {
                source: Source {
                    id: format!("source-{index}"),
                    title: None,
                    url: None,
                },
            })
            .expect("source within count limit");
    }
    let error = too_many_sources
        .push(&LanguageEvent::Source {
            source: Source {
                id: "source-overflow".to_owned(),
                title: None,
                url: None,
            },
        })
        .expect_err("source count is bounded");
    assert_eq!(error.code(), "stream.too_many_sources");
}

#[test]
fn language_stream_assembles_interleaved_reasoning_text_and_tool_arguments() {
    let mut assembler = LanguageAssembler::new();
    let events = [
        LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::Reasoning,
        },
        LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Reasoning("check ".to_owned()),
        },
        LanguageEvent::ContentStarted {
            index: 1,
            content: ContentStart::Text,
        },
        LanguageEvent::ContentDelta {
            index: 1,
            delta: ContentDelta::Text("done".to_owned()),
        },
        LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Reasoning("facts".to_owned()),
        },
        LanguageEvent::ContentStarted {
            index: 2,
            content: ContentStart::ToolCall {
                id: "call-1".to_owned(),
                name: "lookup".to_owned(),
                kind: rsi_ai_protocol::ToolCallKind::Function,
            },
        },
        LanguageEvent::ContentDelta {
            index: 2,
            delta: ContentDelta::ToolArguments("{not-json".to_owned()),
        },
        LanguageEvent::ContentFinished { index: 0 },
        LanguageEvent::ContentFinished { index: 1 },
        LanguageEvent::ContentFinished { index: 2 },
        LanguageEvent::Usage { usage: usage() },
        LanguageEvent::Finished {
            reason: FinishReason::ToolCalls,
            replay: None,
        },
    ];

    for event in events {
        assembler.push(&event).expect("valid event");
    }

    assert_eq!(
        assembler.finish().expect("complete output"),
        LanguageOutput {
            content: vec![
                ContentBlock::Reasoning {
                    text: "check facts".to_owned(),
                },
                ContentBlock::Text {
                    text: "done".to_owned(),
                },
                ContentBlock::ToolCall(ToolCall {
                    id: "call-1".to_owned(),
                    name: "lookup".to_owned(),
                    arguments: "{not-json".to_owned(),
                    kind: rsi_ai_protocol::ToolCallKind::Function,
                }),
            ],
            finish_reason: FinishReason::ToolCalls,
            usage: Some(usage()),
            replay: None,
            warnings: Vec::new(),
            sources: Vec::new(),
        }
    );
}

#[test]
fn language_stream_rejects_out_of_order_or_post_terminal_events() {
    let mut missing_start = LanguageAssembler::new();
    let error = missing_start
        .push(&LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("orphan".to_owned()),
        })
        .expect_err("delta without start");
    assert_eq!(error.code(), "stream.content_not_started");

    let mut post_terminal = LanguageAssembler::new();
    post_terminal
        .push(&LanguageEvent::Finished {
            reason: FinishReason::Stop,
            replay: None,
        })
        .expect("finish");
    let error = post_terminal
        .push(&LanguageEvent::Usage { usage: usage() })
        .expect_err("event after terminal");
    assert_eq!(error.code(), "stream.already_finished");
}

#[test]
fn language_stream_requires_every_block_to_close_before_terminal() {
    let mut assembler = LanguageAssembler::new();
    assembler
        .push(&LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::Text,
        })
        .expect("start");
    let error = assembler
        .push(&LanguageEvent::Finished {
            reason: FinishReason::Stop,
            replay: None,
        })
        .expect_err("open block");
    assert_eq!(error.code(), "stream.content_still_open");
}

#[test]
fn language_stream_requires_one_terminal_event() {
    let assembler = LanguageAssembler::new();
    let error = assembler.finish().expect_err("missing terminal");
    assert_eq!(error.code(), "stream.missing_finish");
}

#[test]
fn provider_extensions_obey_the_shared_json_complexity_bound() {
    let mut value = serde_json::Value::Null;
    for _ in 0..=rsi_ai_protocol::MAX_JSON_DEPTH {
        value = serde_json::Value::Array(vec![value]);
    }

    let mut assembler = LanguageAssembler::new();
    let error = assembler
        .push(&LanguageEvent::Finished {
            reason: FinishReason::Stop,
            replay: Some(ProviderExtension {
                namespace: "provider.test".to_owned(),
                version: 1,
                value,
            }),
        })
        .expect_err("provider extensions use the shared JSON depth bound");
    assert_eq!(error.code(), "stream.invalid_extension");
}

#[test]
fn language_stream_preserves_content_index_order_when_blocks_finish_out_of_order() {
    let mut assembler = LanguageAssembler::new();
    for event in [
        LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::Reasoning,
        },
        LanguageEvent::ContentStarted {
            index: 1,
            content: ContentStart::Text,
        },
        LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Reasoning("first".to_owned()),
        },
        LanguageEvent::ContentDelta {
            index: 1,
            delta: ContentDelta::Text("second".to_owned()),
        },
        LanguageEvent::ContentFinished { index: 1 },
        LanguageEvent::ContentFinished { index: 0 },
        LanguageEvent::Finished {
            reason: FinishReason::Stop,
            replay: None,
        },
    ] {
        assembler.push(&event).expect("valid interleaving");
    }

    assert_eq!(
        assembler.finish().expect("output").content,
        vec![
            ContentBlock::Reasoning {
                text: "first".to_owned(),
            },
            ContentBlock::Text {
                text: "second".to_owned(),
            },
        ]
    );
}

#[test]
fn provider_failure_is_terminal_and_exposes_validated_partial_output() {
    let mut assembler = LanguageAssembler::new();
    for event in [
        LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::Text,
        },
        LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("partial".to_owned()),
        },
        LanguageEvent::ContentFinished { index: 0 },
        LanguageEvent::Failed {
            error: AiError::new(
                ErrorKind::Server,
                ErrorPhase::Stream,
                DispatchStatus::Dispatched,
                "provider stopped the stream",
            )
            .expect("bounded error"),
            replay: None,
        },
    ] {
        assembler.push(&event).expect("valid event");
    }

    let LanguageAssemblyError::Provider { error, partial } =
        assembler.finish().expect_err("provider failure")
    else {
        panic!("expected provider failure")
    };
    assert_eq!(error.kind(), ErrorKind::Server);
    assert_eq!(error.phase(), ErrorPhase::Stream);
    assert_eq!(error.dispatch_status(), DispatchStatus::Dispatched);
    assert_eq!(error.safe_summary(), "provider stopped the stream");
    assert_eq!(
        partial.content,
        vec![ContentBlock::Text {
            text: "partial".to_owned(),
        }]
    );

    let error = assembler_after_failure_error();
    assert_eq!(error.code(), "stream.already_finished");
}

fn assembler_after_failure_error() -> rsi_ai_protocol::StreamError {
    let mut assembler = LanguageAssembler::new();
    assembler
        .push(&LanguageEvent::Failed {
            error: AiError::new(
                ErrorKind::Transport,
                ErrorPhase::FirstEvent,
                DispatchStatus::Unknown,
                "connection closed",
            )
            .expect("bounded error"),
            replay: None,
        })
        .expect("terminal failure");
    assembler
        .push(&LanguageEvent::Finished {
            reason: FinishReason::Stop,
            replay: None,
        })
        .expect_err("second terminal")
}
