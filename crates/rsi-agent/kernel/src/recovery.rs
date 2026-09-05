use super::*;

#[allow(clippy::too_many_lines)] // Recovery classifies every unfinished Turn and its activation in one ordered repair append.
pub(super) async fn repair_unfinished_session(
    store: &Arc<dyn SessionStore>,
    clock: &dyn Clock,
    session_id: &SessionId,
) -> Result<()> {
    let open = store.list_open_turns(session_id, 0, 1).await?;
    if open.turns.is_empty() {
        return Ok(());
    }
    let header = store.header(session_id).await?;
    let (durable_seq, turns, turn_order, _workspace_context) =
        load_control_state(store, None, session_id, header.settings().turn_budget()).await?;
    if turns.len() != turn_order.len()
        || turn_order
            .iter()
            .any(|turn_id| !turns.contains_key(turn_id))
    {
        return Err(KernelError::Invariant(
            "recovery control state and turn order disagree".into(),
        ));
    }
    if turns.is_empty() {
        return Ok(());
    }
    let timestamp = clock.now_ms().max(1);
    let mut final_seq = durable_seq;
    let mut repair = Vec::with_capacity(turn_order.len().saturating_mul(2));
    let mut activation_repair = None;
    for turn_id in turn_order {
        let turn = turns
            .get(&turn_id)
            .expect("validated recovery turn order references exact state");
        if let Some(activation_id) = &turn.activation_id
            && activation_repair
                .replace((
                    activation_id.clone(),
                    turn_id.clone(),
                    turn.current_step.clone(),
                ))
                .is_some()
        {
            return Err(KernelError::Invariant(
                "recovery found multiple active activations in one session".into(),
            ));
        }
        let outcome = if turn.cancel_requested {
            TurnOutcome::Cancelled
        } else if let Some((dimension, consumed, limit)) = turn.budget_exhausted {
            TurnOutcome::BudgetExceeded {
                dimension,
                consumed,
                limit,
            }
        } else {
            let effect = turn.effects.values().find_map(|effect| match effect {
                ActiveEffect::Model { started: true, .. } => Some(EffectKind::Model),
                ActiveEffect::Image { started: true, .. } => Some(EffectKind::Image),
                ActiveEffect::Tool { started: true, .. } => Some(EffectKind::Tool),
                ActiveEffect::Model { started: false, .. }
                | ActiveEffect::Image { started: false, .. }
                | ActiveEffect::Tool { started: false, .. } => None,
            });
            TurnOutcome::Interrupted {
                effect,
                reason: "Kernel recovery found a turn without a durable terminal Fact".into(),
            }
        };
        if let Some(step_id) = &turn.current_step {
            final_seq = final_seq
                .checked_add(1)
                .ok_or_else(|| KernelError::Invariant("recovery Fact sequence exhausted".into()))?;
            repair.push(SessionFact::new(
                final_seq,
                timestamp,
                SessionFactBody::StepEnded {
                    turn_id: turn_id.clone(),
                    step_id: step_id.clone(),
                    outcome: StepOutcome::Stopped {
                        reason: "Kernel recovery interrupted the owning Turn".into(),
                    },
                },
            )?);
        }
        final_seq = final_seq
            .checked_add(1)
            .ok_or_else(|| KernelError::Invariant("recovery Fact sequence exhausted".into()))?;
        repair.push(SessionFact::new(
            final_seq,
            timestamp,
            SessionFactBody::TurnTerminal { turn_id, outcome },
        )?);
    }
    if let Some((activation_id, activation_turn_id, activation_step_id)) = activation_repair {
        let controls = store.read_controls(session_id, 0, 1).await?;
        let active = store.active_activation(session_id).await?.ok_or_else(|| {
            KernelError::Invariant("recovery activation is absent from its durable index".into())
        })?;
        if active.activation_id != activation_id
            || active.turn_id.as_ref() != Some(&activation_turn_id)
        {
            return Err(KernelError::Invariant(
                "recovery activation disagrees with its durable index".into(),
            ));
        }
        let mut next_control_seq = controls.durable_seq;
        let mut control_repairs = Vec::with_capacity(2);
        match active.phase {
            StoreActivationPhase::Running => {}
            StoreActivationPhase::Parked => {
                next_control_seq = next_control_seq.checked_add(1).ok_or_else(|| {
                    KernelError::Invariant("recovery control sequence exhausted".into())
                })?;
                control_repairs.push(AgentControlRecord::new(
                    next_control_seq,
                    timestamp,
                    AgentControlRecordBody::WaitResumed {
                        activation_id: activation_id.clone(),
                        turn_id: activation_turn_id,
                        step_id: activation_step_id.ok_or_else(|| {
                            KernelError::Invariant(
                                "parked recovery activation has no open Step".into(),
                            )
                        })?,
                        cause: WaitResumeCause::Cancel,
                    },
                )?);
            }
            StoreActivationPhase::WaitingForDescendants => {
                return Err(KernelError::Invariant(
                    "open recovery Turn belongs to an activation already waiting for descendants"
                        .into(),
                ));
            }
        }
        next_control_seq = next_control_seq
            .checked_add(1)
            .ok_or_else(|| KernelError::Invariant("recovery control sequence exhausted".into()))?;
        let waiting = AgentControlRecord::new(
            next_control_seq,
            timestamp,
            AgentControlRecordBody::ActivationWaitingForDescendants {
                activation_id: activation_id.clone(),
            },
        )?;
        control_repairs.push(waiting);
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: session_id.clone(),
                    expected_fact_seq: durable_seq,
                    expected_control_seq: controls.durable_seq,
                    header: None,
                    facts: repair,
                    controls: control_repairs,
                }],
                required_active_activations: vec![AgentActivationGuard {
                    session_id: session_id.clone(),
                    activation_id,
                }],
                quiescent_sessions: Vec::new(),
            })
            .await?;
    } else {
        store
            .append(AppendBatch {
                session_id: session_id.clone(),
                expected_seq: durable_seq,
                header: None,
                facts: repair,
            })
            .await?;
    }
    Ok(())
}

