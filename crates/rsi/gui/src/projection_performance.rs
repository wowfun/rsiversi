//! Opt-in Linux thread-CPU comparison of the actual bounded projection.
//! Uncached serialization below is copied from 6bf9809:crates/rsi/web/src/projection.rs.
//! Its JSON is asserted identical before timing. This isolates caching, not an old binary.
use super::*;
use std::{hint::black_box, sync::atomic::Ordering};
struct Uncached<'a>(&'a Block);
impl Serialize for Uncached<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let markdown = (self.0.role == "assistant")
            .then(|| crate::markdown::parse(&self.0.text))
            .flatten();
        let mut view = serializer.serialize_struct(
            "Block",
            6 + usize::from(self.0.tool.is_some()) + usize::from(markdown.is_some()),
        )?;
        view.serialize_field("key", &self.0.key)?;
        view.serialize_field("role", &self.0.role)?;
        view.serialize_field("title", &self.0.title)?;
        view.serialize_field("text", &self.0.text)?;
        view.serialize_field("clipped", &self.0.clipped)?;
        view.serialize_field("sources", &self.0.sources.len())?;
        if let Some(tool) = &self.0.tool {
            view.serialize_field("tool", tool)?;
        }
        if let Some(markdown) = markdown {
            view.serialize_field("markdown", &markdown)?;
        }
        view.end()
    }
}
fn cpu() -> u64 {
    std::fs::read_to_string("/proc/thread-self/schedstat")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}
fn scene(count: usize) -> Transcript {
    let mut transcript = Transcript::default();
    let text = "## Result\n\nA **bounded** response with `code`, [reference](https://example.com), and a list:\n\n- First item\n- Second item\n\n```rust\nlet answer = 42;\n```\n".repeat(4);
    for index in 0..count {
        transcript.add(
            format!("block-{index}"),
            "assistant",
            "Assistant",
            &text,
            false,
        );
    }
    transcript
}
fn measure(count: usize, cached: bool) -> (u64, usize) {
    let mut transcript = scene(count);
    black_box(serde_json::to_vec(&transcript.blocks).unwrap());
    let unchanged = || {
        transcript
            .blocks
            .iter()
            .take(count - 1)
            .map(|block| block.markdown_parses.load(Ordering::Relaxed))
            .sum::<usize>()
    };
    let before = unchanged();
    let started = cpu();
    for tick in 0..500 {
        transcript.add(
            format!("block-{}", count - 1),
            "assistant",
            "Assistant",
            &format!("**Streaming** update {tick}"),
            false,
        );
        if cached {
            black_box(serde_json::to_vec(&transcript.blocks).unwrap());
        } else {
            black_box(
                serde_json::to_vec(&transcript.blocks.iter().map(Uncached).collect::<Vec<_>>())
                    .unwrap(),
            );
        }
    }
    let elapsed = cpu() - started;
    let after = transcript
        .blocks
        .iter()
        .take(count - 1)
        .map(|block| block.markdown_parses.load(Ordering::Relaxed))
        .sum::<usize>();
    (elapsed, after - before)
}
#[test]
#[ignore = "opt-in ten-run Linux CPU measurement; run alone with --nocapture"]
fn projection_performance() {
    for count in [16, 64, 128] {
        let transcript = scene(count);
        assert_eq!(
            serde_json::to_vec(&transcript.blocks).unwrap(),
            serde_json::to_vec(&transcript.blocks.iter().map(Uncached).collect::<Vec<_>>())
                .unwrap()
        );
        for run in 0..10 {
            let (baseline, cached) = if run % 2 == 0 {
                (measure(count, false), measure(count, true))
            } else {
                let cached = measure(count, true);
                (measure(count, false), cached)
            };
            assert_eq!(cached.1, 0, "unchanged finalized blocks must not reparse");
            println!(
                "PROJECTION_PERF {}",
                serde_json::json!({"blocks":count,"run":run,"iterations":500,"baseline_cpu_ns":baseline.0,"cached_cpu_ns":cached.0,"unchanged_reparses":cached.1})
            );
        }
    }
}
