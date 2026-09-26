use super::*;
use rsi_agent_session_protocol::ToolOrigin;
use rsi_tools_protocol::ToolProgramRole;

pub(super) fn apply_recovered_fact(
    turns: &mut BTreeMap<TurnId, TurnControl>,
    order: &mut Vec<TurnId>,
    budget: &TurnBudget,
    header: &SessionHeader,
    fact: &SessionFact,
) -> Result<()> {
    match fact.body() {
        SessionFactBody::TurnAccepted { turn_id, .. }
        | SessionFactBody::MessageTurnAccepted { turn_id, .. }
        | SessionFactBody::ImageRequested { turn_id, .. } => {
            if turns.len() >= MAXIMUM_LIVE_TURNS {
                return Err(KernelError::Invariant(
                    "durable session exceeds the live turn bound".into(),
                ));
            }
            let mut control = TurnControl::new(fact.timestamp_ms(), fact.seq());
            if let SessionFactBody::MessageTurnAccepted {
                activation_id,
                message_ids,
                ..
            } = fact.body()
            {
                control.activation_id = Some(activation_id.clone());
                control.initial_messages = message_ids.iter().cloned().collect();
            }
            if turns.insert(turn_id.clone(), control).is_some() {
                return Err(KernelError::Invariant(
                    "durable turn was accepted more than once".into(),
                ));
            }
            order.push(turn_id.clone());
        }
        SessionFactBody::CancelRequested { turn_id, .. } => {
            let turn = turns
                .get_mut(turn_id)
                .ok_or_else(|| KernelError::Invariant("cancel references unknown turn".into()))?;
            if turn.terminal.is_some() || turn.cancel_requested {
                return Err(KernelError::Invariant(
                    "durable cancellation is duplicate or follows terminal".into(),
                ));
            }
            turn.cancel_requested = true;
            turn.cancellation.cancel();
        }
        SessionFactBody::TurnTerminal { turn_id, .. } => {
            let turn = turns
                .get_mut(turn_id)
                .ok_or_else(|| KernelError::Invariant("terminal references unknown turn".into()))?;
            apply_executor_body(turn, fact.body())
                .map_err(|error| KernelError::Invariant(error.to_string()))?;
            turns.remove(turn_id);
            order.retain(|candidate| candidate != turn_id);
        }
        body => {
            let turn = turns.get_mut(body.turn_id()).ok_or_else(|| {
                KernelError::Invariant("durable Fact references unknown turn".into())
            })?;
            validate_budget_marker(budget, fact)
                .map_err(|error| KernelError::Invariant(error.to_string()))?;
            super::execution::validate_tool_admission(header, turn, body)
                .map_err(|error| KernelError::Invariant(error.to_string()))?;
            super::structured::validate_conclusion(header, turn, body)
                .map_err(|error| KernelError::Invariant(error.to_string()))?;
            let mut usage = turn.budget_usage;
            record_budget_usage(&mut usage, fact).map_err(KernelError::Invariant)?;
            check_budget_usage(budget, usage)
                .map_err(|error| KernelError::Invariant(error.to_string()))?;
            apply_executor_body(turn, body)
                .map_err(|error| KernelError::Invariant(error.to_string()))?;
            if let SessionFactBody::ToolResult {
                conclusion: Some(conclusion),
                ..
            } = body
            {
                turn.conclusion = Some((fact.seq(), conclusion.clone()));
            }
            turn.budget_usage = usage;
        }
    }
    Ok(())
}

