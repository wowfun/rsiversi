use super::*;

#[allow(clippy::too_many_lines)] // Recovery classifies each unfinished Turn and commits its Fact/control boundary atomically.
pub(super) async fn repair_unfinished_session(
    store: &Arc<dyn SessionStore>,
    clock: &dyn Clock,
    session_id: &SessionId,
) -> Result<()> {
    store.validate_session(session_id).await?;
    let header = store.header(session_id).await?;
    let LoadedTurns {
        fact_seq: mut durable_seq,
        control_seq: mut durable_control_seq,
        turns,
        order: turn_order,
    } = load_control_state(store, None, &header).await?;
    if turns.is_empty() {
        return Ok(());
    }
    if turns
        .values()
        .filter(|turn| turn.activation_id.is_some())
        .count()
        > 1
    {
        return Err(KernelError::Invariant(
            "recovery found multiple active activations in one session".into(),
        ));
    }
    let timestamp = clock.now_ms().max(1);
    for turn_id in turn_order {
        let turn = turns
            .get(&turn_id)
            .expect("turn replay updates acceptance order and control state together");
        let mut final_seq = durable_seq;
        let mut repair = Vec::with_capacity(2);
        let outcome = if turn.cancel_requested {
            TurnOutcome::Cancelled
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
            SessionFactBody::TurnTerminal {
                turn_id: turn_id.clone(),
                outcome,
                result: None,
            },
        )?);

        let mut next_control_seq = durable_control_seq;
        let mut control_repairs = Vec::new();
        let mut guards = Vec::new();
        if let Some(activation_id) = &turn.activation_id {
            let active = store.active_activation(session_id).await?.ok_or_else(|| {
                KernelError::Invariant(
                    "recovery activation is absent from its durable index".into(),
                )
            })?;
            if &active.activation_id != activation_id || active.turn_id.as_ref() != Some(&turn_id) {
                return Err(KernelError::Invariant(
                    "recovery activation disagrees with its durable index".into(),
                ));
            }

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
                            turn_id,
                            step_id: turn.current_step.clone().ok_or_else(|| {
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
            let mailbox = store.read_agent_mailbox_summary(session_id).await?;
            for message_id in mailbox.pending_promotable_message_ids {
                next_control_seq = next_control_seq.checked_add(1).ok_or_else(|| {
                    KernelError::Invariant("recovery control sequence exhausted".into())
                })?;
                control_repairs.push(AgentControlRecord::new(
                    next_control_seq,
                    timestamp,
                    AgentControlRecordBody::MessagePromoted { message_id },
                )?);
            }
            next_control_seq = next_control_seq.checked_add(1).ok_or_else(|| {
                KernelError::Invariant("recovery control sequence exhausted".into())
            })?;
            let waiting = AgentControlRecord::new(
                next_control_seq,
                timestamp,
                AgentControlRecordBody::ActivationWaitingForDescendants {
                    activation_id: activation_id.clone(),
                },
            )?;
            control_repairs.push(waiting);

            guards.push(AgentActivationGuard {
                session_id: session_id.clone(),
                activation_id: activation_id.clone(),
            });
        }
        let mut commit = AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session_id.clone(),
                expected_fact_seq: durable_seq,
                expected_control_seq: durable_control_seq,
                header: None,
                facts: repair.into_iter().map(Arc::new).collect(),
                controls: control_repairs,
            }],
            required_active_activations: guards,
            quiescent_descendants_of: None,
        };
        notifications::correlate_terminal_boundaries(&mut commit, |_, _| {})?;
        let committed = store.commit_agent(commit).await?;
        let watermark = committed
            .sessions
            .first()
            .filter(|mark| &mark.session_id == session_id)
            .ok_or_else(|| {
                KernelError::Invariant("recovery commit returned no exact Session watermark".into())
            })?;
        durable_seq = watermark.durable_fact_seq;
        durable_control_seq = watermark.durable_control_seq;
    }
    Ok(())
}

pub(super) fn is_terminal_fact(fact: &SessionFact) -> bool {
    matches!(fact.body(), SessionFactBody::TurnTerminal { .. })
}

#[allow(clippy::too_many_lines)] // One snapshot joins open-Turn Facts and domain usage.
pub(super) struct LoadedTurns {
    pub(super) fact_seq: u64,
    pub(super) control_seq: u64,
    pub(super) turns: BTreeMap<TurnId, TurnControl>,
    pub(super) order: Vec<TurnId>,
}

pub(super) async fn load_control_state(
    store: &Arc<dyn SessionStore>,
    admission: Option<&KernelInner>,
    header: &SessionHeader,
) -> Result<LoadedTurns> {
    let evidence_cache = Mutex::default();
    let session_id = header.session_id();
    let mut open_cursor = 0_u64;
    let mut durable_seq = None;
    let mut turns = BTreeMap::new();
    let mut order = Vec::new();
    loop {
        let page = store
            .list_open_turns(session_id, open_cursor, MAXIMUM_FACTS_PER_READ)
            .await?;
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
                                rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
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
                    evidence::validate_references(
                        store.as_ref(),
                        &evidence_cache,
                        session_id,
                        fact.body(),
                    )
                    .await
                    .map_err(|error| KernelError::Invariant(error.to_string()))?;
                    super::execution::validate_intent_price(header, fact.body())
                        .map_err(|error| KernelError::Invariant(error.to_string()))?;
                    apply_recovered_fact(&mut turns, &mut order, header, fact)?;
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
                    "Store selected a terminal Fact stream".into(),
                ));
            }
            open_cursor = open_turn.accepted_seq;
        }
        if !page.has_more {
            return Ok(LoadedTurns {
                fact_seq: page.durable_seq,
                control_seq: page.durable_control_seq,
                turns,
                order,
            });
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
