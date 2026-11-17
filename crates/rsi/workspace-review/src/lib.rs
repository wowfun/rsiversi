//! Source-authorized execution interval review.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
mod git;
mod owner;
mod plugin;
mod scratch_root;
pub use git::SOURCE_GIT_ENVIRONMENT;
pub use owner::WorkspaceReview;
pub use plugin::{WorkspaceReviewApiFactory, WorkspaceReviewContract, WorkspaceReviewFactory};

#[cfg(test)]
mod tests;

const STORAGE_VERSION: u32 = 2;

#[cfg(test)]
rsi_persistence_testkit::payload_contract!(
    persistence_summaries,
    rsi_workspace_review_api::Summary,
    STORAGE_VERSION,
    format!(
        "tests/fixtures/persistence/summaries-v{}.json",
        STORAGE_VERSION
    ),
    {
        let samples: Vec<rsi_workspace_review_api::Summary> =
            vec![rsi_workspace_review_api::Summary {
                id: "1".repeat(32),
                epoch: "2".repeat(32),
                scope: rsi_workspace_review_api::Scope {
                    workspace: rsi_workspace_protocol::WorkspaceKey::parse("3".repeat(64)).unwrap(),
                    conversation: rsi_workspace_review_api::ConversationIdentity::Native(
                        rsi_agent_session_protocol::SessionId::new("fixture").unwrap(),
                    ),
                },
                turn: Some(rsi_agent_session_protocol::TurnId::new("turn").unwrap()),
                execution: "1".into(),
                accepted_seq: "2".into(),
                live_seq: "9".into(),
                started_ms: "1000".into(),
                finished_ms: Some("2000".into()),
                phase: rsi_workspace_review_api::Phase::Partial,
                changed_files: 2,
                added_lines: 9,
                removed_lines: 3,
                omissions: vec![rsi_workspace_review_api::Omission {
                    kind: rsi_workspace_review_api::OmissionKind::Binary,
                    count: 1,
                }],
            }];
        for sample in &samples {
            sample.validate().unwrap();
        }
        samples
    }
);