pub(super) fn apply_executor_body(
    turn: &mut TurnControl,
    body: &SessionFactBody,
) -> TurnResult<()> {
    if turn.terminal.is_some() {
        return Err(TurnError::Invalid("Fact follows a terminal turn".into()));
    }
    if turn.conclusion.is_some()
        && !matches!(
            body,
            SessionFactBody::StepEnded { .. } | SessionFactBody::TurnTerminal { .. }
        )
    {
        return Err(TurnError::Invalid(
            "business Fact follows an accepted Tool conclusion".into(),
        ));
    }
    if turn.budget_exhausted.is_some()
        && !matches!(
            body,
            SessionFactBody::StepEnded { .. } | SessionFactBody::TurnTerminal { .. }
        )
    {
        return Err(TurnError::Invalid(
            "only the current-Step closure and terminal may follow budget exhaustion".into(),
        ));
    }
    match body {
        SessionFactBody::ModelIntent { .. }
        | SessionFactBody::ModelStarted { .. }
        | SessionFactBody::ModelEvent { .. } => apply_model_body(turn, body)?,
        SessionFactBody::ImageIntent { .. }
        | SessionFactBody::ImageStarted { .. }
        | SessionFactBody::ImageOutput { .. } => apply_image_body(turn, body)?,
        SessionFactBody::ToolIntent { .. }
        | SessionFactBody::ToolRejected { .. }
        | SessionFactBody::ToolCallsSuperseded { .. }
        | SessionFactBody::ToolStarted { .. }
        | SessionFactBody::ToolResult { .. } => apply_tool_body(turn, body)?,
        SessionFactBody::StepStarted { .. }
        | SessionFactBody::InputMessageEntered { .. }
        | SessionFactBody::WorkspaceTouched { .. }
        | SessionFactBody::StepEnded { .. } => apply_step_body(turn, body)?,
        SessionFactBody::BudgetExhausted {
            dimension,
            consumed,
            limit,
            ..
        } => {
            turn.budget_exhausted = Some((*dimension, *consumed, *limit));
        }
        SessionFactBody::TurnTerminal { outcome, .. } => {
            if turn.current_step.is_some() {
                return Err(TurnError::Invalid(
                    "Turn terminal requires its current Step to end first".into(),
                ));
            }
            outcome
                .validate()
                .map_err(|error| TurnError::Invalid(error.to_string()))?;
            match (turn.budget_exhausted, outcome) {
                (
                    Some((dimension, consumed, limit)),
                    TurnOutcome::BudgetExceeded {
                        dimension: outcome_dimension,
                        consumed: outcome_consumed,
                        limit: outcome_limit,
                    },
                ) if dimension == *outcome_dimension
                    && consumed == *outcome_consumed
                    && limit == *outcome_limit => {}
                (Some(_), TurnOutcome::Cancelled) if turn.cancel_requested => {}
                (Some(_), _) => {
                    return Err(TurnError::Invalid(
                        "budget exhaustion and terminal outcome disagree".into(),
                    ));
                }
                (None, TurnOutcome::BudgetExceeded { .. }) => {
                    return Err(TurnError::Invalid(
                        "budget terminal lacks its preceding exhaustion Fact".into(),
                    ));
                }
                (None, _) => {}
            }
            turn.terminal = Some(outcome.clone());
            turn.effects.clear();
        }
        SessionFactBody::TurnAccepted { .. }
        | SessionFactBody::MessageTurnAccepted { .. }
        | SessionFactBody::ImageRequested { .. }
        | SessionFactBody::CancelRequested { .. } => {
            return Err(TurnError::Invalid(
                "executor cannot publish acceptance or cancellation Facts".into(),
            ));
        }
    }
    Ok(())
}

fn apply_step_body(turn: &mut TurnControl, body: &SessionFactBody) -> TurnResult<()> {
    match body {
        SessionFactBody::StepStarted { step_id, .. } => {
            if turn.current_step.replace(step_id.clone()).is_some() {
                return Err(TurnError::Invalid(
                    "Step start follows another open Step".into(),
                ));
            }
        }
        SessionFactBody::InputMessageEntered { step_id, .. }
        | SessionFactBody::WorkspaceTouched { step_id, .. } => {
            if turn.current_step.as_ref() != Some(step_id) {
                return Err(TurnError::Invalid(
                    "Step-scoped Fact does not match the open Step".into(),
                ));
            }
            if matches!(body, SessionFactBody::InputMessageEntered { .. }) {
                ensure_no_active_effect(turn)?;
            }
        }
        SessionFactBody::StepEnded { step_id, .. } => {
            if turn.current_step.as_ref() != Some(step_id) {
                return Err(TurnError::Invalid(
                    "Step end does not match the open Step".into(),
                ));
            }
            turn.current_step = None;
        }
        _ => unreachable!("caller selected a Step Fact"),
    }
    Ok(())
}

