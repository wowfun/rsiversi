//! Validated durable data and effect-free timer arithmetic.
use rsi_agent_session_protocol::{ContinuationInput, DomainRequestId, MessageId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const MIN_INTERVAL_MS: u64 = 60_000;

/// Explicit supported UTC timer rules; no local timezone or cron interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScheduleRule {
    /// One occurrence after a positive delay.
    After {
        /// Milliseconds from creation.
        delay_ms: u64,
    },
    /// One occurrence at an absolute future UTC instant.
    At {
        /// Unix milliseconds.
        at_ms: u64,
    },
    /// Fixed-rate occurrences anchored at creation.
    Every {
        /// Interval, at least one minute.
        interval_ms: u64,
    },
}
/// Durable reminder. Active intent alone never authorizes a timer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Reminder {
    /// Stable logical identity.
    pub id: DomainRequestId,
    /// Human-requested work, at most 2 KiB UTF-8.
    pub prompt: String,
    /// First scheduled UTC occurrence, retained for fixed-rate arithmetic.
    pub anchor_ms: u64,
    /// Next unaccepted occurrence.
    pub due_ms: u64,
    /// Recurrence interval; absent for a one-shot.
    pub interval_ms: Option<u64>,
    /// Durable intent, independent of process-local arming.
    pub active: bool,
    /// A one-shot occurrence has already been accepted.
    pub consumed: bool,
}
/// The most recent exact accepted automatic input.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ScheduleReservation {
    /// Reminder selected at this admission.
    pub reminder_id: DomainRequestId,
    /// Exact bounded prompt retained even after reminder deletion.
    pub prompt: String,
    /// Frozen allocation and input identity.
    pub input: ContinuationInput,
    /// Latest anchored occurrence represented by a coalesced round.
    pub occurrence_ms: u64,
    /// True only after a canonical mailbox discard or terminal Turn.
    pub settled: bool,
}
/// One bounded Session-wide allowance, never reset by reminder replacement.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ScheduleState {
    /// At most 16 current reminders.
    pub reminders: Vec<Reminder>,
    /// Lifetime accepted automatic parent Turns, at most 100.
    pub allocated_rounds: u64,
    /// Latest acceptance only; history remains in canonical controls.
    pub reservation: Option<ScheduleReservation>,
}
/// Validates a bounded set of reminders returned by a Store or remote catalog.
///
/// # Errors
/// Rejects duplicate IDs, invalid prompts and invalid recurrence arithmetic.
pub fn validate_reminders(reminders: &[Reminder]) -> Result<(), String> {
    if reminders.len() > 16 {
        return Err("invalid Schedule capacity".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    for reminder in reminders {
        prompt(&reminder.prompt)?;
        if !ids.insert(&reminder.id)
            || reminder.anchor_ms == 0
            || reminder.due_ms < reminder.anchor_ms
            || reminder.interval_ms.is_some_and(|interval| {
                interval < MIN_INTERVAL_MS || (reminder.due_ms - reminder.anchor_ms) % interval != 0
            })
            || (reminder.interval_ms.is_none() && reminder.due_ms != reminder.anchor_ms)
            || (reminder.consumed && (reminder.active || reminder.interval_ms.is_some()))
        {
            return Err("invalid reminder identity or anchored time".into());
        }
    }
    Ok(())
}
impl ScheduleState {
    /// Whether the Session has spent its lifetime automatic Turn allowance.
    pub const fn exhausted(&self) -> bool {
        self.allocated_rounds >= 100
    }

    /// Reads a committed snapshot with the Schedule codec.
    ///
    /// # Errors
    /// Rejects a different domain, an unsupported codec and malformed state.
    pub fn from_snapshot(
        snapshot: &rsi_agent_session_protocol::DomainSnapshot,
    ) -> Result<Self, String> {
        if snapshot.identity().id() != crate::SCHEDULE_DOMAIN
            || snapshot.identity().version() != crate::SCHEDULE_CODEC_VERSION
        {
            return Err("unsupported Schedule domain or codec".into());
        }
        let state =
            Self::deserialize(snapshot.state().value()).map_err(|error| error.to_string())?;
        state.validate()?;
        Ok(state)
    }
    /// Validates external/durable state at the owning boundary.
    ///
    /// # Errors
    /// Malformed capacity, timer arithmetic, identity or frozen input is rejected.
    pub fn validate(&self) -> Result<(), String> {
        if self.allocated_rounds > 100 || (self.allocated_rounds == 0) != self.reservation.is_none()
        {
            return Err("invalid Schedule capacity or allocation state".into());
        }
        validate_reminders(&self.reminders)?;
        if let Some(reservation) = &self.reservation {
            prompt(&reservation.prompt)?;
            reservation.input.validate().map_err(|e| e.to_string())?;
            if reservation.input.owner != owner()
                || reservation.input.round != self.allocated_rounds
                || reservation.input.message_id != message_id(self.allocated_rounds)?
                || reservation.occurrence_ms == 0
                || reservation.input.text
                    != render_input(
                        &reservation.reminder_id,
                        &reservation.prompt,
                        reservation.occurrence_ms,
                        self.allocated_rounds,
                    )
            {
                return Err("invalid frozen Schedule reservation".into());
            }
        }
        Ok(())
    }
    /// Adds new intent without spending or replenishing its Session allowance.
    ///
    /// # Errors
    /// Rejects exhausted capacity, duplicate identities, invalid prompts or timer overflow.
    pub fn create(
        &mut self,
        id: DomainRequestId,
        text: String,
        rule: ScheduleRule,
        now_ms: u64,
    ) -> Result<(), String> {
        prompt(&text)?;
        if self.exhausted()
            || self.reminders.len() >= 16
            || self.reminders.iter().any(|r| r.id == id)
        {
            return Err("Schedule allowance, reminder capacity or identity is unavailable".into());
        }
        let (due_ms, interval_ms) = match rule {
            ScheduleRule::After { delay_ms } if delay_ms > 0 => (now_ms.checked_add(delay_ms).ok_or("Schedule time overflow")?, None),
            ScheduleRule::At { at_ms } if at_ms > now_ms => (at_ms, None),
            ScheduleRule::Every { interval_ms } if interval_ms >= MIN_INTERVAL_MS => (now_ms.checked_add(interval_ms).ok_or("Schedule time overflow")?, Some(interval_ms)),
            _ => return Err("Schedule requires a future instant, positive delay or interval of at least one minute".into()),
        };
        if due_ms == 0 {
            return Err("invalid Schedule time".into());
        }
        self.reminders.push(Reminder {
            id,
            prompt: text,
            anchor_ms: due_ms,
            due_ms,
            interval_ms,
            active: true,
            consumed: false,
        });
        self.validate()
    }
    /// Selects exactly the retained reminders to resume; no budget reset.
    ///
    /// # Errors
    /// Rejects empty, duplicate or unknown selections; consumed or exhausted
    /// selections are accepted only to clean up their unsettled reservation.
    pub fn resume(&mut self, ids: &[DomainRequestId]) -> Result<(), String> {
        let unique = ids.iter().collect::<std::collections::BTreeSet<_>>();
        let available = !self.exhausted();
        let cleanup = self.reservation.as_ref().filter(|r| !r.settled);
        if ids.is_empty()
            || ids.len() > 16
            || unique.len() != ids.len()
            || ids.iter().any(|id| {
                let pending = cleanup.is_some_and(|r| &r.reminder_id == id);
                !pending
                    && !self
                        .reminders
                        .iter()
                        .any(|r| &r.id == id && !r.consumed && available)
            })
        {
            return Err(
                "resume requires pending reminders with allowance or an unsettled reservation"
                    .into(),
            );
        }
        for reminder in &mut self.reminders {
            reminder.active = unique.contains(&reminder.id) && !reminder.consumed && available;
        }
        Ok(())
    }
    /// Deletes one reminder without refunding its accepted work.
    ///
    /// # Errors
    /// Rejects an unknown reminder identity.
    pub fn delete(&mut self, id: &DomainRequestId) -> Result<Reminder, String> {
        let index = self
            .reminders
            .iter()
            .position(|r| &r.id == id)
            .ok_or("reminder not found")?;
        Ok(self.reminders.remove(index))
    }
    fn can_reserve(&self) -> bool {
        !self.exhausted() && self.reservation.as_ref().is_none_or(|r| r.settled)
    }
    /// Earliest eligible due time, absent while the previous accepted round is open.
    pub fn next_due(&self) -> Option<u64> {
        if !self.can_reserve() {
            return None;
        }
        self.reminders
            .iter()
            .filter(|r| r.active)
            .map(|r| r.due_ms)
            .min()
    }
    /// Proposes one due input for atomic idle admission. Failure preserves state.
    ///
    /// # Errors
    /// Rejects an unsettled round, absent due work or overflowing anchored arithmetic.
    pub fn reserve(&mut self, now_ms: u64) -> Result<ContinuationInput, String> {
        if !self.can_reserve() {
            return Err("no due Schedule round".into());
        }
        let (index, reminder) = self
            .reminders
            .iter()
            .enumerate()
            .filter(|(_, r)| r.active && r.due_ms <= now_ms)
            .min_by_key(|(_, r)| (r.due_ms, &r.id))
            .ok_or("no due Schedule round")?;
        let occurrence_ms = if let Some(interval) = reminder.interval_ms {
            let elapsed = now_ms
                .checked_sub(reminder.anchor_ms)
                .ok_or("invalid Schedule clock")?;
            reminder
                .anchor_ms
                .checked_add(
                    (elapsed / interval)
                        .checked_mul(interval)
                        .ok_or("Schedule time overflow")?,
                )
                .ok_or("Schedule time overflow")?
        } else {
            reminder.due_ms
        };
        let next = reminder
            .interval_ms
            .map(|interval| {
                occurrence_ms
                    .checked_add(interval)
                    .ok_or("Schedule time overflow")
            })
            .transpose()?;
        let round = self
            .allocated_rounds
            .checked_add(1)
            .ok_or("Schedule round overflow")?;
        let input = ContinuationInput {
            owner: owner(),
            round,
            message_id: message_id(round)?,
            text: render_input(&reminder.id, &reminder.prompt, occurrence_ms, round),
        };
        input.validate().map_err(|e| e.to_string())?;
        let reservation = ScheduleReservation {
            reminder_id: reminder.id.clone(),
            prompt: reminder.prompt.clone(),
            input: input.clone(),
            occurrence_ms,
            settled: false,
        };
        let reminder = &mut self.reminders[index];
        if let Some(next) = next {
            reminder.due_ms = next;
        } else {
            reminder.active = false;
            reminder.consumed = true;
        }
        self.allocated_rounds = round;
        self.reservation = Some(reservation);
        Ok(input)
    }
    /// Settles only the exact allocated input; accepted rounds are never refunded.
    ///
    /// # Errors
    /// Rejects a message that is not the current reservation.
    pub fn settle(&mut self, message: &MessageId) -> Result<(), String> {
        let reservation = self
            .reservation
            .as_mut()
            .filter(|r| &r.input.message_id == message)
            .ok_or("Schedule reservation changed")?;
        reservation.settled = true;
        Ok(())
    }
}
/// Stable logical owner for one Session's Schedule domain.
///
/// # Panics
/// Only if the protocol stops accepting the constant `schedule` identity.
pub fn owner() -> DomainRequestId {
    DomainRequestId::new("schedule").expect("constant identity")
}
/// Stable internal request identity for an allocated round.
///
/// # Errors
/// Rejects an operation whose encoded identity exceeds the protocol bounds.
pub fn request_id(round: u64, operation: &str) -> Result<DomainRequestId, String> {
    DomainRequestId::new(format!("schedule-{operation}-{round}")).map_err(|e| e.to_string())
}
fn message_id(round: u64) -> Result<MessageId, String> {
    MessageId::new(format!(
        "schedule-{:x}",
        Sha256::digest(format!("schedule-round-{round}"))
    ))
    .map_err(|e| e.to_string())
}
fn prompt(text: &str) -> Result<(), String> {
    if text.trim().is_empty()
        || text.len() > 2048
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        return Err(
            "reminder prompt must contain 1..=2048 UTF-8 bytes without control characters".into(),
        );
    }
    Ok(())
}

fn render_input(id: &DomainRequestId, prompt: &str, occurrence_ms: u64, round: u64) -> String {
    format!(
        "Scheduled reminder {id} at UTC Unix ms {occurrence_ms}. Automatic parent round {round}/100.\n{prompt}\nThis finite round cannot create or replenish reminders."
    )
}

#[cfg(test)]
rsi_persistence_testkit::payload_contract!(
    persistence_schedule,
    ScheduleState,
    crate::SCHEDULE_CODEC_VERSION,
    format!(
        "tests/fixtures/persistence/schedule-v{}.json",
        crate::SCHEDULE_CODEC_VERSION
    ),
    {
        let mut state = ScheduleState::default();
        let mut samples = vec![state.clone()];
        state
            .create(
                DomainRequestId::new("fixture").unwrap(),
                "Check 中🦀".into(),
                ScheduleRule::Every {
                    interval_ms: 300_000,
                },
                1000,
            )
            .unwrap();
        samples.push(state.clone());
        state.reserve(301_000).unwrap();
        samples.push(state);

        let samples: Vec<ScheduleState> = samples;
        for sample in &samples {
            sample.validate().unwrap();
        }
        samples
    }
);
