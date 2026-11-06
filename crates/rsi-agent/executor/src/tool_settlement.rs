//! One pure prepublication stage shared by fresh and retained Tool results.
use super::*;
use rsi_agent_composition_protocol::{ContributionKind, ToolSettlement, ToolSettlementContext};

impl Driver {
    /// Keep an admitted body's result through cancellation without waiting
    /// indefinitely for a non-cooperative implementation.
    pub(super) async fn wait_tool_settlement<T>(
        &self,
        mut future: std::pin::Pin<&mut impl std::future::Future<Output = T>>,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<T, DriveFailure> {
        tokio::select! {
            biased;
            result = &mut future => return Ok(result),
            () = stop.cancelled() => return Err(DriveFailure::Stopped),
            () = cancellation.cancelled() => {},
        }
        tokio::select! {
            biased;
            result = &mut future => Ok(result),
            () = stop.cancelled() => Err(DriveFailure::Stopped),
            () = tokio::time::sleep(self.config.retained_tool_wait()) =>
                Err(DriveFailure::Turn(TurnOutcome::Cancelled)),
        }
    }

    #[allow(clippy::too_many_lines)] // One bounded pure stage aggregates domains, conclusions and retained publication observers.
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
            settlement.require_uncancelled_turn |= additions.require_uncancelled_turn;
            if settlement
                .publications
                .len()
                .saturating_add(additions.publications.len())
                > rsi_agent_composition_protocol::MAXIMUM_AGENT_CONTRIBUTIONS
            {
                return Err(failed(
                    "tool.settlement_capacity",
                    "too many publication observers",
                ));
            }
            settlement.publications.extend(additions.publications);
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

pub(super) async fn notify_publications(
    observers: &mut [rsi_agent_composition_protocol::Publication],
    receipt: Option<&rsi_agent_composition_protocol::PublicationReceipt<'_>>,
) -> std::result::Result<(), DriveFailure> {
    let notify = async {
        for observer in observers {
            match receipt {
                Some(receipt) => observer.committed(receipt).await?,
                None => observer.rejected().await?,
            }
        }
        Ok::<_, rsi_agent_composition_protocol::ContributionError>(())
    };
    tokio::time::timeout(
        Duration::from_secs(5),
        AssertUnwindSafe(notify).catch_unwind(),
    )
    .await
    .map_err(|_| failed("tool.publication_timeout", "notification deadline exceeded"))?
    .map_err(|_| failed("tool.publication_panic", "notification panicked"))?
    .map_err(|error| failed("tool.publication_failed", bounded(&error.to_string())))
}
