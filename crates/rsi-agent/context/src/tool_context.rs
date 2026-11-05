use crate::{ContextBudget, ContextCredit, ContextError, Result, budget::weight};
use rsi_agent_session_protocol::{
    AgentMessageContent, EffectId, InputMessageSource, MAXIMUM_TOOL_CONTEXT_BYTES,
    MAXIMUM_TOOL_CONTEXT_MESSAGES, SessionFact, SessionFactBody, SessionId, SettledToolContext,
};
use rsi_tools_protocol::ToolContextMessage;
use std::{
    collections::{BTreeSet, VecDeque},
    sync::Arc,
};

#[derive(Debug, Default)]
pub(super) struct Pending {
    rows: VecDeque<(SettledToolContext, ContextCredit)>,
    bytes: usize,
}

impl Pending {
    fn reserve(
        &self,
        session: &SessionId,
        effect: &EffectId,
        message: &ToolContextMessage,
        budget: &ContextBudget,
    ) -> Result<ContextCredit> {
        // Field names, punctuation and the bounded ordinal occupy fewer than 64 bytes.
        let size = weight(session)? + weight(effect)? + weight(message)? + 64;
        if self.rows.len() >= MAXIMUM_TOOL_CONTEXT_MESSAGES
            || size > MAXIMUM_TOOL_CONTEXT_BYTES - self.bytes
        {
            return Err(ContextError::Capacity);
        }
        budget.reserve(size)
    }
    fn push(&mut self, input: SettledToolContext, credit: ContextCredit) {
        self.bytes += credit.bytes();
        self.rows.push_back((input, credit));
    }
    pub fn messages(&self) -> Vec<SettledToolContext> {
        self.rows.iter().map(|(row, _)| row.clone()).collect()
    }
    fn consume(
        &mut self,
        session: &SessionId,
        effect: &EffectId,
        ordinal: u16,
        inherited: bool,
        agrees: impl FnOnce(&ToolContextMessage) -> bool,
    ) -> Result<()> {
        let matches = |row: &SettledToolContext| {
            row.session == *session && row.effect == *effect && row.ordinal == ordinal
        };
        // A selected inherited interval can start after this specific tool result.
        if inherited
            && self.rows.front().is_none_or(|(row, _)| !matches(row))
            && !self.rows.iter().any(|(row, _)| matches(row))
        {
            return Ok(());
        }
        let (input, credit) = self
            .rows
            .front()
            .ok_or_else(|| ContextError::Invalid("tool context has no retained result".into()))?;
        if !matches(input) || !agrees(&input.message) {
            return Err(ContextError::Invalid(
                "entered tool context differs from its retained result".into(),
            ));
        }
        self.bytes -= credit.bytes();
        self.rows.pop_front();
        Ok(())
    }
    pub fn ingest(
        &mut self,
        source: &SessionId,
        facts: &[Arc<SessionFact>],
        inherited: bool,
        budget: &ContextBudget,
    ) -> Result<()> {
        for fact in facts {
            match fact.body() {
                SessionFactBody::ToolResult {
                    effect_id, result: rsi_tools_protocol::ToolResult { additional_contexts, .. }, ..
                } | SessionFactBody::ToolRejected { effect_id, additional_contexts, .. } => {
                    for (index, message) in additional_contexts.iter().enumerate() {
                        let credit = self.reserve(source, effect_id, message, budget)?;
                        let input = SettledToolContext {
                            session: source.clone(),
                            effect: effect_id.clone(),
                            ordinal: u16::try_from(index).map_err(|_| ContextError::Capacity)?,
                            message: message.clone(),
                        };
                        self.push(input, credit);
                    }
                }
                SessionFactBody::InputMessageClaimed{source:InputMessageSource::PluginContext{tool:Some(origin),..},content,..}=>self.consume(&origin.session,&origin.effect,origin.ordinal,inherited,|message|
                    origin.message_id==message.id && origin.source==message.source && content.len()==message.text.len() &&
                    content.iter().zip(&message.text).all(|(part,text)| matches!(part,AgentMessageContent::Text{text:actual} if actual==text)))?,
                SessionFactBody::ToolContextsClaimed{messages,..}=>{
                    for item in messages {self.consume(&item.session,&item.effect,item.ordinal,inherited,|message|message==&item.message)?;}
                }
                _=>{}
            }
        }
        Ok(())
    }
    pub fn encode(&self, budget: &ContextBudget) -> Result<(Vec<u8>, ContextCredit)> {
        if self.rows.is_empty() {
            return Ok((Vec::new(), budget.reserve(0)?));
        }
        let rows: Vec<_> = self.rows.iter().map(|(input, _)| input).collect();
        let credit = budget.reserve(weight(&rows)?)?;
        let bytes = serde_json::to_vec(&rows).map_err(|e| ContextError::Invalid(e.to_string()))?;
        Ok((bytes, credit))
    }
    pub fn restore(bytes: &[u8], budget: &ContextBudget) -> Result<Self> {
        if bytes.is_empty() {
            return Ok(Self::default());
        }
        if bytes.len() > MAXIMUM_TOOL_CONTEXT_BYTES + MAXIMUM_TOOL_CONTEXT_MESSAGES + 2 {
            return Err(ContextError::Capacity);
        }
        let _scratch = budget.reserve(bytes.len())?;
        let rows: Vec<SettledToolContext> =
            serde_json::from_slice(bytes).map_err(|e| ContextError::Invalid(e.to_string()))?;
        let mut pending = Self::default();
        let mut seen = BTreeSet::new();
        for input in rows {
            input
                .message
                .validate()
                .map_err(|e| ContextError::Invalid(e.to_string()))?;
            if input.ordinal >= 64
                || !seen.insert((input.session.clone(), input.effect.clone(), input.ordinal))
            {
                return Err(ContextError::Invalid(
                    "invalid pending tool context checkpoint".into(),
                ));
            }
            let credit = pending.reserve(&input.session, &input.effect, &input.message, budget)?;
            pending.push(input, credit);
        }
        Ok(pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{StepId, TurnId};
    use rsi_tools_protocol::{ToolResult, ToolResultIdentity};

    #[test]
    fn policy_message_survives_checkpoint_and_fork_until_its_claim() {
        use rsi_agent_session_protocol::{ContributionId, ToolOrigin, ToolRejection};
        let source = SessionId::new("parent").unwrap();
        let turn = TurnId::new("turn").unwrap();
        let effect = EffectId::new("refused").unwrap();
        let message = ToolContextMessage {
            id: "review".into(),
            source: serde_json::json!({"kind":"plugin"}),
            text: vec!["Try a read-only operation".into()],
        };
        let fact = Arc::new(
            SessionFact::new(
                1,
                1,
                SessionFactBody::ToolRejected {
                    turn_id: turn.clone(),
                    origin: ToolOrigin::Model {
                        effect_id: EffectId::new("model").unwrap(),
                    },
                    program_role: rsi_tools_protocol::ToolProgramRole::Unavailable,
                    effect_id: effect.clone(),
                    identity: ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64))
                        .unwrap(),
                    name: "bash".into(),
                    arguments: serde_json::json!({}),
                    rejection: ToolRejection::PolicyDenied {
                        contribution_id: ContributionId::new("policy").unwrap(),
                        reason: "Refused".into(),
                    },
                    additional_contexts: vec![message.clone()],
                },
            )
            .unwrap(),
        );
        for inherited in [false, true] {
            let budget = ContextBudget::default();
            let mut pending = Pending::default();
            pending
                .ingest(&source, std::slice::from_ref(&fact), inherited, &budget)
                .unwrap();
            let (bytes, _credit) = pending.encode(&budget).unwrap();
            let mut reopened = Pending::restore(&bytes, &budget).unwrap();
            let messages = reopened.messages();
            assert_eq!(
                messages,
                [SettledToolContext {
                    session: source.clone(),
                    effect: effect.clone(),
                    ordinal: 0,
                    message: message.clone()
                }]
            );
            let claim =
                SettledToolContext::claim(messages, &turn, &StepId::new("next").unwrap()).unwrap();
            reopened
                .ingest(
                    &source,
                    &[Arc::new(SessionFact::new(2, 2, claim).unwrap())],
                    inherited,
                    &budget,
                )
                .unwrap();
            assert!(reopened.messages().is_empty());
        }
    }

