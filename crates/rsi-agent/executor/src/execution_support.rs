use super::*;

pub(super) async fn run_executor_pool(
    driver: Arc<Driver>,
    stop: CancellationToken,
) -> std::result::Result<(), String> {
    let admission = Arc::new(Semaphore::new(driver.config.maximum_active_turns));
    let mut lanes = JoinSet::<std::result::Result<(), String>>::new();
    let mut failure = None;
    loop {
        while let Some(result) = lanes.try_join_next() {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    failure.get_or_insert(error);
                    stop.cancel();
                }
                Err(error) => {
                    failure.get_or_insert_with(|| format!("Agent executor task failed: {error}"));
                    stop.cancel();
                }
            }
        }
        let claim = match driver.claim_next(&stop).await {
            Ok(Some(claim)) => claim,
            Ok(None) => {
                break;
            }
            Err(error) => {
                failure.get_or_insert_with(|| format!("Agent executor claim lane failed: {error}"));
                stop.cancel();
                break;
            }
        };
        let permit = tokio::select! {
            () = stop.cancelled() => {
                let _ignored = driver.turns.release(&claim);
                break;
            }
            permit = Arc::clone(&admission).acquire_owned() => {
                permit.map_err(|_| "Agent executor admission closed".to_owned())?
            }
        };
        let lane_service = Arc::new(ExecutorLaneParking {
            admission: Arc::clone(&admission),
            permit: Mutex::new(Some(permit)),
            stop: stop.clone(),
            closed: CancellationToken::new(),
        });
        let parking = ToolLaneParkingAuthority::new(lane_service.clone());
        let task_driver = Arc::clone(&driver);
        let task_stop = stop.clone();
        let task_failure_stop = stop.clone();
        lanes.spawn(EXECUTOR_LANE_PARKING.scope(parking, async move {
            let result = AssertUnwindSafe(task_driver.run_claim(claim, &task_stop))
                .catch_unwind()
                .await;
            lane_service.close();
            if result.is_ok() {
                Ok(())
            } else {
                task_failure_stop.cancel();
                Err("Agent executor claim task panicked".into())
            }
        }));
    }
    while let Some(result) = lanes.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                failure.get_or_insert(error);
            }
            Err(error) => {
                failure.get_or_insert_with(|| format!("Agent executor task failed: {error}"));
            }
        }
    }
    driver.abort_retirement_tasks().await;
    // All lanes have joined, so no effect can add another tracking pin. A lane
    // panic may bypass normal terminal retirement while this Driver stays alive.
    driver
        .active_tools
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    failure.map_or(Ok(()), Err)
}

pub(super) async fn publish_nonterminal_with_capacity_retry(
    turns: &dyn TurnExecution,
    config: &ExecutorConfig,
    claim: &TurnClaim,
    bodies: Vec<SessionFactBody>,
) -> std::result::Result<Vec<Arc<SessionFact>>, DriveFailure> {
    match turns.publish(claim, bodies).await {
        Ok(PublishAttempt::Published(facts)) => Ok(facts),
        Ok(PublishAttempt::FlushRequired { unpublished }) => {
            let tail = live_tail(turns, claim).await?;
            if tail == 0 {
                return Err(fatal(
                    "Fact publication requires a nonempty flushable prefix",
                ));
            }
            flush_execution_prefix(turns, config, claim, tail).await?;
            match turns.publish(claim, unpublished).await {
                Ok(PublishAttempt::Published(facts)) => Ok(facts),
                Ok(PublishAttempt::FlushRequired { .. }) => Err(fatal(
                    "Fact publication remained full after its durable flush",
                )),
                Err(error) => Err(turn_failure(error)),
            }
        }
        Err(error) => Err(turn_failure(error)),
    }
}

