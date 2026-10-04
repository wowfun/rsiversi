//! Closed Workflow workbench contract. Targets remain ordinary Session authority.
use crate::{Result, SessionError};
use rsi_agent_session_protocol::ProgramRunId;
use rsi_agent_turn_protocol::{ProgramChildView, ProgramOverview};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Builtin preset requiring the opt-in Workflow runtime.
pub const WORKFLOW_PRESET_ID: &str = "workflow";
/// Program runtime factory identity in standard Host composition.
pub const PROGRAM_RUNTIME_PLUGIN_ID: &str = "rsi.agent.program.runtime";
/// Program Tool contribution identity in standard Agent composition.
pub const PROGRAM_TOOLS_PLUGIN_ID: &str = "rsi.agent.program.tools";

/// Maximum acceptance-history rows in one workbench page.
pub const MAXIMUM_WORKFLOW_HISTORY_ROWS: usize =
    rsi_agent_store_protocol::MAXIMUM_PROGRAM_HISTORY_ROWS;
/// Number of initial child receipts in one workbench detail page.
pub const WORKFLOW_CHILD_PAGE_SIZE: usize = rsi_agent_turn_protocol::PROGRAM_CHILD_PAGE_SIZE;
/// Maximum UTF-8 data fragment bytes in a workbench CAS page.
pub const MAXIMUM_WORKFLOW_FRAGMENT_BYTES: usize = 16 * 1024;

/// Coarse Runtime facts; no configuration or dependency identities cross this read.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum WorkflowRuntimeKind {
    Absent,
    Disabled,
    Pending,
    Available,
    Unavailable,
}
/// Host facts are instantaneous observations, separate from Session generation facts.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct WorkflowRuntimeStatus {
    pub kind: WorkflowRuntimeKind,
    pub available: bool,
    pub host_restart_required: bool,
    pub desired_revision: u64,
    pub observed_revision: u64,
}
/// Closed evidence about the exact Session's captured Program tools.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum WorkflowTools {
    Enabled,
    Disabled,
    NotResident,
    Unavailable,
}
/// Plan mode remains a separately observed domain state.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum WorkflowPlan {
    On,
    Off,
    Unavailable,
}
/// Session-correlated readiness never executes Node or hydrates a cold generation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct WorkflowReadiness {
    pub runtime: WorkflowRuntimeStatus,
    pub tools: WorkflowTools,
    pub plan: WorkflowPlan,
    pub local_execution: bool,
    pub generation: Option<String>,
}
/// Product-supplied dynamic coarse Runtime source; optional in the Session adapter.
pub trait WorkflowReadinessSource: fmt::Debug + Send + Sync + 'static {
    /// Checks the applied Runtime supply without rebuilding Profile observations.
    fn available(&self) -> bool;
    /// Reads current desired/applied observations without invoking a configured plugin.
    fn runtime(&self) -> WorkflowRuntimeStatus;
}
/// Local product source, independent of Node availability.
#[derive(Debug)]
pub struct WorkflowReadinessContract;
impl rsi_meta_contract::LocalContract for WorkflowReadinessContract {
    const KEY: &'static str = "rsi.workflow.readiness";
    type Service = dyn WorkflowReadinessSource;
}
/// Fixed-watermark history cursor within the handle's Session.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct WorkflowCursor {
    #[serde(with = "sequence")]
    pub seed_control_seq: u64,
    #[serde(with = "sequence::optional")]
    pub before_accepted_control_seq: Option<u64>,
}
/// One bounded history request; an absent cursor captures the server watermark.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct WorkflowList {
    pub cursor: Option<WorkflowCursor>,
    pub limit: usize,
}
impl WorkflowList {
    /// Validates finite page size and exclusive ordering position.
    pub fn validate(&self) -> Result<()> {
        if self.limit == 0
            || self.limit > MAXIMUM_WORKFLOW_HISTORY_ROWS
            || self.cursor.as_ref().is_some_and(|cursor| {
                cursor
                    .before_accepted_control_seq
                    .is_some_and(|b| b == 0 || b > cursor.seed_control_seq.saturating_add(1))
            })
        {
            return Err(SessionError::Invalid(
                "invalid workflow history cursor or limit".into(),
            ));
        }
        Ok(())
    }
}
/// Bounded descending summaries plus an exact continuation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct WorkflowPage {
    #[serde(with = "sequence")]
    pub seed_control_seq: u64,
    pub runs: Vec<ProgramOverview>,
    pub next: Option<WorkflowCursor>,
}
impl WorkflowPage {
    /// Validates correlation, ordering, watermark and continuation against its request.
    pub fn validate(
        &self,
        session: &rsi_agent_session_protocol::SessionId,
        request: &WorkflowList,
    ) -> Result<()> {
        request.validate()?;
        if self.runs.len() > request.limit {
            return Err(SessionError::Invalid("workflow page exceeds limit".into()));
        }
        if request
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.seed_control_seq != self.seed_control_seq)
        {
            return Err(SessionError::Invalid("workflow page changed seed".into()));
        }
        let mut before = request
            .cursor
            .as_ref()
            .and_then(|cursor| cursor.before_accepted_control_seq);
        for run in &self.runs {
            run.validate()
                .map_err(|e| SessionError::Invalid(e.to_string()))?;
            if &run.session_id != session
                || run.accepted_control_seq > self.seed_control_seq
                || before.is_some_and(|b| run.accepted_control_seq >= b)
            {
                return Err(SessionError::Invalid(
                    "workflow page has foreign or unordered runs".into(),
                ));
            }
            before = Some(run.accepted_control_seq);
        }
        if let Some(next) = &self.next
            && (self.runs.is_empty()
                || next.seed_control_seq != self.seed_control_seq
                || next.before_accepted_control_seq != before)
        {
            return Err(SessionError::Invalid(
                "workflow continuation does not advance".into(),
            ));
        }
        Ok(())
    }
}
/// Detail pages remain bound to one canonical run revision.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct WorkflowRead {
    pub run_id: ProgramRunId,
    #[serde(with = "sequence::optional")]
    pub expected_control_seq: Option<u64>,
    pub children_offset: usize,
    pub result_offset: Option<usize>,
    pub script_offset: Option<usize>,
}

