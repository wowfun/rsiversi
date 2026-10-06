use super::*;
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Child {
    pub session_id: SessionId,
    pub message_id: MessageId,
    pub activation: Option<ActivationId>,
    pub receipt: Option<ProgramChildReceipt>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RunState {
    pub descriptor: ProgramRunDescriptor,
    pub accepted_control_seq: u64,
    pub control_seq: u64,
    pub phase: Option<String>,
    pub progress: Option<String>,
    pub result: Option<ProgramBlob>,
    pub started: bool,
    pub detached: bool,
    pub cancelling: bool,
    pub outcome: Option<ProgramOutcome>,
    pub children: BTreeMap<u32, Child>,
}

impl RunState {
    pub fn replay(records: &rsi_agent_store_protocol::StoreProgramRecords) -> TurnResult<Self> {
        let Some(AgentControlRecordBody::ProgramRun {
            event: ProgramRunEvent::Accepted { descriptor },
            ..
        }) = records.records.first().map(AgentControlRecord::body)
        else {
            return Err(invalid("program history has no acceptance"));
        };
        let mut state = Self {
            descriptor: descriptor.as_ref().clone(),
            accepted_control_seq: records.head.first_control_seq,
            control_seq: records.head.last_control_seq,
            phase: None,
            progress: None,
            result: None,
            started: false,
            detached: false,
            cancelling: false,
            outcome: None,
            children: BTreeMap::new(),
        };
        for record in records.records.iter().skip(1) {
            let AgentControlRecordBody::ProgramRun { run_id, event } = record.body() else {
                return Err(invalid("foreign record in program history"));
            };
            if run_id != &state.descriptor.run_id {
                return Err(invalid("foreign run in program history"));
            }
            state.apply(event)?;
        }
        Ok(state)
    }
    pub fn validate_transition(&self, event: &ProgramRunEvent) -> TurnResult<()> {
        event
            .validate(&self.descriptor.run_id)
            .map_err(|e| invalid(&e.to_string()))?;
        if self.outcome.is_some() {
            return Err(invalid("program is already terminal"));
        }
        match event {
            ProgramRunEvent::Accepted { .. } => Err(invalid("program was already accepted")),
            ProgramRunEvent::Started if !self.started && !self.cancelling => Ok(()),
            ProgramRunEvent::Detached if self.started && !self.detached && !self.cancelling => {
                Ok(())
            }
            ProgramRunEvent::CancellationRequested if !self.cancelling => Ok(()),
            ProgramRunEvent::ChildAdmitted {
                ordinal,
                child_session_id,
                ..
            } if self.started
                && !self.cancelling
                && usize::try_from(*ordinal)
                    .is_ok_and(|ordinal| ordinal == self.children.len() + 1) =>
            {
                if self
                    .descriptor
                    .child_session_id(*ordinal)
                    .map_err(|e| invalid(&e.to_string()))?
                    != *child_session_id
                {
                    return Err(invalid(
                        "program child identity is not derived from its run",
                    ));
                }
                Ok(())
            }
            ProgramRunEvent::ChildStarted { ordinal, .. } if !self.cancelling => {
                let child = self
                    .children
                    .get(ordinal)
                    .ok_or_else(|| invalid("program child was not admitted"))?;
                if child.activation.is_some() || child.receipt.is_some() {
                    return Err(invalid(
                        "program child initial activation was already claimed",
                    ));
                }
                Ok(())
            }
            ProgramRunEvent::ChildSettled { receipt } => {
                let child = self
                    .children
                    .get(&receipt.ordinal)
                    .ok_or_else(|| invalid("program child receipt was not admitted"))?;
                if child.receipt.is_some()
                    || child.session_id != receipt.child_session_id
                    || child.activation != receipt.activation_id
                {
                    return Err(invalid(
                        "program child receipt differs from its exact initial activation",
                    ));
                }
                if child.activation.is_none() && receipt.outcome == ProgramOutcome::Completed {
                    return Err(invalid("unclaimed program child cannot complete"));
                }
                Ok(())
            }
            ProgramRunEvent::Progress { .. } if self.started && !self.cancelling => Ok(()),
            ProgramRunEvent::Terminal { outcome, .. }
                if *outcome == ProgramOutcome::Interrupted
                    || self.children.values().all(|child| child.receipt.is_some()) =>
            {
                if *outcome == ProgramOutcome::Completed && (!self.started || self.cancelling) {
                    return Err(invalid("unstarted or cancelled program cannot complete"));
                }
                Ok(())
            }
            _ => Err(invalid(
                "program transition is not admissible in its current state",
            )),
        }
    }

    pub fn apply(&mut self, event: &ProgramRunEvent) -> TurnResult<()> {
        self.validate_transition(event)?;
        match event {
            ProgramRunEvent::Accepted { .. } => {
                unreachable!("acceptance rejected by transition validation")
            }
            ProgramRunEvent::Started => self.started = true,
            ProgramRunEvent::Detached => self.detached = true,
            ProgramRunEvent::CancellationRequested => self.cancelling = true,
            ProgramRunEvent::ChildAdmitted {
                ordinal,
                child_session_id,
                message_id,
            } => {
                self.children.insert(
                    *ordinal,
                    Child {
                        session_id: child_session_id.clone(),
                        message_id: message_id.clone(),
                        activation: None,
                        receipt: None,
                    },
                );
            }
            ProgramRunEvent::ChildStarted {
                ordinal,
                activation_id,
            } => {
                self.children
                    .get_mut(ordinal)
                    .expect("validated child")
                    .activation = Some(activation_id.clone());
            }
            ProgramRunEvent::ChildSettled { receipt } => {
                self.children
                    .get_mut(&receipt.ordinal)
                    .expect("validated receipt")
                    .receipt = Some(receipt.clone());
            }
            ProgramRunEvent::Progress { phase, message } => {
                if phase.is_some() {
                    self.phase.clone_from(phase);
                }
                self.progress = Some(message.clone());
            }
            ProgramRunEvent::Terminal { outcome, result } => {
                self.outcome = Some(outcome.clone());
                self.result.clone_from(result);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn initial() -> RunState {
        let descriptor = ProgramRunDescriptor {
            run_id: ProgramRunId::new("transition-run").unwrap(),
            session_id: SessionId::new("transition-session").unwrap(),
            creator_turn_id: TurnId::new("creator-turn").unwrap(),
            creator_effect_id: EffectId::new("creator-effect").unwrap(),
            parent_header_sha256: "a".repeat(64),
            fork: ProgramForkBoundary {
                requested_turns: rsi_agent_session_protocol::ForkTurnSelection::All,
                resolved_after_seq: 0,
                resolved_terminal_seq: 0,
                terminal_prefix_sha256: "0".repeat(64),
                resolved_terminal_control_seq: 0,
                terminal_control_prefix_sha256: "0".repeat(64),
                effective_turns: 0,
            },
            selection: rsi_agent_session_protocol::ModelSelection {
                model: rsi_ai_protocol::ModelRef::new("test", "model").unwrap(),
                reasoning_effort: None,
            },
            sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
            require_approval: false,
            script: ProgramBlob {
                sha256: "b".repeat(64),
                bytes: 1,
            },
            guard: None,
        };
        descriptor.validate().unwrap();
        RunState {
            descriptor,
            accepted_control_seq: 1,
            control_seq: 1,
            phase: None,
            progress: None,
            result: None,
            started: false,
            detached: false,
            cancelling: false,
            outcome: None,
            children: BTreeMap::new(),
        }
    }

    fn events(initial: &RunState) -> [ProgramRunEvent; 9] {
        let child_id = initial.descriptor.child_session_id(1).unwrap();
        let activation = ActivationId::new("child-activation").unwrap();
        [
            ProgramRunEvent::Accepted {
                descriptor: Box::new(initial.descriptor.clone()),
            },
            ProgramRunEvent::Started,
            ProgramRunEvent::Detached,
            ProgramRunEvent::CancellationRequested,
            ProgramRunEvent::ChildAdmitted {
                ordinal: 1,
                child_session_id: child_id.clone(),
                message_id: MessageId::new("child-message").unwrap(),
            },
            ProgramRunEvent::ChildStarted {
                ordinal: 1,
                activation_id: activation.clone(),
            },
            ProgramRunEvent::ChildSettled {
                receipt: ProgramChildReceipt {
                    ordinal: 1,
                    child_session_id: child_id,
                    activation_id: Some(activation),
                    outcome: ProgramOutcome::Completed,
                    result: None,
                },
            },
            ProgramRunEvent::Progress {
                phase: Some("work".into()),
                message: "progress".into(),
            },
            ProgramRunEvent::Terminal {
                outcome: ProgramOutcome::Completed,
                result: Some(ProgramBlob {
                    sha256: "c".repeat(64),
                    bytes: 1,
                }),
            },
        ]
    }

    #[test]
    fn transition_matrix_keeps_admission_replay_and_rejected_state_consistent() {
        let initial = initial();
        let events = events(&initial);
        let started = advanced(&initial, &[&events[1]]);
        let admitted = advanced(&started, &[&events[4]]);
        let child_started = advanced(&admitted, &[&events[5]]);
        let settled = advanced(&child_started, &[&events[6]]);
        let detached = advanced(&started, &[&events[2]]);
        let cancelling = advanced(&child_started, &[&events[3]]);
        let terminal = advanced(&settled, &[&events[8]]);
        // Each row is an independently specified admission contract for all nine event variants.
        let cases = [
            (
                initial,
                [false, true, false, true, false, false, false, false, false],
            ),
            (
                started,
                [false, false, true, true, true, false, false, true, true],
            ),
            (
                admitted,
                [false, false, true, true, false, true, false, true, false],
            ),
            (
                child_started,
                [false, false, true, true, false, false, true, true, false],
            ),
            (
                settled,
                [false, false, true, true, false, false, false, true, true],
            ),
            (
                detached,
                [false, false, false, true, true, false, false, true, true],
            ),
            (
                cancelling,
                [false, false, false, false, false, false, true, false, false],
            ),
            (terminal, [false; 9]),
        ];
        for (before, allowed) in cases {
            for (event, allowed) in events.iter().zip(allowed) {
                let mut after = before.clone();
                let validated = before.validate_transition(event);
                let applied = after.apply(event);
                assert_eq!(
                    validated.is_ok(),
                    allowed,
                    "admission: {event:?}, {before:?}"
                );
                assert_eq!(applied.is_ok(), allowed, "replay: {event:?}, {before:?}");
                if allowed {
                    assert_mutation(&after, event);
                } else {
                    assert_eq!(after, before, "rejection must not mutate the fold");
                }
            }
            let interrupted = ProgramRunEvent::Terminal {
                outcome: ProgramOutcome::Interrupted,
                result: None,
            };
            let mut after = before.clone();
            assert_eq!(after.apply(&interrupted).is_ok(), before.outcome.is_none());
        }
    }

    fn advanced(before: &RunState, events: &[&ProgramRunEvent]) -> RunState {
        let mut after = before.clone();
        for event in events {
            after.apply(event).unwrap();
        }
        after
    }

    fn assert_mutation(after: &RunState, event: &ProgramRunEvent) {
        match event {
            ProgramRunEvent::Accepted { .. } => panic!("Accepted must return a typed error"),
            ProgramRunEvent::Started => assert!(after.started),
            ProgramRunEvent::Detached => assert!(after.detached),
            ProgramRunEvent::CancellationRequested => assert!(after.cancelling),
            ProgramRunEvent::ChildAdmitted {
                ordinal,
                child_session_id,
                message_id,
            } => {
                let child = &after.children[ordinal];
                assert_eq!(&child.session_id, child_session_id);
                assert_eq!(&child.message_id, message_id);
                assert!(child.activation.is_none() && child.receipt.is_none());
            }
            ProgramRunEvent::ChildStarted {
                ordinal,
                activation_id,
            } => {
                assert_eq!(
                    after.children[ordinal].activation.as_ref(),
                    Some(activation_id)
                );
            }
            ProgramRunEvent::ChildSettled { receipt } => {
                assert_eq!(
                    after.children[&receipt.ordinal].receipt.as_ref(),
                    Some(receipt)
                );
            }
            ProgramRunEvent::Progress { phase, message } => {
                assert_eq!(&after.phase, phase);
                assert_eq!(after.progress.as_ref(), Some(message));
            }
            ProgramRunEvent::Terminal { outcome, result } => {
                assert_eq!(after.outcome.as_ref(), Some(outcome));
                assert_eq!(&after.result, result);
            }
        }
    }
}