pub(super) fn apply_model_body(turn: &mut TurnControl, body: &SessionFactBody) -> TurnResult<()> {
    match body {
        SessionFactBody::ModelIntent {
            effect_id,
            purpose,
            snapshot,
            evidence,
            ..
        } => {
            ensure_no_active_effect(turn)?;
            let bytes = turn
                .evidence_inline_bytes
                .checked_add(evidence.inline_bytes())
                .ok_or(TurnError::EvidenceBudget)?;
            if bytes > rsi_agent_session_protocol::MAXIMUM_REQUEST_EVIDENCE_BYTES {
                return Err(TurnError::EvidenceBudget);
            }
            turn.evidence_inline_bytes = bytes;
            if !Arc::make_mut(&mut turn.seen_model_effects).insert(effect_id.clone()) {
                return Err(TurnError::Invalid(
                    "model effect identity was reused within a Turn".into(),
                ));
            }
            turn.tool_source = if matches!(
                purpose,
                rsi_agent_session_protocol::ModelPurpose::Conversation
            ) {
                Some(Arc::new(tool_origin::ToolSource::new(
                    effect_id.clone(),
                    snapshot,
                )?))
            } else {
                None
            };
            turn.effects.insert(
                effect_id.clone(),
                ActiveEffect::Model {
                    purpose: purpose.event_purpose(),
                    effect_id: effect_id.clone(),
                    started: false,
                },
            );
        }
        SessionFactBody::ModelStarted { effect_id, .. } => match turn.effects.get_mut(effect_id) {
            Some(ActiveEffect::Model {
                effect_id: current,
                started,
                ..
            }) if current == effect_id && !*started => *started = true,
            _ => return Err(TurnError::Invalid("model start has no exact intent".into())),
        },
        SessionFactBody::ModelEvent {
            effect_id,
            event,
            purpose,
            ..
        } => {
            match turn.effects.get(effect_id) {
                Some(ActiveEffect::Model {
                    effect_id: current,
                    started: true,
                    purpose: expected,
                }) if current == effect_id && expected == purpose => {}
                _ => return Err(TurnError::Invalid("model event has no exact start".into())),
            }
            if let Some(source) = &mut turn.tool_source {
                tool_origin::ToolSource::observe_shared(source, event)?;
            }
            if matches!(event, rsi_ai_protocol::LanguageEvent::Failed { .. }) {
                turn.tool_source = None;
            }
            if matches!(
                event,
                rsi_ai_protocol::LanguageEvent::Finished { .. }
                    | rsi_ai_protocol::LanguageEvent::Failed { .. }
            ) {
                turn.effects.remove(effect_id);
            }
        }
        _ => unreachable!("caller selected a Model Fact"),
    }
    Ok(())
}