mod sequence {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    fn parse<E: Error>(text: &str) -> std::result::Result<u64, E> {
        text.parse::<u64>()
            .ok()
            .filter(|value| value.to_string() == text)
            .ok_or_else(|| E::custom("invalid canonical workflow sequence"))
    }
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde with requires a borrowed field serializer"
    )]
    pub fn serialize<S: Serializer>(
        value: &u64,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<u64, D::Error> {
        parse(&String::deserialize(deserializer)?)
    }
    pub mod optional {
        use super::{Deserialize, Deserializer, Serializer, parse};
        use serde::Serialize;
        #[allow(
            clippy::ref_option,
            reason = "serde with requires a borrowed field serializer"
        )]
        pub fn serialize<S: Serializer>(
            value: &Option<u64>,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            value.map(|value| value.to_string()).serialize(serializer)
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> std::result::Result<Option<u64>, D::Error> {
            Option::<String>::deserialize(deserializer)?
                .map(|text| parse(&text))
                .transpose()
        }
    }
}
impl WorkflowRead {
    /// Child pages contain at most sixteen children; results use 16 KiB envelopes.
    pub fn validate(&self) -> Result<()> {
        if self
            .script_offset
            .is_some_and(|o| o > rsi_agent_session_protocol::MAXIMUM_PROGRAM_SCRIPT_BYTES)
            || self.children_offset > rsi_agent_session_protocol::MAXIMUM_PROGRAM_CHILDREN as usize
            || !self
                .children_offset
                .is_multiple_of(WORKFLOW_CHILD_PAGE_SIZE)
            || self
                .result_offset
                .is_some_and(|o| o > rsi_agent_session_protocol::MAXIMUM_PROGRAM_RESULT_BYTES)
            || (self.children_offset > 0
                || self.result_offset.is_some()
                || self.script_offset.is_some())
                && self.expected_control_seq.is_none()
        {
            return Err(SessionError::Invalid(
                "invalid workflow detail cursor".into(),
            ));
        }
        Ok(())
    }
}
/// UTF-8 fragment of the canonical result, authenticated by its durable binding.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct WorkflowResultFragment<F = String> {
    pub sha256: String,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub fragment: F,
}
/// A bounded read includes only the selected child and optional result page.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct WorkflowDetail {
    pub run: ProgramOverview,
    pub children: Vec<ProgramChildView>,
    pub children_offset: usize,
    pub next_children_offset: Option<usize>,
    pub result: Option<WorkflowResultFragment>,
    pub script: Option<WorkflowResultFragment>,
}