    #[test]
    fn multiple_full_results_fit_one_entered_fact_above_the_store_page_size() {
        let session = SessionId::new("root").unwrap();
        let turn = TurnId::new("tool-turn").unwrap();
        let step = StepId::new("next").unwrap();
        let budget = ContextBudget::default();
        let mut pending = Pending::default();
        for i in 0..9 {
            let effect = EffectId::new(format!("effect-{i}")).unwrap();
            let mut result = ToolResult::new(serde_json::Value::Null, vec![], false).unwrap();
            result.additional_contexts = (0..64)
                .map(|ordinal| ToolContextMessage {
                    id: format!("{i}-{ordinal}"),
                    source: serde_json::json!({"kind":"hook"}),
                    text: vec![format!("message {i}-{ordinal}"), "another block".into()],
                })
                .collect();
            let fact = SessionFact::new(
                i + 1,
                i + 1,
                SessionFactBody::ToolResult {
                    turn_id: turn.clone(),
                    effect_id: effect.clone(),
                    identity: ToolResultIdentity::new(
                        "owner",
                        effect.as_str(),
                        format!("call-{i}"),
                        "a".repeat(64),
                    )
                    .unwrap(),
                    result,
                    conclusion: None,
                },
            )
            .unwrap();
            pending
                .ingest(&session, &[Arc::new(fact)], false, &budget)
                .unwrap();
        }
        let bodies = SettledToolContext::claim(pending.messages(), &turn, &step)
            .into_iter()
            .collect::<Vec<_>>();
        let [SessionFactBody::ToolContextsClaimed { messages, .. }] = bodies.as_slice() else {
            panic!("expected a single batch")
        };
        assert_eq!(messages.len(), 576);
        assert_eq!(messages.first().unwrap().message.id, "0-0");
        assert_eq!(messages.last().unwrap().message.id, "8-63");
        let batch = SessionFact::new(10, 10, bodies.into_iter().next().unwrap()).unwrap();
        let wire = serde_json::to_vec(&batch).unwrap();
        let decoded = serde_json::from_slice(&wire).unwrap();
        pending
            .ingest(&session, &[Arc::new(decoded)], false, &budget)
            .unwrap();
        assert!(pending.messages().is_empty());
        assert_eq!(budget.used(), 0);
    }
}
