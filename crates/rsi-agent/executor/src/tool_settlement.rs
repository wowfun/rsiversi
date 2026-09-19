//! One pure prepublication stage shared by fresh and retained Tool results.
use super::*;
use rsi_agent_composition_protocol::{ContributionKind, ToolSettlement, ToolSettlementContext};

impl Driver {
    pub(super) async fn tool_settlement_proposals(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        intent: &SessionFact,
        result: &ToolResult,
    ) -> std::result::Result<ToolSettlement, DriveFailure> {
        let entries = composition.contributions().entries();
        let SessionFactBody::ToolIntent {
            turn_id,
            effect_id,
            name,
            ..
        } = intent.body()
        else {
            return Err(fatal("settlement requires a ToolIntent"));
        };
        if turn_id != claim.turn_id() {
            return Err(fatal("settlement intent belongs to another Turn"));
        }
        let mut settlement = ToolSettlement {
            conclusion: structured_conclusion(composition, name, result).map_err(fatal)?,
            ..ToolSettlement::default()
        };
        if !entries
            .iter()
            .any(|entry| matches!(entry.kind(), ContributionKind::ToolSettlement(_)))
        {
            return Ok(settlement);
        }
        let domains = tokio::time::timeout(
            Duration::from_secs(30),
            self.turns.tool_settlement_domains(claim, effect_id),
        )
        .await
        .map_err(|_| {
            failed(
                "tool.settlement_timeout",
                "domain capture exceeded its deadline",
            )
        })?
        .map_err(crate::execution_support::turn_failure)?;
        let context = ToolSettlementContext {
            header: claim.header(),
            intent,
            result,
            domains: &domains,
        };

        let mut identities = std::collections::BTreeSet::new();
        for entry in entries {
            let ContributionKind::ToolSettlement(callback) = entry.kind() else {
                continue;
            };
            let additions =
                std::panic::catch_unwind(AssertUnwindSafe(|| callback.settle(&context)))
                    .map_err(|_| {
                        failed(
                            "tool.settlement_panic",
                            format!("{}: callback panicked", entry.id()),
                        )
                    })?
                    .map_err(|error| {
                        failed(
                            "tool.settlement_failed",
                            format!("{}: {}", entry.id(), bounded(&error.to_string())),
                        )
                    })?;
            if settlement
                .domains
                .len()
                .saturating_add(additions.domains.len())
                > rsi_agent_session_protocol::MAXIMUM_SESSION_DOMAINS
            {
                return Err(failed(
                    "tool.settlement_capacity",
                    "too many domain proposals",
                ));
            }
            if let Some(conclusion) = additions.conclusion
                && settlement.conclusion.replace(conclusion).is_some()
            {
                return Err(failed(
                    "tool.settlement_conflict",
                    "multiple conclusions for one result",
                ));
            }
            for proposal in additions.domains {
                if !identities.insert(proposal.snapshot().identity().id().to_owned()) {
                    return Err(failed(
                        "tool.settlement_conflict",
                        "multiple proposals target one domain",
                    ));
                }
                settlement.domains.push(proposal);
            }
        }
        Ok(settlement)
    }
}

fn structured_conclusion(
    composition: &AgentCompositionPin,
    name: &str,
    result: &ToolResult,
) -> rsi_agent_session_protocol::Result<Option<rsi_agent_session_protocol::ToolConclusion>> {
    if name == rsi_agent_session_protocol::REPORT_RESULT_TOOL
        && !result.is_error
        && let Some(contract) = composition.output_contract()
    {
        Ok(Some(rsi_agent_session_protocol::ToolConclusion {
            structured: Some(contract.summarize(&result.value)?),
        }))
    } else {
        Ok(None)
    }
}
