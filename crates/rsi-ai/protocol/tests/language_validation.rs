use rsi_ai_protocol::{
    AiError, ContentDelta, ContentStart, DispatchStatus, ErrorKind, ErrorPhase, FinishReason,
    LanguageAssembler, LanguageAssemblyError, LanguageEvent, LanguageStreamValidator,
    MAX_LANGUAGE_OUTPUT_BYTES, MAX_SOURCES, MAX_WARNINGS, Source, TokenUsage, ToolCallKind,
    Warning,
};

fn start(index: u32, tool: bool) -> LanguageEvent {
    LanguageEvent::ContentStarted {
        index,
        content: if tool {
            ContentStart::ToolCall {
                id: "call".into(),
                name: "write".into(),
                kind: ToolCallKind::Function,
            }
        } else {
            ContentStart::Text
        },
    }
}

fn end(reason: FinishReason) -> LanguageEvent {
    LanguageEvent::Finished {
        reason,
        replay: None,
    }
}

fn check(events: impl IntoIterator<Item = LanguageEvent>, expected: Result<(), &'static str>) {
    let mut validator = LanguageStreamValidator::new();
    let mut assembler = LanguageAssembler::new();
    for event in events {
        let checked = validator.push(&event).map_err(|error| error.code());
        assert_eq!(
            checked,
            assembler.push(&event).map_err(|error| error.code())
        );
        if checked.is_err() {
            assert_eq!(checked, expected);
            return;
        }
    }
    assert_eq!(validator.finish().map_err(|error| error.code()), expected);
    let outcome = match assembler.finish() {
        Err(LanguageAssemblyError::Protocol(error)) => Err(error.code()),
        Ok(_)
        | Err(LanguageAssemblyError::Incomplete { .. } | LanguageAssemblyError::Provider { .. }) => {
            Ok(())
        }
    };
    assert_eq!(outcome, expected);
}

#[test]
fn evidence_validation_preserves_terminal_and_block_grammar() {
    check([], Err("stream.missing_finish"));
    check([end(FinishReason::Stop)], Err("stream.empty_output"));
    check([start(1, false)], Err("stream.non_contiguous_index"));
    check(
        [start(0, false), start(0, true)],
        Err("stream.non_contiguous_index"),
    );
    check(
        [LanguageEvent::ContentFinished { index: 0 }],
        Err("stream.content_not_started"),
    );
    check(
        [
            start(0, false),
            LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Reasoning("x".into()),
            },
        ],
        Err("stream.content_type_mismatch"),
    );
    check(
        [
            start(0, false),
            LanguageEvent::ContentFinished { index: 0 },
            LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Text("x".into()),
            },
        ],
        Err("stream.content_not_started"),
    );
    check(
        [start(0, false), end(FinishReason::Stop)],
        Err("stream.content_still_open"),
    );
    check(
        [
            start(0, true),
            LanguageEvent::ContentFinished { index: 0 },
            end(FinishReason::Stop),
        ],
        Err("stream.finish_reason_mismatch"),
    );
    check(
        [
            start(0, false),
            LanguageEvent::ContentFinished { index: 0 },
            end(FinishReason::ToolCalls),
        ],
        Err("stream.finish_reason_mismatch"),
    );
    let usage = LanguageEvent::Usage {
        usage: TokenUsage::default(),
    };
    check([usage.clone(), usage], Err("stream.duplicate_usage"));
    check(
        [end(FinishReason::Cancelled), start(0, false)],
        Err("stream.already_finished"),
    );

    for reason in [
        FinishReason::Stop,
        FinishReason::ToolCalls,
        FinishReason::ContentFilter,
        FinishReason::MaxTokens,
        FinishReason::Cancelled,
    ] {
        check(
            [
                start(0, reason == FinishReason::ToolCalls),
                LanguageEvent::ContentFinished { index: 0 },
                end(reason),
            ],
            Ok(()),
        );
    }
    for reason in [FinishReason::MaxTokens, FinishReason::Cancelled] {
        check([end(reason.clone())], Ok(()));
        check(
            [
                start(0, true),
                LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::ToolArguments("{unfinished".into()),
                },
                end(reason),
            ],
            Ok(()),
        );
    }
}

#[test]
fn validator_enforces_aggregate_payload_and_metadata_limits() {
    let mut validator = LanguageStreamValidator::new();
    validator.push(&start(0, false)).unwrap();
    let delta = LanguageEvent::ContentDelta {
        index: 0,
        delta: ContentDelta::Text("x".repeat(64 * 1024)),
    };
    for _ in 0..MAX_LANGUAGE_OUTPUT_BYTES / (64 * 1024) {
        validator.push(&delta).unwrap();
    }
    assert_eq!(
        validator.push(&delta).unwrap_err().code(),
        "stream.output_too_large"
    );

    for (event, count, code) in [
        (
            LanguageEvent::Source {
                source: Source {
                    id: "id".into(),
                    title: None,
                    url: None,
                },
            },
            MAX_SOURCES,
            "stream.too_many_sources",
        ),
        (
            LanguageEvent::Warning {
                warning: Warning {
                    code: "warning".into(),
                    message: "message".into(),
                },
            },
            MAX_WARNINGS,
            "stream.too_many_warnings",
        ),
    ] {
        let mut validator = LanguageStreamValidator::new();
        for _ in 0..count {
            validator.push(&event).unwrap();
        }
        assert_eq!(validator.push(&event).unwrap_err().code(), code);
    }
}

#[test]
fn provider_failure_admits_partial_evidence_without_becoming_success() {
    check(
        [
            start(0, true),
            LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::ToolArguments("{partial".into()),
            },
            LanguageEvent::Failed {
                error: AiError::new(
                    ErrorKind::OutputValidation,
                    ErrorPhase::Assemble,
                    DispatchStatus::Dispatched,
                    "invalid provider response",
                )
                .unwrap(),
                replay: None,
            },
        ],
        Ok(()),
    );
}