pub(super) fn apply_image_body(turn: &mut TurnControl, body: &SessionFactBody) -> TurnResult<()> {
    match body {
        SessionFactBody::ImageIntent { effect_id, .. } => {
            ensure_no_active_effect(turn)?;
            turn.effects.insert(
                effect_id.clone(),
                ActiveEffect::Image {
                    effect_id: effect_id.clone(),
                    started: false,
                    next_index: 0,
                },
            );
        }
        SessionFactBody::ImageStarted { effect_id, .. } => match turn.effects.get_mut(effect_id) {
            Some(ActiveEffect::Image {
                effect_id: current,
                started,
                ..
            }) if current == effect_id && !*started => *started = true,
            _ => return Err(TurnError::Invalid("Image start has no exact intent".into())),
        },
        SessionFactBody::ImageOutput {
            effect_id, index, ..
        } => match turn.effects.get_mut(effect_id) {
            Some(ActiveEffect::Image {
                effect_id: current,
                started: true,
                next_index,
            }) if current == effect_id && *index == *next_index => {
                *next_index = next_index
                    .checked_add(1)
                    .ok_or_else(|| TurnError::Invalid("Image output index exhausted".into()))?;
            }
            _ => {
                return Err(TurnError::Invalid(
                    "Image output has no exact start or contiguous index".into(),
                ));
            }
        },
        _ => unreachable!("caller selected an Image Fact"),
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // Intent admission, rejection, supersession, start and settlement share one source-proof state machine.
pub(super) fn apply_tool_body(turn: &mut TurnControl, body: &SessionFactBody) -> TurnResult<()> {
    match body {
        SessionFactBody::ToolRejected {
            identity,
            name,
            arguments,
            origin,
            program_role,
            ..
        } => match origin {
            ToolOrigin::Model { effect_id } => {
                ensure_no_active_effect(turn)?;
                turn.tool_source
                    .as_mut()
                    .ok_or_else(|| {
                        TurnError::Invalid("ToolRejected has no Conversation source".into())
                    })
                    .map(Arc::make_mut)?
                    .reject(effect_id, identity.call_id(), name, arguments)?;
            }
            ToolOrigin::Program {
                parent_effect_id,
                ordinal,
            } => {
                admit_program_origin(turn, parent_effect_id, *ordinal, *program_role, false)?;
            }
        },
        SessionFactBody::ToolCallsSuperseded {
            source_model_effect_id,
            ..
        } => {
            ensure_no_active_effect(turn)?;
            turn.tool_source
                .as_mut()
                .ok_or_else(|| {
                    TurnError::Invalid("Tool supersession has no Conversation source".into())
                })
                .map(Arc::make_mut)?
                .supersede(source_model_effect_id)?;
        }
        SessionFactBody::ToolIntent {
            effect_id,
            origin,
            program_role,
            identity,
            name,
            arguments,
            parallel_safe,
            ..
        } => {
            let source_selection = match origin {
                ToolOrigin::Model { effect_id: source } => {
                    if !turn.effects.is_empty()
                        && (!parallel_safe
                            || turn.effects.values().any(|effect| {
                                !matches!(
                                    effect,
                                    ActiveEffect::Tool {
                                        parallel_safe: true,
                                        origin: ToolOrigin::Model { .. },
                                        ..
                                    }
                                )
                            }))
                    {
                        return Err(TurnError::Invalid(
                            "overlapping Tool intents require parallel-safe definitions".into(),
                        ));
                    }
                    turn.tool_source
                        .as_mut()
                        .ok_or_else(|| {
                            TurnError::Invalid("ToolIntent has no Conversation source".into())
                        })
                        .map(Arc::make_mut)?
                        .consume(source, identity.call_id(), name, arguments)?
                }
                ToolOrigin::Program {
                    parent_effect_id,
                    ordinal,
                } => admit_program_origin(
                    turn,
                    parent_effect_id,
                    *ordinal,
                    *program_role,
                    *parallel_safe,
                )?,
            };
            if turn
                .effects
                .insert(
                    effect_id.clone(),
                    ActiveEffect::Tool {
                        name: name.clone(),
                        source_selection: Arc::new(source_selection),
                        effect_id: effect_id.clone(),
                        identity: identity.clone(),
                        started: false,
                        parallel_safe: *parallel_safe,
                        origin: origin.clone(),
                        program_role: *program_role,
                        next_program_ordinal: 1,
                    },
                )
                .is_some()
            {
                return Err(TurnError::Invalid("Tool effect identity was reused".into()));
            }
        }
        SessionFactBody::ToolStarted {
            effect_id,
            identity,
            ..
        } => match turn.effects.get_mut(effect_id) {
            Some(ActiveEffect::Tool {
                effect_id: current,
                identity: current_identity,
                started,
                ..
            }) if current == effect_id && current_identity == identity && !*started => {
                *started = true;
            }
            _ => return Err(TurnError::Invalid("Tool start has no exact intent".into())),
        },
        SessionFactBody::ToolResult {
            effect_id,
            identity,
            ..
        } => match turn.effects.get(effect_id) {
            Some(ActiveEffect::Tool {
                effect_id: current,
                identity: current_identity,
                started: true,
                ..
            }) if current == effect_id && current_identity == identity => {
                if turn.effects.values().any(|effect| matches!(effect, ActiveEffect::Tool { origin: ToolOrigin::Program { parent_effect_id, .. }, .. } if parent_effect_id == effect_id)) {
                    return Err(TurnError::Invalid("coordinator result precedes nested Tool settlement".into()));
                }
                turn.effects.remove(effect_id);
            }
            _ => return Err(TurnError::Invalid("Tool result has no exact start".into())),
        },
        _ => unreachable!("caller selected a Tool Fact"),
    }
    Ok(())
}

fn admit_program_origin(
    turn: &mut TurnControl,
    parent: &EffectId,
    ordinal: u32,
    role: ToolProgramRole,
    parallel_safe: bool,
) -> TurnResult<rsi_agent_session_protocol::ModelSelection> {
    if role != ToolProgramRole::Callable
        || turn.effects.len() > rsi_agent_session_protocol::MAXIMUM_PROGRAM_OUTSTANDING_CALLS
    {
        return Err(TurnError::Invalid(
            "program Tool eligibility or active-call capacity violated".into(),
        ));
    }
    for (id, effect) in &turn.effects {
        if id != parent
            && (!parallel_safe
                || !matches!(effect, ActiveEffect::Tool { parallel_safe: true, origin: ToolOrigin::Program { parent_effect_id, .. }, .. } if parent_effect_id == parent))
        {
            return Err(TurnError::Invalid(
                "program Tool overlap violates its scheduling declaration".into(),
            ));
        }
    }
    let Some(ActiveEffect::Tool {
        started: true,
        program_role: ToolProgramRole::Coordinator,
        origin: ToolOrigin::Model { .. },
        next_program_ordinal,
        source_selection,
        ..
    }) = turn.effects.get_mut(parent)
    else {
        return Err(TurnError::Invalid(
            "program call lacks its exact started coordinator".into(),
        ));
    };
    if ordinal != *next_program_ordinal {
        return Err(TurnError::Invalid(
            "program call ordinal was reused or skipped".into(),
        ));
    }
    *next_program_ordinal = ordinal.checked_add(1).ok_or(TurnError::Capacity)?;
    Ok(source_selection.as_ref().clone())
}

pub(super) fn ensure_no_active_effect(turn: &TurnControl) -> TurnResult<()> {
    if !turn.effects.is_empty() {
        return Err(TurnError::Invalid("external effect already active".into()));
    }
    Ok(())
}

fn is_terminal_step_closure(facts: &[SessionFact], index: usize) -> bool {
    matches!(facts[index].body(), SessionFactBody::StepEnded { .. })
        && match &facts[index + 1..] {
            [terminal] => matches!(terminal.body(), SessionFactBody::TurnTerminal { .. }),
            [budget, terminal] => {
                matches!(budget.body(), SessionFactBody::BudgetExhausted { .. })
                    && matches!(terminal.body(), SessionFactBody::TurnTerminal { .. })
            }
            _ => false,
        }
}

pub(super) fn enforce_turn_budget(
    budget: &TurnBudget,
    turn: &TurnControl,
    facts: &[SessionFact],
    now_ms: u64,
) -> TurnResult<BudgetUsage> {
    let admits_work = facts.iter().enumerate().any(|(index, fact)| {
        !is_terminal_step_closure(facts, index)
            && !matches!(
                fact.body(),
                SessionFactBody::BudgetExhausted { .. } | SessionFactBody::TurnTerminal { .. }
            )
    });
    if admits_work {
        let elapsed = turn.elapsed.consumed(turn.accepted_at_ms, now_ms);
        if elapsed >= budget.maximum_elapsed_ms() {
            return Err(TurnError::BudgetExceeded {
                dimension: BudgetDimension::Elapsed,
                consumed: elapsed,
                limit: budget.maximum_elapsed_ms(),
            });
        }
    }
    for fact in facts {
        validate_budget_marker(budget, fact)?;
    }

    let mut usage = turn.budget_usage;
    for (index, fact) in facts.iter().enumerate() {
        if !is_terminal_step_closure(facts, index) {
            record_budget_usage(&mut usage, fact).map_err(TurnError::Invariant)?;
        }
    }
    check_publication_budget(budget, usage, facts)?;
    Ok(usage)
}

pub(super) fn validate_budget_marker(budget: &TurnBudget, fact: &SessionFact) -> TurnResult<()> {
    if let SessionFactBody::BudgetExhausted {
        dimension,
        consumed,
        limit,
        ..
    } = fact.body()
        && (*limit != budget_limit(budget, *dimension) || *consumed < *limit)
    {
        return Err(TurnError::Invalid(
            "budget exhaustion does not match the frozen turn budget".into(),
        ));
    }
    Ok(())
}

pub(super) fn add_generated_usage(
    usage: &mut BudgetUsage,
    records: u64,
    bytes: u64,
) -> TurnResult<()> {
    usage.generated_records = usage
        .generated_records
        .checked_add(records)
        .ok_or_else(|| TurnError::Invariant("generated record count overflow".into()))?;
    usage.generated_record_bytes = usage
        .generated_record_bytes
        .checked_add(bytes)
        .ok_or_else(|| TurnError::Invariant("generated record byte count overflow".into()))?;
    Ok(())
}

pub(super) fn enforce_domain_budget(
    budget: &TurnBudget,
    turn: &TurnControl,
    facts: &[SessionFact],
    control: &AgentControlRecord,
    now_ms: u64,
) -> TurnResult<BudgetUsage> {
    let elapsed = turn.elapsed.consumed(turn.accepted_at_ms, now_ms);
    if elapsed >= budget.maximum_elapsed_ms() {
        return Err(TurnError::BudgetExceeded {
            dimension: BudgetDimension::Elapsed,
            consumed: elapsed,
            limit: budget.maximum_elapsed_ms(),
        });
    }
    let mut usage = turn.budget_usage;
    for fact in facts {
        validate_budget_marker(budget, fact)?;
        record_budget_usage(&mut usage, fact).map_err(TurnError::Invariant)?;
    }
    add_generated_usage(&mut usage, 1, control.encoded_len() as u64)?;
    check_publication_budget(budget, usage, facts)?;
    Ok(usage)
}

fn check_publication_budget(
    budget: &TurnBudget,
    usage: BudgetUsage,
    facts: &[SessionFact],
) -> TurnResult<()> {
    check_budget_usage(budget, usage).map_err(|error| {
        if matches!(
            error,
            TurnError::BudgetExceeded {
                dimension: BudgetDimension::GeneratedRecordBytes,
                ..
            }
        ) && facts.iter().any(|fact| {
            matches!(
                fact.body(),
                SessionFactBody::ModelIntent {
                    evidence: rsi_agent_session_protocol::RequestEvidence::Available { .. },
                    ..
                }
            )
        }) {
            TurnError::EvidenceBudget
        } else {
            error
        }
    })
}

pub(super) const fn budget_limit(budget: &TurnBudget, dimension: BudgetDimension) -> u64 {
    match dimension {
        BudgetDimension::Elapsed => budget.maximum_elapsed_ms(),
        BudgetDimension::ProviderAttempts => budget.maximum_provider_attempts(),
        BudgetDimension::ToolCalls => budget.maximum_tool_calls(),
        BudgetDimension::GeneratedRecords => budget.maximum_generated_records(),
        BudgetDimension::GeneratedRecordBytes => budget.maximum_generated_record_bytes(),
    }
}

pub(super) fn check_budget_usage(budget: &TurnBudget, usage: BudgetUsage) -> TurnResult<()> {
    for (dimension, consumed, limit) in [
        (
            BudgetDimension::ProviderAttempts,
            usage.provider_attempts,
            budget.maximum_provider_attempts(),
        ),
        (
            BudgetDimension::ToolCalls,
            usage.tool_calls,
            budget.maximum_tool_calls(),
        ),
        (
            BudgetDimension::GeneratedRecords,
            usage.generated_records,
            budget.maximum_generated_records(),
        ),
        (
            BudgetDimension::GeneratedRecordBytes,
            usage.generated_record_bytes,
            budget.maximum_generated_record_bytes(),
        ),
    ] {
        if consumed > limit {
            return Err(TurnError::BudgetExceeded {
                dimension,
                consumed,
                limit,
            });
        }
    }
    Ok(())
}

pub(super) fn record_budget_usage(
    usage: &mut BudgetUsage,
    fact: &SessionFact,
) -> std::result::Result<(), String> {
    if matches!(
        fact.body(),
        SessionFactBody::TurnAccepted { .. }
            | SessionFactBody::MessageTurnAccepted { .. }
            | SessionFactBody::ImageRequested { .. }
            | SessionFactBody::CancelRequested { .. }
            | SessionFactBody::BudgetExhausted { .. }
            | SessionFactBody::TurnTerminal { .. }
    ) {
        return Ok(());
    }
    usage.generated_records = usage
        .generated_records
        .checked_add(1)
        .ok_or_else(|| "generated record count overflowed".to_owned())?;
    usage.generated_record_bytes = usage
        .generated_record_bytes
        .checked_add(
            u64::try_from(fact.encoded_len())
                .map_err(|_| "generated record byte length exceeds u64".to_owned())?,
        )
        .ok_or_else(|| "generated record bytes overflowed".to_owned())?;
    match fact.body() {
        SessionFactBody::ModelIntent { .. } | SessionFactBody::ImageIntent { .. } => {
            usage.provider_attempts = usage
                .provider_attempts
                .checked_add(1)
                .ok_or_else(|| "provider attempt count overflowed".to_owned())?;
        }
        SessionFactBody::ToolIntent { .. } | SessionFactBody::ToolRejected { .. } => {
            usage.tool_calls = usage
                .tool_calls
                .checked_add(1)
                .ok_or_else(|| "Tool-call count overflowed".to_owned())?;
        }
        SessionFactBody::TurnAccepted { .. }
        | SessionFactBody::MessageTurnAccepted { .. }
        | SessionFactBody::ImageRequested { .. }
        | SessionFactBody::CancelRequested { .. }
        | SessionFactBody::BudgetExhausted { .. }
        | SessionFactBody::ModelStarted { .. }
        | SessionFactBody::ImageStarted { .. }
        | SessionFactBody::ImageOutput { .. }
        | SessionFactBody::ModelEvent { .. }
        | SessionFactBody::ToolStarted { .. }
        | SessionFactBody::ToolResult { .. }
        | SessionFactBody::ToolCallsSuperseded { .. }
        | SessionFactBody::StepStarted { .. }
        | SessionFactBody::InputMessageEntered { .. }
        | SessionFactBody::StepEnded { .. }
        | SessionFactBody::WorkspaceTouched { .. }
        | SessionFactBody::TurnTerminal { .. } => {}
    }
    Ok(())
}

pub(super) fn clone_turn_control(turn: &TurnControl) -> TurnControl {
    TurnControl {
        initial_messages: turn.initial_messages.clone(),
        claim_composition: turn.claim_composition.clone(),
        program_roles: turn.program_roles.clone(),
        conclusion: turn.conclusion.clone(),
        tool_source: turn.tool_source.clone(),
        seen_model_effects: turn.seen_model_effects.clone(),
        evidence_inline_bytes: turn.evidence_inline_bytes,
        elapsed: Arc::clone(&turn.elapsed),
        accepted_at_ms: turn.accepted_at_ms,
        accepted_seq: turn.accepted_seq,
        activation_id: turn.activation_id.clone(),
        current_step: turn.current_step.clone(),
        terminal: turn.terminal.clone(),
        terminal_seq: turn.terminal_seq,
        cancel_requested: turn.cancel_requested,
        cancellation: turn.cancellation.clone(),
        claim: turn.claim.clone(),
        prepared_lane: turn.prepared_lane.clone(),
        effects: turn.effects.clone(),
        budget_usage: turn.budget_usage,
        budget_exhausted: turn.budget_exhausted,
    }
}

pub(super) fn canonicalize_terminal(body: SessionFactBody, cancelled: bool) -> SessionFactBody {
    match body {
        SessionFactBody::TurnTerminal { turn_id, .. } if cancelled => {
            SessionFactBody::TurnTerminal {
                turn_id,
                outcome: TurnOutcome::Cancelled,
                result: None,
            }
        }
        SessionFactBody::TurnTerminal {
            turn_id,
            outcome: TurnOutcome::Cancelled,
            ..
        } => SessionFactBody::TurnTerminal {
            turn_id,
            outcome: TurnOutcome::Failed {
                code: "executor.unrequested_cancellation".into(),
                message: "executor proposed cancellation without a durable request".into(),
            },
            result: None,
        },
        other => other,
    }
}

pub(super) fn next_fact(
    inner: &KernelInner,
    session: &SessionRuntime,
    body: SessionFactBody,
) -> Result<Arc<SessionFact>> {
    let seq = session
        .live_seq()?
        .checked_add(1)
        .ok_or_else(|| KernelError::Invariant("Fact sequence exhausted".into()))?;
    SessionFact::new(seq, inner.clock.now_ms().max(1), body)
        .map(Arc::new)
        .map_err(KernelError::Session)
}

pub(super) fn push_pending(
    inner: &KernelInner,
    session: &mut SessionRuntime,
    fact: Arc<SessionFact>,
) -> Result<()> {
    let bytes = fact.encoded_len();
    let projected = session
        .pending_bytes
        .checked_add(bytes)
        .ok_or_else(|| KernelError::Invariant("pending Fact bytes overflowed".into()))?;
    if projected > MAXIMUM_PENDING_FACT_BYTES {
        return Err(KernelError::Capacity(
            "speculative Fact buffer capacity is exhausted".into(),
        ));
    }
    reserve_atomic_capacity(
        &inner.process_pending_bytes,
        bytes,
        inner.limits.maximum_process_pending_fact_bytes,
    )?;
    session.pending_bytes = projected;
    session.pending.push_back(fact);
    Ok(())
}

pub(super) fn publish_live_watermarks(session: &SessionRuntime) {
    let live_seq = session
        .live_seq()
        .expect("validated pending suffix has a representable live sequence");
    session.updates.send_replace(LiveWatermarks {
        live_seq,
        durable_seq: session.durable_seq,
    });
}

pub(super) fn reserve_atomic_capacity(
    counter: &AtomicUsize,
    amount: usize,
    limit: usize,
) -> Result<()> {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let projected = current
            .checked_add(amount)
            .ok_or_else(|| KernelError::Capacity("process capacity counter overflowed".into()))?;
        if projected > limit {
            return Err(KernelError::Capacity(
                "process speculative Fact capacity is exhausted".into(),
            ));
        }
        match counter.compare_exchange_weak(current, projected, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return Ok(()),
            Err(observed) => current = observed,
        }
    }
}

pub(super) fn enqueue(state: &mut KernelState, session_id: SessionId, turn_id: TurnId) {
    let candidate = (session_id, turn_id);
    if state.queued.insert(candidate.clone()) {
        state.claim_queue.push_back(candidate);
    }
}

pub(super) fn deregister_executor(
    inner: &Weak<KernelInner>,
    executor_id: &str,
    registration_id: u64,
) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let mut state = lock_state(&inner);
    if state.executors.get(executor_id) != Some(&registration_id) {
        return;
    }
    state.executors.remove(executor_id);
    let mut released = Vec::new();
    for (session_id, session) in &mut state.sessions {
        for (turn_id, turn) in &mut session.turns {
            if turn.claim.as_ref().is_some_and(|owner| {
                owner.executor == executor_id && owner.registration == registration_id
            }) && retire_claim(turn)
            {
                released.push((session_id.clone(), turn_id.clone()));
            }
        }
    }
    for (session_id, turn_id) in released {
        enqueue(&mut state, session_id, turn_id);
    }
    drop(state);
    inner.claim_changed.notify_waiters();
}

