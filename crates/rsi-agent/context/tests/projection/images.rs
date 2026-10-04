use super::*;
use rsi_agent_context::{ContextPage, DefaultContextBuilder, ModelContextState};
use rsi_ai_protocol::{
    ImageToolResultCapability, ImageToolResultMode, LanguageProfile, LanguageRequestOptions,
    ToolDialect,
};
use rsi_media_protocol::{MediaId, MediaRef};
use rsi_tools_protocol::ToolContent;
use std::sync::Arc;

fn profile(capability: ImageToolResultCapability) -> LanguageProfile {
    LanguageProfile::new(
        128_000,
        4096,
        32768,
        ToolDialect::Responses,
        true,
        capability,
        vec![],
    )
    .unwrap()
}

fn image_history() -> Vec<SessionFact> {
    let media = MediaRef {
        id: MediaId::new("a".repeat(64)).unwrap(),
        mime: "image/png".into(),
        bytes: 120,
        width: 2,
        height: 3,
    };
    let result = ToolResult::new(
        json!({"opaque":"original"}),
        vec![
            ToolContent::Text {
                text: "before".into(),
            },
            ToolContent::Image { media },
            ToolContent::Text {
                text: "after".into(),
            },
        ],
        true,
    )
    .unwrap();
    let mut source = tool_result_facts(result);
    source.push(
        SessionFact::new(
            source.len() as u64 + 1,
            50,
            SessionFactBody::TurnTerminal {
                turn_id: TurnId::new("turn-1").unwrap(),
                outcome: TurnOutcome::Completed,
                result: None,
            },
        )
        .unwrap(),
    );
    source
}

#[test]
fn tool_images_follow_each_target_without_changing_facts_checkpoint_or_fork() {
    let source = image_history();
    let original = serde_json::to_vec(&source).unwrap();
    let source: Vec<_> = source.into_iter().map(Arc::new).collect();
    let mut state = ModelContextState::open(
        Arc::new(DefaultContextBuilder::default()),
        header(""),
        ContextLimits::default(),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    state.ingest(ContextPage::Canonical(&source)).unwrap();
    let checkpoint = state.checkpoint().unwrap();
    let yes = profile(ImageToolResultCapability::Yes(
        ImageToolResultMode::FunctionOutput,
    ));
    let rich = state
        .build(LanguageRequestOptions::default(), &yes)
        .unwrap();
    for capability in [
        ImageToolResultCapability::No,
        ImageToolResultCapability::Unknown,
    ] {
        let request = state
            .build(LanguageRequestOptions::default(), &profile(capability))
            .unwrap();
        let [
            MessageContent::ToolResult {
                call_id,
                content,
                is_error,
            },
        ] = request.messages()[2].content()
        else {
            panic!("missing tool result")
        };
        assert_eq!(call_id, "tool-call");
        assert!(*is_error);
        assert!(matches!(&content[0], MessageContent::Text {text} if text=="before"));
        assert!(
            matches!(&content[1], MessageContent::Text {text} if text.contains(&"a".repeat(64)) && text.contains("120 bytes"))
        );
        assert!(matches!(&content[2], MessageContent::Text {text} if text=="after"));
        assert_eq!(state.checkpoint().unwrap(), checkpoint);
    }
    assert_eq!(
        state
            .build(LanguageRequestOptions::default(), &yes)
            .unwrap(),
        rich
    );
    let restored = state.restored(&checkpoint).unwrap();
    assert_eq!(
        restored
            .build(LanguageRequestOptions::default(), &yes)
            .unwrap(),
        rich
    );
    assert_eq!(
        serde_json::to_vec(&source.iter().map(AsRef::as_ref).collect::<Vec<_>>()).unwrap(),
        original
    );
    let mut child = ModelContextState::open(
        Arc::new(DefaultContextBuilder::default()),
        fork_header("", source.len() as u64, 1),
        ContextLimits::default(),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    child.ingest(ContextPage::ForkSeed(&source)).unwrap();
    child.ingest(ContextPage::FinishSeed).unwrap();
    assert_eq!(
        child
            .build(LanguageRequestOptions::default(), &yes)
            .unwrap(),
        rich
    );
    let text = child
        .build(
            LanguageRequestOptions::default(),
            &profile(ImageToolResultCapability::No),
        )
        .unwrap();
    assert!(
        !serde_json::to_string(&text)
            .unwrap()
            .contains("\"type\":\"image\"")
    );
}

#[test]
fn installed_image_history_summary_survives_cold_restore_and_profile_switching() {
    use super::compaction::{accepted, append, model_bodies};
    use rsi_agent_session_protocol::{CompactionTrigger, ModelPurpose};
    let mut history: Vec<_> = image_history().into_iter().map(Arc::new).collect();
    let mut state = ModelContextState::open(
        Arc::new(DefaultContextBuilder::default()),
        header(""),
        ContextLimits::default(),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    state.ingest(ContextPage::Canonical(&history)).unwrap();
    append(
        &mut state,
        &mut history,
        vec![accepted("current", "next task")],
    );
    append(
        &mut state,
        &mut history,
        model_bodies(
            "current",
            "tail",
            ModelPurpose::Conversation,
            &"tail ".repeat(15_000),
            None,
            FinishReason::Stop,
        ),
    );
    let yes = profile(ImageToolResultCapability::Yes(
        ImageToolResultMode::FunctionOutput,
    ));
    let no = profile(ImageToolResultCapability::No);
    let model = ModelRef::new("deployment", "model").unwrap();
    let options = LanguageRequestOptions::default();
    let rich = state
        .plan_compaction(
            &options,
            &model,
            &yes,
            Some(CompactionTrigger::ProviderContextLimit),
            false,
        )
        .unwrap()
        .unwrap();
    state.build(options.clone(), &no).unwrap();
    let textual = state
        .plan_compaction(
            &options,
            &model,
            &no,
            Some(CompactionTrigger::ProviderContextLimit),
            false,
        )
        .unwrap()
        .unwrap();
    assert_eq!(rich.plan, textual.plan);
    assert_eq!(rich.request, textual.request);
    append(
        &mut state,
        &mut history,
        model_bodies(
            "current",
            "image-summary",
            ModelPurpose::ContextCompaction(Box::new(rich.plan)),
            "The preceding tool returned a small PNG image and its metadata.",
            None,
            FinishReason::Stop,
        ),
    );
    let effect = EffectId::new("image-summary").unwrap();
    assert!(state.summary_installed(&effect));
    let checkpoint = state.checkpoint().unwrap();
    let mut replay = ModelContextState::open(
        Arc::new(DefaultContextBuilder::default()),
        header(""),
        ContextLimits::default(),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    replay.ingest(ContextPage::Canonical(&history)).unwrap();
    for restored in [state.restored(&checkpoint).unwrap(), replay] {
        assert!(restored.summary_installed(&effect));
        for profile in [&yes, &no] {
            assert_eq!(
                restored.build(options.clone(), profile).unwrap(),
                state.build(options.clone(), profile).unwrap()
            );
        }
        assert_eq!(restored.checkpoint().unwrap(), checkpoint);
    }
}