impl WorkflowDetail {
    /// Authenticates all detail fields against the exact Session, run and page request.
    pub fn validate(
        &self,
        session: &rsi_agent_session_protocol::SessionId,
        request: &WorkflowRead,
    ) -> Result<()> {
        request.validate()?;
        self.run
            .validate()
            .map_err(|e| SessionError::Invalid(e.to_string()))?;
        let invalid = || SessionError::Invalid("invalid workflow detail identity or page".into());
        if &self.run.session_id != session
            || self.run.run_id != request.run_id
            || request
                .expected_control_seq
                .is_some_and(|v| v != self.run.control_seq)
            || self.children_offset != request.children_offset
            || self.children.len() > WORKFLOW_CHILD_PAGE_SIZE
            || self.children_offset > self.run.children
            || self.children.len()
                != (self.run.children - self.children_offset).min(WORKFLOW_CHILD_PAGE_SIZE)
        {
            return Err(invalid());
        }
        let end = self.children_offset + self.children.len();
        if self.next_children_offset != (end < self.run.children).then_some(end) {
            return Err(invalid());
        }
        for (index, child) in self.children.iter().enumerate() {
            if child.ordinal as usize != self.children_offset + index + 1 {
                return Err(invalid());
            }
            if let Some(receipt) = &child.receipt {
                if receipt.ordinal != child.ordinal || receipt.child_session_id != child.session_id
                {
                    return Err(invalid());
                }
                receipt.validate().map_err(|_| invalid())?;
            }
        }
        fragment(
            self.result.as_ref(),
            request.result_offset,
            self.run.result_ref.as_ref(),
        )?;
        fragment(
            self.script.as_ref(),
            request.script_offset,
            Some(&self.run.script_ref),
        )?;
        Ok(())
    }
}