/// Closes one current claim; returns whether nonterminal work can be requeued now.
pub(super) fn retire_claim(turn: &mut TurnControl) -> bool {
    let gate = &turn.claim.as_ref().expect("current claim owner").mutations;
    if !gate.retire() {
        return false;
    }
    turn.claim = None;
    turn.terminal.is_none()
}

pub(super) fn lock_state(inner: &KernelInner) -> std::sync::MutexGuard<'_, KernelState> {
    inner
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(super) fn turn_store_error(error: StoreError) -> TurnError {
    match error {
        StoreError::Invalid(message) => TurnError::Invalid(bounded_diagnostic(&message)),
        error @ StoreError::SchemaMismatch { .. } => {
            TurnError::Invariant(bounded_diagnostic(&error.to_string()))
        }
        StoreError::NotFound(session) => TurnError::SessionNotFound(session),
        StoreError::TurnNotFound { session, turn } => TurnError::TurnNotFound { session, turn },
        StoreError::DomainRevisionConflict {
            domain,
            expected,
            actual,
        } => TurnError::DomainRevisionConflict {
            domain,
            expected: rsi_agent_session_protocol::DomainRevision::new(expected),
            actual: rsi_agent_session_protocol::DomainRevision::new(actual),
        },
        StoreError::DomainRequestConflict { request_id } => {
            TurnError::DomainRequestConflict { request_id }
        }
        other => TurnError::Store(bounded_diagnostic(&other.to_string())),
    }
}