pub(super) fn is_terminal_fact(fact: &SessionFact) -> bool {
    matches!(fact.body(), SessionFactBody::TurnTerminal { .. })
}

pub(super) fn validate_durable_intent_fence(
    session: &SessionRuntime,
    body: &SessionFactBody,
) -> TurnResult<()> {
    let is_start = matches!(
        body,
        SessionFactBody::ModelStarted { .. }
            | SessionFactBody::ImageStarted { .. }
            | SessionFactBody::ToolStarted { .. }
    );
    if !is_start {
        return Ok(());
    }
    let turn = session
        .turns
        .get(body.turn_id())
        .ok_or_else(|| TurnError::Invalid("effect start references an unknown turn".into()))?;
    let matches_active_intent = match body {
        SessionFactBody::ModelStarted { effect_id, .. } => matches!(
            turn.effects.get(effect_id),
            Some(ActiveEffect::Model { started: false, .. })
        ),
        SessionFactBody::ImageStarted { effect_id, .. } => matches!(
            turn.effects.get(effect_id),
            Some(ActiveEffect::Image { started: false, .. })
        ),
        SessionFactBody::ToolStarted {
            effect_id,
            identity,
            ..
        } => matches!(
            turn.effects.get(effect_id),
            Some(ActiveEffect::Tool {
                identity: current,
                started: false,
                ..
            }) if current == identity
        ),
        _ => false,
    };
    let intent_is_pending = session.pending.iter().any(|fact| {
        matches!(
            (fact.body(), body),
            (
                SessionFactBody::ModelIntent { effect_id: intent, .. },
                SessionFactBody::ModelStarted { effect_id: started, .. }
            ) if intent == started
        ) || matches!(
            (fact.body(), body),
            (
                SessionFactBody::ImageIntent { effect_id: intent, .. },
                SessionFactBody::ImageStarted { effect_id: started, .. }
            ) if intent == started
        ) || matches!(
            (fact.body(), body),
            (
                SessionFactBody::ToolIntent { effect_id: intent, .. },
                SessionFactBody::ToolStarted { effect_id: started, .. }
            ) if intent == started
        )
    });
    if !matches_active_intent || intent_is_pending {
        return Err(TurnError::Invalid(
            "effect start requires its matching durable intent".into(),
        ));
    }
    Ok(())
}

