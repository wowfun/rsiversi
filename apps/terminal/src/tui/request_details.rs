use super::{
    state::{Action, State},
    transcript,
};

pub(super) fn metrics_text(read: &rsi_session_protocol::MetricsRead, configured: bool) -> String {
    let m = &read.summary;
    let mut text = format!(
        "Session usage{}\n{} requests · {} failed\n{} input tokens · {} output tokens\nUsage reported for {} of {} requests\n",
        if read.complete {
            ""
        } else {
            " · still loading"
        },
        m.attempts,
        m.failed_attempts,
        m.tokens.input_tokens(),
        m.tokens.output_tokens(),
        m.reported_attempts,
        m.attempts
    );
    for (label, value) in [
        ("Cache read", m.tokens.cache_read_tokens()),
        ("Cache write", m.tokens.cache_write_tokens()),
        ("Reasoning", m.tokens.reasoning_tokens()),
    ] {
        if let Some(value) = value {
            let _ =
                std::fmt::Write::write_fmt(&mut text, format_args!("{label}: {value} tokens\n"));
        }
    }
    if let Some(last) = &m.last_attempt {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!("\nLast request: {}", last.model.model()),
        );
        if let Some(effort) = &last.reasoning_effort {
            let _ = std::fmt::Write::write_fmt(&mut text, format_args!(" / {effort}"));
        }
        if let Some(ms) = last.elapsed_ms {
            let _ = std::fmt::Write::write_fmt(
                &mut text,
                format_args!(" · {}.{:03}s", ms / 1000, ms % 1000),
            );
        }
        if last.failed {
            text.push_str(" · failed");
        }
        text.push('\n');
    }
    if configured {
        let cost = &m.configured_cost;
        text.push_str("\nConfigured cost");
        if !cost.is_complete() || !read.complete {
            text.push_str(" · partial");
        }
        text.push('\n');
        for total in &cost.totals {
            text.push_str(&total.display());
            text.push('\n');
        }
        if cost.totals.is_empty() {
            text.push_str("No priced usage available\n");
        }
        for (label, count) in [
            ("Missing price", cost.missing_price),
            ("Missing usage", cost.missing_usage),
            ("Missing cache breakdown", cost.missing_breakdown),
            ("Amount exceeds supported range", cost.overflow),
        ] {
            if count > 0 {
                let _ = std::fmt::Write::write_fmt(
                    &mut text,
                    format_args!("{label}: {count} requests\n"),
                );
            }
        }
    }
    if let rsi_session_protocol::ModelAvailability::Unavailable { reason } =
        &read.current_model.availability
    {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!(
                "\nSelected model unavailable: {reason}\n/model opens model and effort selection\n"
            ),
        );
    }
    text
}

pub(super) fn show_tree_metrics(state: &mut State, read: &rsi_session_protocol::TreeMetricsRead) {
    let totals = &read.totals;
    let mut text = format!(
        "Agent tree usage{}\n{} / {} sessions read{}\n{} requests · {} failed\n{} input tokens · {} output tokens\nUsage reported for {} of {} requests\n\nEach session has its own captured Fact watermark.\nMembership control cursor: {}\n",
        if read.complete && read.membership_complete {
            ""
        } else {
            " · partial"
        },
        read.members.iter().filter(|member| member.complete).count(),
        read.members.len(),
        if read.membership_complete {
            ""
        } else {
            " · first 256 members only"
        },
        totals.attempts,
        totals.failed_attempts,
        totals.tokens.input_tokens(),
        totals.tokens.output_tokens(),
        totals.reported_attempts,
        totals.attempts,
        read.membership_control_seq
    );
    for member in &read.members {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!(
                "{}: {} / {}{}\n",
                member.session_id,
                member.through_seq,
                member
                    .watermark
                    .map_or_else(|| "not sampled".into(), |value| value.to_string()),
                if member.complete { "" } else { " · loading" }
            ),
        );
    }
    if !totals.configured_cost.totals.is_empty() {
        text.push_str(
            if totals.configured_cost.is_complete() && read.complete && read.membership_complete {
                "\nConfigured cost\n"
            } else {
                "\nConfigured cost · partial\n"
            },
        );
        for total in &totals.configured_cost.totals {
            let _ = std::fmt::Write::write_fmt(&mut text, format_args!("{}\n", total.display()));
        }
    }
    state.open_detail(text);
    state.detail_next = Some(Action::TreeMetrics(read.complete));
    state.notice(if read.complete {
        "→ Refresh tree usage"
    } else {
        "→ Continue loading tree usage"
    });
}

pub(super) fn show_evidence(state: &mut State, page: rsi_session_protocol::EvidencePage) {
    use rsi_session_protocol::{EvidencePageContent, EvidenceRead};
    match page.content {
        EvidencePageContent::Unavailable { reason } => state.open_detail(
            match reason {
                rsi_agent_session_protocol::EvidenceUnavailable::Budget => {
                    "Request evidence is unavailable because its byte budget was exhausted."
                }
                rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured => {
                    "This request did not capture inspection evidence."
                }
            }
            .into(),
        ),
        EvidencePageContent::Available {
            start,
            text,
            more,
            total_bytes,
            ..
        } => {
            let next = start + u32::try_from(text.len()).expect("validated evidence page fits u32");
            // The exact source page stays separate from paging labels and notices.
            state.open_detail(text);
            let action = |offset| {
                Action::Evidence(EvidenceRead {
                    intent_seq: page.intent_seq,
                    section: page.section,
                    offset,
                    maximum_bytes: u32::try_from(transcript::WINDOW).expect("source page fits u32"),
                })
            };
            state.detail_previous = (start > 0).then(|| {
                action(start.saturating_sub(
                    u32::try_from(transcript::WINDOW).expect("source page fits u32"),
                ))
            });
            state.detail_next = more.then(|| action(next));
            state.notice(format!(
                "Request {} · {:?} · bytes {start}..{next} / {total_bytes} · ←/→ pages",
                page.intent_seq, page.section
            ));
        }
    }
}