pub(super) async fn publish_terminal(
    turns: &dyn TurnExecution,
    config: &ExecutorConfig,
    claim: &TurnClaim,
    outcome: TurnOutcome,
) -> std::result::Result<Arc<SessionFact>, DriveFailure> {
    if let Some(terminal) = turns
        .finish_activation_turn(claim, &outcome)
        .await
        .map_err(turn_failure)?
    {
        return Ok(terminal);
    }
    turns
        .close_current_step(claim, &outcome)
        .await
        .map_err(turn_failure)?;
    let mut last_capacity_flush = None;
    let mut bodies = vec![SessionFactBody::TurnTerminal {
        turn_id: claim.turn_id().clone(),
        outcome,
    }];
    let facts = loop {
        match turns.publish(claim, bodies).await {
            Ok(PublishAttempt::Published(facts)) => break facts,
            Ok(PublishAttempt::FlushRequired { unpublished }) => {
                let tail = live_tail(turns, claim).await?;
                if tail == 0 || last_capacity_flush.is_some_and(|flushed| flushed >= tail) {
                    return Err(fatal(
                        "terminal publication remained full without new flushable Facts",
                    ));
                }
                flush_execution_prefix(turns, config, claim, tail).await?;
                last_capacity_flush = Some(tail);
                bodies = unpublished;
            }
            Err(error) => return Err(turn_failure(error)),
        }
    };
    let fact = facts
        .last()
        .ok_or_else(|| failed("executor.empty_publish", "publication returned no Facts"))?
        .clone();
    flush_execution_prefix(turns, config, claim, fact.seq()).await?;
    Ok(fact)
}

pub(super) async fn publish_budget_exhaustion(
    turns: &dyn TurnExecution,
    config: &ExecutorConfig,
    claim: &TurnClaim,
    dimension: BudgetDimension,
    consumed: u64,
    limit: u64,
) -> std::result::Result<Arc<SessionFact>, DriveFailure> {
    turns
        .close_current_step(
            claim,
            &TurnOutcome::BudgetExceeded {
                dimension,
                consumed,
                limit,
            },
        )
        .await
        .map_err(turn_failure)?;
    let mut bodies = vec![SessionFactBody::BudgetExhausted {
        turn_id: claim.turn_id().clone(),
        dimension,
        consumed,
        limit,
    }];
    let mut last_capacity_flush = None;
    let facts = loop {
        match turns.publish(claim, bodies).await {
            Ok(PublishAttempt::Published(facts)) => break facts,
            Ok(PublishAttempt::FlushRequired { unpublished }) => {
                let tail = live_tail(turns, claim).await?;
                if tail == 0 || last_capacity_flush.is_some_and(|flushed| flushed >= tail) {
                    return Err(fatal(
                        "budget publication remained full without new flushable Facts",
                    ));
                }
                flush_execution_prefix(turns, config, claim, tail).await?;
                last_capacity_flush = Some(tail);
                bodies = unpublished;
            }
            Err(error) => return Err(turn_failure(error)),
        }
    };
    let fact = facts
        .last()
        .ok_or_else(|| failed("executor.empty_publish", "publication returned no Facts"))?
        .clone();
    flush_execution_prefix(turns, config, claim, fact.seq()).await?;
    Ok(fact)
}

pub(super) async fn live_tail(
    turns: &dyn TurnExecution,
    claim: &TurnClaim,
) -> std::result::Result<u64, DriveFailure> {
    // Facts at or before the claim were already represented by its live watermark.
    // Only scan the executor-owned suffix when recovering publication capacity.
    let mut cursor = claim.live_seq();
    loop {
        let page = turns
            .read_facts(
                claim,
                cursor,
                rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ,
            )
            .await
            .map_err(fatal)?;
        let tail = page.through_seq;
        if tail == cursor {
            return Ok(cursor);
        }
        if tail <= cursor {
            return Err(fatal("Fact scan made no progress before terminal retry"));
        }
        cursor = tail;
    }
}

pub(super) async fn flush_execution_prefix(
    turns: &dyn TurnExecution,
    config: &ExecutorConfig,
    claim: &TurnClaim,
    through_seq: u64,
) -> std::result::Result<u64, DriveFailure> {
    tokio::time::timeout(config.durability_wait(), turns.flush(claim, through_seq))
        .await
        .map_err(|_| {
            fatal(format!(
                "Fact durability wait exceeded {} ms",
                config.durability_wait_ms
            ))
        })?
        .map_err(fatal)
}