pub(super) fn turn_composition_error(error: AgentCompositionError) -> TurnError {
    match error {
        AgentCompositionError::InvalidInput(message) => {
            TurnError::Invalid(bounded_diagnostic(&message))
        }
        AgentCompositionError::Unavailable { .. }
        | AgentCompositionError::OutputCatalogMismatch
        | AgentCompositionError::MissingOutputCatalog
        | AgentCompositionError::UnsupportedSeedCodec { .. }
        | AgentCompositionError::Domain(_)
        | AgentCompositionError::DefaultUnavailable { .. }
        | AgentCompositionError::Capacity => {
            TurnError::Composition(bounded_diagnostic(&error.to_string()))
        }
        AgentCompositionError::ShuttingDown => TurnError::ShuttingDown,
    }
}

pub(super) fn bounded_diagnostic(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        let character = if matches!(character, '\0' | '\u{7f}') {
            '\u{fffd}'
        } else {
            character
        };
        if output.len().saturating_add(character.len_utf8()) > MAXIMUM_AGENT_DIAGNOSTIC_BYTES {
            break;
        }
        output.push(character);
    }
    output
}

pub(super) fn turn_not_found(session_id: &SessionId, turn_id: &TurnId) -> TurnError {
    TurnError::TurnNotFound {
        session: session_id.to_string(),
        turn: turn_id.to_string(),
    }
}

pub(super) fn submission_conflict(session_id: &SessionId, turn_id: &TurnId) -> TurnError {
    TurnError::SubmissionConflict {
        session: session_id.to_string(),
        turn: turn_id.to_string(),
    }
}

pub(super) fn turn_kernel_error(error: KernelError) -> TurnError {
    match error {
        KernelError::Flush(message) | KernelError::Shutdown(message) => TurnError::Flush(message),
        KernelError::Session(error) => TurnError::Invalid(error.to_string()),
        KernelError::Composition(message) => TurnError::Composition(message),
        KernelError::Capacity(_) => TurnError::Capacity,
        KernelError::Invariant(message) => TurnError::Invariant(message),
        KernelError::Store(error) => turn_store_error(error),
    }
}

#[allow(clippy::needless_pass_by_value)] // This is a direct `map_err` adapter over an owned error.
pub(super) fn kernel_turn_error(error: TurnError) -> KernelError {
    KernelError::Invariant(bounded_diagnostic(&error.to_string()))
}