pub(super) async fn load_control_state(
    store: &Arc<dyn SessionStore>,
    admission: Option<&KernelInner>,
    session_id: &SessionId,
    budget: &TurnBudget,
) -> Result<(
    u64,
    BTreeMap<TurnId, TurnControl>,
    Vec<TurnId>,
    WorkspaceContextState,
)> {
    let stored_workspace_context = store.read_workspace_context_state(session_id).await?;
    stored_workspace_context.validate()?;
    let workspace_context = WorkspaceContextState {
        instructions_sha256: stored_workspace_context.instructions_sha256.clone(),
        skill_catalog_sha256: stored_workspace_context.skill_catalog_sha256.clone(),
    };
    let mut open_cursor = 0_u64;
    let mut durable_seq = None;
    let mut turns = BTreeMap::new();
    let mut order = Vec::new();
    loop {
        let page = store
            .list_open_turns(session_id, open_cursor, MAXIMUM_FACTS_PER_READ)
            .await?;
        if page.durable_seq != stored_workspace_context.durable_fact_seq {
            return Err(KernelError::Invariant(
                "Store durable watermark changed during workspace-context state load".into(),
            ));
        }
        if durable_seq
            .replace(page.durable_seq)
            .is_some_and(|previous| previous != page.durable_seq)
        {
            return Err(KernelError::Invariant(
                "Store durable watermark changed during control-state load".into(),
            ));
        }
        for open_turn in &page.turns {
            let mut turn_cursor = 0_u64;
            loop {
                let turn_page = match admission {
                    Some(inner) => {
                        read_turn_facts_bounded(
                            inner,
                            session_id,
                            &open_turn.turn_id,
                            turn_cursor,
                            MAXIMUM_FACTS_PER_READ,
                        )
                        .await?
                    }
                    None => {
                        store
                            .read_turn_facts(
                                session_id,
                                &open_turn.turn_id,
                                turn_cursor,
                                MAXIMUM_FACTS_PER_READ,
                            )
                            .await?
                    }
                };
                if turn_page.durable_seq != page.durable_seq {
                    return Err(KernelError::Invariant(
                        "Store durable watermark changed during per-turn load".into(),
                    ));
                }
                for fact in &turn_page.facts {
                    apply_recovered_fact(&mut turns, &mut order, budget, fact)?;
                    turn_cursor = fact.seq();
                }
                if !turn_page.has_more {
                    break;
                }
                if turn_page.facts.is_empty() {
                    return Err(KernelError::Invariant(
                        "Store turn Fact page made no progress".into(),
                    ));
                }
            }
            if !turns.contains_key(&open_turn.turn_id) {
                return Err(KernelError::Invariant(
                    "Store open-turn index selected a terminal Fact stream".into(),
                ));
            }
            open_cursor = open_turn.accepted_seq;
        }
        if !page.has_more {
            return Ok((
                durable_seq.unwrap_or(page.durable_seq),
                turns,
                order,
                workspace_context,
            ));
        }
        if page.turns.is_empty() {
            return Err(KernelError::Invariant(
                "Store open-turn page made no progress".into(),
            ));
        }
    }
}

pub(super) async fn read_stored_outcome(
    inner: &KernelInner,
    session_id: &SessionId,
    turn_id: &TurnId,
) -> TurnResult<Option<TurnOutcome>> {
    let boundary = read_turn_boundary_bounded(inner, session_id, turn_id)
        .await
        .map_err(turn_store_error)?;
    let Some(terminal) = boundary.terminal() else {
        return Ok(None);
    };
    match terminal.body() {
        SessionFactBody::TurnTerminal { outcome, .. } => Ok(Some(outcome.clone())),
        _ => Err(TurnError::Invariant(
            "Store turn boundary returned a nonterminal terminal Fact".into(),
        )),
    }
}