pub(super) fn prepare_tool_effect(
    call: &ModelToolCall,
) -> std::result::Result<(EffectId, Value), Box<DriveFailure>> {
    let arguments = match call.kind {
        ToolCallKind::Function => parse_tool_arguments(&call.arguments)
            .map_err(|error| Box::new(failed("tool.invalid_arguments", error.to_string())))?,
        ToolCallKind::Freeform => Value::String(call.arguments.clone()),
    };
    Ok((
        next_effect_id().map_err(|error| Box::new(fatal(error)))?,
        arguments,
    ))
}

pub(super) fn next_effect_id() -> Result<EffectId> {
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy)
        .map_err(|error| ExecutorError::Invalid(format!("OS entropy failed: {error}")))?;
    EffectId::new(format!("effect-{:032x}", u128::from_le_bytes(entropy)))
        .map_err(|error| ExecutorError::Invalid(error.to_string()))
}

pub(super) fn should_retry(
    snapshot: &PreparedCallSnapshot,
    error: &rsi_ai_protocol::AiError,
    attempt: u8,
) -> bool {
    attempt < snapshot.retry_policy.max_retries()
        && snapshot.retry_policy.retries(error.kind())
        && matches!(
            error.dispatch_status(),
            DispatchStatus::NotStarted | DispatchStatus::NotDispatched
        )
}

pub(super) fn retry_delay(
    snapshot: &PreparedCallSnapshot,
    error: &rsi_ai_protocol::AiError,
    attempt: u8,
) -> Duration {
    let policy = &snapshot.retry_policy;
    let multiplier = 1_u64 << u32::from(attempt.min(16));
    let exponential = policy
        .initial_delay_ms()
        .saturating_mul(multiplier)
        .min(policy.max_delay_ms());
    let requested = error
        .retry_after_ms()
        .unwrap_or(0)
        .min(policy.max_delay_ms());
    let base = exponential.max(requested);
    let spread = base.saturating_mul(u64::from(policy.jitter_per_mille())) / 1_000;
    let seed = u64::from_str_radix(snapshot.request_sha256.get(..16).unwrap_or("0"), 16)
        .unwrap_or(0)
        ^ u64::from(attempt);
    let sampled = if spread == 0 {
        0
    } else {
        seed % spread.saturating_mul(2).saturating_add(1)
    };
    Duration::from_millis(base.saturating_sub(spread).saturating_add(sampled))
}

pub(super) struct CombinedCancellation {
    token: CancellationToken,
    listener: JoinHandle<()>,
}

impl CombinedCancellation {
    pub(super) fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    pub(super) fn cancel(&self) {
        self.token.cancel();
    }
}

impl Drop for CombinedCancellation {
    fn drop(&mut self) {
        self.token.cancel();
        self.listener.abort();
    }
}

pub(super) fn combine_cancellation(
    turn: &CancellationToken,
    stop: &CancellationToken,
) -> CombinedCancellation {
    let combined = CancellationToken::new();
    let output = combined.clone();
    let done = combined.clone();
    let turn = turn.clone();
    let stop = stop.clone();
    let listener = tokio::spawn(async move {
        tokio::select! {
            () = turn.cancelled() => output.cancel(),
            () = stop.cancelled() => output.cancel(),
            () = done.cancelled() => {}
        }
    });
    CombinedCancellation {
        token: combined,
        listener,
    }
}

pub(super) fn ai_failure(error: &rsi_ai_protocol::AiError) -> DriveFailure {
    if error.kind() == ErrorKind::Cancelled {
        return DriveFailure::Turn(TurnOutcome::Cancelled);
    }
    if error.dispatch_status() == DispatchStatus::Unknown {
        return DriveFailure::Turn(TurnOutcome::Interrupted {
            effect: Some(EffectKind::Model),
            reason: bounded(error.safe_summary()),
        });
    }
    DriveFailure::Turn(TurnOutcome::Failed {
        code: error.kind().code().into(),
        message: bounded(error.safe_summary()),
    })
}

pub(super) fn image_ai_failure(
    error: &rsi_ai_protocol::AiError,
    media: Vec<MediaRef>,
) -> DriveFailure {
    if media.is_empty() {
        if error.kind() == ErrorKind::Cancelled {
            return DriveFailure::Turn(TurnOutcome::Cancelled);
        }
        if error.dispatch_status() == DispatchStatus::Unknown {
            return DriveFailure::Turn(TurnOutcome::Interrupted {
                effect: Some(EffectKind::Image),
                reason: bounded(error.safe_summary()),
            });
        }
    }
    image_operation_failure(media, error.kind().code(), error.safe_summary())
}

