use super::{
    AgentMessage, AgentMessageContent, AgentMessageSource, BTreeSet, WorkspaceContextError,
    valid_skill_name,
};

/// Bounded deduplicated skill names selected from direct-user input in source order.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceSkillRequests {
    names: Vec<String>,
    seen: BTreeSet<String>,
}

impl WorkspaceSkillRequests {
    /// Extracts at most 4,096 candidate tokens from bounded direct Human messages.
    pub fn from_messages(messages: &[&AgentMessage]) -> Result<Self, WorkspaceContextError> {
        if messages.len() > rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES
            || messages.iter().any(|message| {
                message.content.len()
                    > rsi_agent_session_protocol::MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS
            })
        {
            return Err(WorkspaceContextError::Capacity);
        }
        let mut requests = Self::default();
        for message in messages {
            if matches!(message.source, AgentMessageSource::Human) {
                requests.push_content(&message.content)?;
            }
        }
        Ok(requests)
    }

    /// Examines borrowed direct-user content without copying message payloads.
    pub fn push_content(
        &mut self,
        content: &[AgentMessageContent],
    ) -> Result<(), WorkspaceContextError> {
        for block in content {
            if let AgentMessageContent::Text { text } = block {
                self.push_text(text)?;
            }
        }
        Ok(())
    }

    /// Recognizes the first token on the first nonempty line of direct-user text.
    pub fn push_text(&mut self, text: &str) -> Result<(), WorkspaceContextError> {
        let Some(name) = text
            .lines()
            .find(|line| !line.trim().is_empty())
            .and_then(|line| line.split_whitespace().next())
            .and_then(|token| token.strip_prefix('/'))
            .filter(|name| valid_skill_name(name))
        else {
            return Ok(());
        };
        if self.seen.contains(name) {
            return Ok(());
        }
        if self.names.len() >= 4096 {
            return Err(WorkspaceContextError::Capacity);
        }
        self.seen.insert(name.to_owned());
        self.names.push(name.to_owned());
        Ok(())
    }

    /// Borrows validated selected names in first-occurrence order.
    pub fn names(&self) -> &[String] {
        &self.names
    }
}