fn fragment(
    value: Option<&WorkflowResultFragment>,
    requested: Option<usize>,
    binding: Option<&rsi_agent_session_protocol::ProgramBlob>,
) -> Result<()> {
    let invalid = || SessionError::Invalid("invalid workflow CAS page".into());
    match (value, requested) {
        (None, None) => Ok(()),
        (Some(value), Some(offset)) => {
            let binding = binding.ok_or_else(invalid)?;
            let bytes = usize::try_from(binding.bytes).map_err(|_| invalid())?;
            if value.sha256 != binding.sha256
                || value.offset != offset
                || value.fragment.len() > MAXIMUM_WORKFLOW_FRAGMENT_BYTES
                || offset.checked_add(value.fragment.len()).is_none_or(|end| {
                    end > bytes || value.next_offset != (end < bytes).then_some(end)
                })
                || value.fragment.is_empty() && offset < bytes
            {
                return Err(invalid());
            }
            Ok(())
        }
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn overview() -> ProgramOverview {
        ProgramOverview {
            session_id: rsi_agent_session_protocol::SessionId::new("s").unwrap(),
            run_id: ProgramRunId::new("program-test").unwrap(),
            accepted_control_seq: 3,
            control_seq: 5,
            started: true,
            detached: true,
            cancelling: false,
            orphaned: false,
            children: 0,
            settled_children: 0,
            phase: None,
            progress: None,
            outcome: Some(rsi_agent_session_protocol::ProgramOutcome::Completed),
            script_ref: rsi_agent_session_protocol::ProgramBlob {
                sha256: "a".repeat(64),
                bytes: 3,
            },
            result_ref: Some(rsi_agent_session_protocol::ProgramBlob {
                sha256: "b".repeat(64),
                bytes: 3,
            }),
            retention: None,
        }
    }
    #[test]
    fn workflow_read_rejects_unaligned_children_and_invalid_overview_text() {
        let mut request = WorkflowRead {
            run_id: overview().run_id,
            expected_control_seq: Some(5),
            children_offset: 1,
            script_offset: None,
            result_offset: None,
        };
        assert!(request.validate().is_err());
        request.children_offset = WORKFLOW_CHILD_PAGE_SIZE;
        assert!(request.validate().is_ok());
        for phase in ["", "phase\u{7f}"] {
            let mut row = overview();
            row.phase = Some(phase.into());
            assert!(row.validate().is_err());
        }
        let mut row = overview();
        row.progress = Some("progress\u{7f}".into());
        assert!(
            row.validate().is_ok(),
            "canonical Program progress admits DEL in its JSON text"
        );
        let mut row = overview();
        row.started = false;
        assert!(row.validate().is_err());
    }

    #[test]
    fn workflow_action_cursors_preserve_u64_through_json_values() {
        for sequence in [0, (1u64 << 53) + 1, u64::MAX] {
            let cursor = WorkflowCursor {
                seed_control_seq: sequence,
                before_accepted_control_seq: Some(sequence),
            };
            let json = serde_json::to_value(&cursor).unwrap();
            assert_eq!(json["seed_control_seq"], sequence.to_string());
            let decoded: WorkflowCursor = serde_json::from_value(json).unwrap();
            assert_eq!(decoded.seed_control_seq, sequence);
            assert_eq!(decoded.before_accepted_control_seq, Some(sequence));
            let request = WorkflowRead {
                run_id: overview().run_id,
                expected_control_seq: Some(sequence),
                children_offset: 0,
                result_offset: None,
                script_offset: None,
            };
            let json = serde_json::to_value(&request).unwrap();
            assert_eq!(json["expected_control_seq"], sequence.to_string());
            assert_eq!(
                serde_json::from_value::<WorkflowRead>(json)
                    .unwrap()
                    .expected_control_seq,
                Some(sequence)
            );
        }
        for value in [
            serde_json::json!(1),
            serde_json::json!("01"),
            serde_json::json!("+1"),
            serde_json::json!("18446744073709551616"),
        ] {
            assert!(
                serde_json::from_value::<WorkflowCursor>(serde_json::json!({
                    "seed_control_seq":value,"before_accepted_control_seq":null
                }))
                .is_err()
            );
            assert!(
                serde_json::from_value::<WorkflowRead>(serde_json::json!({
                    "run_id":overview().run_id,"expected_control_seq":value,
                    "children_offset":0,"result_offset":null,"script_offset":null
                }))
                .is_err()
            );
        }
    }
    #[test]
    fn workflow_history_rejects_foreign_rows_and_nonadvancing_continuation() {
        let row = overview();
        let session = row.session_id.clone();
        let request = WorkflowList {
            cursor: Some(WorkflowCursor {
                seed_control_seq: 5,
                before_accepted_control_seq: None,
            }),
            limit: 1,
        };
        let mut page = WorkflowPage {
            seed_control_seq: 5,
            runs: vec![row],
            next: Some(WorkflowCursor {
                seed_control_seq: 5,
                before_accepted_control_seq: Some(3),
            }),
        };
        page.validate(&session, &request).unwrap();
        page.next.as_mut().unwrap().before_accepted_control_seq = Some(4);
        assert!(page.validate(&session, &request).is_err());
        page.next = None;
        page.runs[0].session_id = rsi_agent_session_protocol::SessionId::new("foreign").unwrap();
        assert!(page.validate(&session, &request).is_err());
    }
    #[test]
    fn workflow_history_captures_and_preserves_exact_seed() {
        let session = overview().session_id;
        let mut request = WorkflowList {
            cursor: None,
            limit: 1,
        };
        let mut page = WorkflowPage {
            seed_control_seq: (1u64 << 53) + 1,
            runs: vec![overview()],
            next: None,
        };
        page.validate(&session, &request).unwrap();
        let json = serde_json::to_value(&page).unwrap();
        assert_eq!(json["seed_control_seq"], page.seed_control_seq.to_string());
        let decoded: WorkflowPage = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.seed_control_seq, page.seed_control_seq);
        request.cursor = Some(WorkflowCursor {
            seed_control_seq: page.seed_control_seq,
            before_accepted_control_seq: None,
        });
        page.validate(&session, &request).unwrap();
        page.seed_control_seq -= 1;
        assert!(page.validate(&session, &request).is_err());
        request.cursor = None;
        page.seed_control_seq = 2;
        assert!(
            page.validate(&session, &request).is_err(),
            "unseeded replies still bound rows to the returned watermark"
        );
    }
    #[test]
    fn workflow_fragments_reject_stale_revision_digest_and_nonadvancing_pages() {
        let row = overview();
        let session = row.session_id.clone();
        let request = WorkflowRead {
            run_id: row.run_id.clone(),
            expected_control_seq: Some(5),
            children_offset: 0,
            result_offset: Some(0),
            script_offset: None,
        };
        let mut detail = WorkflowDetail {
            run: row,
            children: vec![],
            children_offset: 0,
            next_children_offset: None,
            result: Some(WorkflowResultFragment {
                sha256: "b".repeat(64),
                offset: 0,
                next_offset: None,
                fragment: "界".into(),
            }),
            script: None,
        };
        detail.validate(&session, &request).unwrap();
        detail.run.control_seq = 6;
        assert!(detail.validate(&session, &request).is_err());
        detail.run.control_seq = 5;
        detail.result.as_mut().unwrap().sha256 = "a".repeat(64);
        assert!(detail.validate(&session, &request).is_err());
        detail.result.as_mut().unwrap().sha256 = "b".repeat(64);
        detail.result.as_mut().unwrap().fragment.clear();
        detail.result.as_mut().unwrap().next_offset = Some(0);
        assert!(detail.validate(&session, &request).is_err());
    }
}
