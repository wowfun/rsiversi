use super::*;
#[derive(Clone, Debug)]
pub(super) struct Child {
    pub session_id: SessionId,
    pub message_id: MessageId,
    pub activation: Option<ActivationId>,
    pub receipt: Option<ProgramChildReceipt>,
}
#[derive(Clone, Debug)]
pub(super) struct RunState {
    pub descriptor: ProgramRunDescriptor,
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
    pub fn apply(&mut self, event: &ProgramRunEvent) -> TurnResult<()> {
        event
            .validate(&self.descriptor.run_id)
            .map_err(|e| invalid(&e.to_string()))?;
        if self.outcome.is_some() {
            return Err(invalid("program is already terminal"));
        }
        match event {
            ProgramRunEvent::Accepted { .. } => {
                return Err(invalid("program was already accepted"));
            }
            ProgramRunEvent::Started if !self.started && !self.cancelling => self.started = true,
            ProgramRunEvent::Detached if self.started && !self.detached && !self.cancelling => {
                self.detached = true;
            }
            ProgramRunEvent::CancellationRequested if !self.cancelling => self.cancelling = true,
            ProgramRunEvent::ChildAdmitted {
                ordinal,
                child_session_id,
                message_id,
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
            } if !self.cancelling => {
                let child = self
                    .children
                    .get_mut(ordinal)
                    .ok_or_else(|| invalid("program child was not admitted"))?;
                if child.activation.is_some() || child.receipt.is_some() {
                    return Err(invalid(
                        "program child initial activation was already claimed",
                    ));
                }
                child.activation = Some(activation_id.clone());
            }
            ProgramRunEvent::ChildSettled { receipt } => {
                let child = self
                    .children
                    .get_mut(&receipt.ordinal)
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
                child.receipt = Some(receipt.clone());
            }
            ProgramRunEvent::Progress { phase, message } if self.started && !self.cancelling => {
                if phase.is_some() {
                    self.phase.clone_from(phase);
                }
                self.progress = Some(message.clone());
            }
            ProgramRunEvent::Terminal { outcome, result }
                if *outcome == ProgramOutcome::Interrupted
                    || self.children.values().all(|child| child.receipt.is_some()) =>
            {
                if *outcome == ProgramOutcome::Completed && (!self.started || self.cancelling) {
                    return Err(invalid("unstarted or cancelled program cannot complete"));
                }
                self.outcome = Some(outcome.clone());
                self.result.clone_from(result);
            }
            _ => {
                return Err(invalid(
                    "program transition is not admissible in its current state",
                ));
            }
        }
        Ok(())
    }
}
