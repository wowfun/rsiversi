//! Report-only measurement; the terminal I/O owner has separate PTY tests.
use crate::test_state::{TestState, draw};
use ratatui::{Terminal, backend::TestBackend};
use rsi_agent_session_protocol::{
    AgentPresetId, EffectId, FrozenAgentSettings, SessionFact, SessionFactBody, SessionHeader,
    SessionId, TurnId,
};
use rsi_ai_protocol::{ContentDelta, LanguageEvent, ModelRef};
use std::{hint::black_box, time::Instant};

fn cpu() -> u64 {
    std::fs::read_to_string("/proc/thread-self/schedstat")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

fn delta(state: &mut TestState, seq: u64, block: usize, text: &str) {
    state.transcript.apply(
        &SessionFact::new(
            seq,
            seq,
            SessionFactBody::ModelEvent {
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                turn_id: TurnId::new("performance").unwrap(),
                effect_id: EffectId::new(format!("block-{block}")).unwrap(),
                event: LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text(text.into()),
                },
            },
        )
        .unwrap(),
    );
}

fn state(count: usize) -> TestState {
    let header = SessionHeader::new(
        SessionId::new("performance").unwrap(),
        1,
        "/isolated/performance",
        AgentPresetId::new("default").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "system",
            ModelRef::new("fixture", "fixture").unwrap(),
            rsi_sandbox::SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let mut state = TestState::new(header, false);
    let text = "## Result\n\nA **bounded** response with `code`, [reference](https://example.com), and a list:\n\n- First item\n- Second item\n\n```rust\nlet answer = 42;\n```\n".repeat(4);
    for block in 0..count {
        delta(&mut state, block as u64 + 1, block, &text);
    }
    assert_eq!(state.transcript.blocks.len(), count);
    state
}

#[test]
#[ignore = "report-only Linux CPU/cell rendering measurement"]
fn deterministic_render_cost() {
    let report = std::env::var("RSI_TUI_PERFORMANCE_REPORT").expect("explicit report path");
    let mut rows = Vec::new();
    for count in [16, 64, 128] {
        for run in 0..10 {
            let mut state = state(count);
            let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
            terminal
                .draw(|frame| {
                    black_box(draw(frame, &state));
                })
                .unwrap();
            let mut elapsed = Vec::new();
            let started = cpu();
            for tick in 0..50 {
                if tick % 5 == 0 {
                    state.editor = crate::editor::Editor::default();
                }
                let input = Instant::now();
                state.editor.insert("x").unwrap();
                delta(&mut state, count as u64 + tick + 1, count - 1, "x");
                terminal
                    .draw(|frame| {
                        black_box(draw(frame, &state));
                    })
                    .unwrap();
                elapsed.push(input.elapsed().as_nanos());
            }
            let cpu_ns = cpu() - started;
            let cells = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>();
            assert!(cells.contains("xxxxx"), "editor must be rendered");
            assert!(cells.contains("Result"), "Markdown body must be rendered");
            rows.push(serde_json::json!({"blocks": count, "run": run, "cpu_ns": cpu_ns, "edit_to_cells_ns": elapsed}));
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(report)
        .unwrap();
    serde_json::to_writer_pretty(&mut file, &serde_json::json!({
        "format": 1, "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "width": 120, "height": 40, "iterations_per_run": 50, "runs": rows,
        "boundary": "Linux thread CPU and TestBackend cells; excludes terminal I/O and physical paint"
    })).unwrap();
}