pub(super) fn image_operation_failure(
    media: Vec<MediaRef>,
    code: impl Into<String>,
    message: impl Into<String>,
) -> DriveFailure {
    let code = bounded(&code.into());
    let message = bounded(&message.into());
    if media.is_empty() {
        DriveFailure::Turn(TurnOutcome::Failed { code, message })
    } else {
        DriveFailure::Turn(TurnOutcome::PartialFailed {
            media,
            code,
            message,
        })
    }
}

pub(super) fn tool_failure(error: &ToolError) -> DriveFailure {
    match error {
        ToolError::Cancelled => DriveFailure::Turn(TurnOutcome::Cancelled),
        ToolError::Timeout => failed("tool.timeout", "Tool invocation timed out"),
        ToolError::Capacity => failed("tool.capacity", "Tool capacity is exhausted"),
        ToolError::ShuttingDown => failed("tool.shutting_down", "Tool provider is shutting down"),
        ToolError::InvalidInput(_)
        | ToolError::Duplicate(_)
        | ToolError::Unknown(_)
        | ToolError::Withdrawn(_)
        | ToolError::Sealed
        | ToolError::Sandbox(_)
        | ToolError::Execution(_) => failed("tool.execution", error.to_string()),
    }
}

pub(super) fn failed(code: impl Into<String>, message: impl Into<String>) -> DriveFailure {
    DriveFailure::Turn(failure_outcome(code, message))
}

pub(super) fn failure_outcome(code: impl Into<String>, message: impl Into<String>) -> TurnOutcome {
    let code = code.into();
    let message = message.into();
    TurnOutcome::Failed {
        code: bounded(&code),
        message: bounded(&message),
    }
}

pub(super) fn fatal(error: impl fmt::Display) -> DriveFailure {
    DriveFailure::Fatal(bounded(&error.to_string()))
}

pub(super) fn turn_failure(error: TurnError) -> DriveFailure {
    match error {
        TurnError::ShuttingDown => DriveFailure::Stopped,
        TurnError::BudgetExceeded {
            dimension,
            consumed,
            limit,
        } => DriveFailure::Budget {
            dimension,
            consumed,
            limit,
        },
        other => fatal(other),
    }
}

pub(super) fn apply_finalization_failure(
    outcome: TurnOutcome,
    code: &str,
    message: &str,
    cleanup_failed: bool,
) -> TurnOutcome {
    let code = bounded(code);
    let message = bounded(message);
    match outcome {
        TurnOutcome::PartialFailed { media, .. } => TurnOutcome::PartialFailed {
            media,
            code,
            message,
        },
        TurnOutcome::Completed => TurnOutcome::Failed { code, message },
        _ if cleanup_failed => TurnOutcome::Failed { code, message },
        original => original,
    }
}

pub(super) fn bounded(value: &str) -> String {
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
    if output.is_empty() {
        output.push_str("Agent executor failed");
    }
    output
}

pub(super) fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[derive(Debug)]
pub(super) enum DriveFailure {
    Stopped,
    Turn(TurnOutcome),
    SettledTool {
        outcome: TurnOutcome,
        identity: ToolResultIdentity,
    },
    Fatal(String),
    Budget {
        dimension: BudgetDimension,
        consumed: u64,
        limit: u64,
    },
    DurableBudget {
        dimension: BudgetDimension,
        consumed: u64,
        limit: u64,
    },
    SettledToolBudget {
        dimension: BudgetDimension,
        consumed: u64,
        limit: u64,
        identity: ToolResultIdentity,
    },
}

pub(super) fn settled_tool_budget(
    failure: DriveFailure,
    identity: ToolResultIdentity,
) -> DriveFailure {
    match failure {
        DriveFailure::Budget {
            dimension,
            consumed,
            limit,
        } => DriveFailure::SettledToolBudget {
            dimension,
            consumed,
            limit,
            identity,
        },
        failure => failure,
    }
}
