//! Captured fork preparation; the source and child remain unpublished by callbacks.
use super::*;
use rsi_agent_composition_protocol::{ContributionKind, DomainBaseline, ForkTransformContext};
use rsi_agent_store_protocol::StoreForkBoundary;

impl AgentKernel {
    pub(super) async fn prepare_independent_fork(
        &self,
        request: rsi_agent_turn_protocol::PrepareFork,
    ) -> TurnResult<PreparedFreshSession> {
        let lease = self
            .inner
            .store
            .prepare_session(request.source.session_id())
            .await
            .map_err(turn_store_error)?;
        let source =
            observation::read_validated_header_bounded(&self.inner, request.source.session_id())
                .await
                .map_err(turn_store_error)?;
        if source != request.source {
            return Err(TurnError::Invalid("fork source Header changed".into()));
        }
        let point = request.point;
        let boundary = self
            .inner
            .store
            .resolve_fork_boundary(source.session_id(), &point, request.turns.clone())
            .await
            .map_err(turn_store_error)?;
        let history = rsi_agent_session_protocol::HistoryOrigin {
            parent_session_id: source.session_id().clone(),
            parent_header_fingerprint: source
                .fingerprint()
                .map_err(|error| TurnError::Invalid(error.to_string()))?,
            point,
            resolved_after_seq: boundary.resolved_after_seq,
            resolved_terminal_seq: boundary.resolved_terminal_seq,
            terminal_prefix_sha256: boundary.terminal_prefix_sha256.clone(),
            resolved_terminal_control_seq: boundary.resolved_terminal_control_seq,
            terminal_control_prefix_sha256: boundary.terminal_control_prefix_sha256.clone(),
            requested_turns: request.turns.clone(),
            effective_turns: boundary.effective_turns,
        };
        let header = source
            .forked_independent(request.session_id, request.created_at_ms, history)
            .and_then(|header| header.with_agent_preset_id(request.agent_preset_id))
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let composition = self
            .inner
            .composition
            .pin_session(&header, None)
            .await
            .map_err(turn_composition_error)?;
        let mut baseline = self
            .fork_baseline(
                &source,
                &composition,
                &request.turns,
                &boundary,
                request.cancellation.clone(),
            )
            .await?;
        if request.cancellation.is_cancelled() || self.inner.stop_worker.is_cancelled() {
            return Err(TurnError::ExecutionUnavailable);
        }
        baseline.retain(Arc::new(lease));
        PreparedFreshSession::new(header, composition)
            .and_then(|prepared| prepared.with_baseline(baseline))
            .map_err(turn_composition_error)
    }
    pub(super) async fn fork_baseline(
        &self,
        source: &SessionHeader,
        composition: &AgentCompositionPin,
        selection: &rsi_agent_session_protocol::ForkTurnSelection,
        boundary: &StoreForkBoundary,
        requested: CancellationToken,
    ) -> TurnResult<DomainBaseline> {
        if matches!(
            selection,
            rsi_agent_session_protocol::ForkTurnSelection::All
        ) && observation::fork_sources_from_header(&self.inner, source)
            .await?
            .len()
            >= rsi_agent_session_protocol::MAXIMUM_FORK_HISTORY_DEPTH
        {
            return Err(TurnError::Capacity);
        }
        let mut baseline = DomainBaseline::new(composition.domains().clone())
            .map_err(|error| turn_composition_error(error.into()))?;
        if boundary.resolved_terminal_control_seq == 0
            && (matches!(
                selection,
                rsi_agent_session_protocol::ForkTurnSelection::None
            ) || source.history_origin().is_none())
        {
            return Ok(baseline);
        }
        // A child starts with its captured domain baseline at control 1. A
        // further fork before its first completed Turn must transform that
        // baseline, never the live state of the currently invoking Turn.
        let control_seq = boundary.resolved_terminal_control_seq.max(1);
        let inherited = observation::read_domain_states_bounded(
            &self.inner,
            source.session_id(),
            Some(control_seq),
        )
        .await
        .map_err(turn_store_error)?;
        let cancellation = self.inner.stop_worker.child_token();
        let _close = cancellation.clone().drop_guard();
        let context = ForkTransformContext {
            source: Arc::new(source.clone()),
            selection: selection.clone(),
            after_seq: boundary.resolved_after_seq,
            through_seq: boundary.resolved_terminal_seq,
            through_control_seq: control_seq,
            domains: inherited
                .states
                .into_iter()
                .map(|state| state.snapshot)
                .collect(),
            facts: captured_history::capture(
                self,
                source.session_id(),
                boundary.resolved_terminal_seq,
                cancellation.clone(),
            ),
        };
        let transform = async {
            let mut proposals = Vec::new();
            for entry in composition.contributions().entries() {
                let ContributionKind::ForkTransform(callback) = entry.kind() else {
                    continue;
                };
                let result = callback
                    .transform(&context, cancellation.clone())
                    .await
                    .map_err(|error| TurnError::Invalid(error.to_string()))?;
                if result.len()
                    > rsi_agent_session_protocol::MAXIMUM_SESSION_DOMAINS
                        .saturating_sub(proposals.len())
                {
                    return Err(TurnError::Capacity);
                }
                proposals.extend(result);
            }
            baseline
                .inherit_transformed(&context.domains, &proposals)
                .map_err(|error| turn_composition_error(error.into()))?;
            Ok(baseline)
        };
        tokio::select! {
            () = cancellation.cancelled() => Err(TurnError::ExecutionUnavailable),
            () = requested.cancelled() => Err(TurnError::ExecutionUnavailable),
            result = tokio::time::timeout(Duration::from_secs(30), transform) => {
                result.map_err(|_| TurnError::Invalid("fork domain transformation deadline elapsed".into()))?
            }
        }
    }
}
