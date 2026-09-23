//! Shared resource reads and deterministic completion semantics for applications.

use crate::SessionController;
use rsi_agent_session_protocol::{ContributionId, SessionResourceRequest, SessionResourceValue};
use rsi_session_protocol::{ResourceSnapshot, Result, SessionError};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

/// One completion candidate; selection replaces only the active input token.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputCompletion {
    /// Exact match key without the slash.
    pub name: String,
    /// Human description, including user-only visibility where applicable.
    pub description: String,
    /// Literal text inserted into the current token range.
    pub replacement: String,
    /// Commands precede skills; groups remain explicit in both clients.
    pub group: CompletionGroup,
    /// Resource coordinates for preview, independent of insertion text.
    pub resource: Option<SessionResourceRequest>,
}
/// Closed first-party input groups.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionGroup {
    /// Explicit application or Session command.
    Command,
    /// User-invocable skill.
    Skill,
    /// Named custom subagent.
    Agent,
}
/// Case-insensitive ordered subsequence match, with exact and prefix matches first.
pub fn completion_rank(name: &str, query: &str) -> Option<u8> {
    let name = name.to_lowercase();
    let query = query.to_lowercase();
    if name == query {
        return Some(0);
    }
    if name.starts_with(&query) {
        return Some(1);
    }
    let mut letters = name.chars();
    query
        .chars()
        .all(|wanted| letters.any(|letter| letter == wanted))
        .then_some(2)
}
/// Filters and ranks candidates without filesystem or Session work.
pub fn rank_completions(entries: &[InputCompletion], query: &str) -> Vec<InputCompletion> {
    let mut result: Vec<_> = entries
        .iter()
        .filter_map(|entry| {
            completion_rank(&entry.name, query)
                .map(|rank| (entry.group, rank, entry.name.to_lowercase(), entry.clone()))
        })
        .collect();
    result.sort_by(|left, right| {
        (&left.0, &left.1, &left.2, &left.3.name).cmp(&(
            &right.0,
            &right.1,
            &right.2,
            &right.3.name,
        ))
    });
    result.into_iter().map(|entry| entry.3).collect()
}

impl SessionController {
    /// Assembles shared slash candidates; applications supply their reserved names.
    /// Skill failures preserve usable commands and return a displayable diagnostic.
    pub async fn completion_catalog(
        &self,
        reserved: &[&str],
    ) -> Result<(Vec<InputCompletion>, String)> {
        let commands = self.commands().await?;
        let mut entries: Vec<_> = commands
            .commands()
            .iter()
            .filter(|entry| entry.name() != "model-selection" && !reserved.contains(&entry.name()))
            .map(|entry| InputCompletion {
                name: entry.name().into(),
                description: entry.description().into(),
                replacement: format!("/{}", entry.name()),
                group: CompletionGroup::Command,
                resource: None,
            })
            .collect();
        let names: Vec<_> = reserved
            .iter()
            .copied()
            .chain(
                commands
                    .commands()
                    .iter()
                    .map(rsi_agent_session_protocol::SessionCommandDescriptor::name),
            )
            .collect();
        let sources = match self.resource_sources().await {
            Ok(sources) => sources,
            Err(error) => {
                return Ok((
                    entries,
                    format!("Skills unavailable: {error}. Agents unavailable: {error}."),
                ));
            }
        };
        let mut notice = match self.skill_completions_from(&names, &sources).await {
            Ok(skills) => {
                entries.extend(skills);
                String::new()
            }
            Err(error) => format!("Skills unavailable: {error}."),
        };
        match self.agent_completions_from(&sources).await {
            Ok(agents) => entries.extend(agents),
            Err(error) => {
                let _ = write!(notice, " Agents unavailable: {error}.");
            }
        }
        Ok((entries, notice))
    }
    /// Reads the current named Agent catalog, including unavailable definitions for preview.
    pub async fn agent_completions(&self) -> Result<Vec<InputCompletion>> {
        self.agent_completions_from(&self.resource_sources().await?)
            .await
    }
    async fn resource_sources(&self) -> Result<Vec<ContributionId>> {
        let snapshot = self.read_resource(SessionResourceRequest::Sources).await?;
        let SessionResourceValue::Sources { sources } = &snapshot.response().value else {
            return Err(SessionError::Backend("invalid resource discovery".into()));
        };
        Ok(sources.clone())
    }
    async fn agent_completions_from(
        &self,
        sources: &[ContributionId],
    ) -> Result<Vec<InputCompletion>> {
        let source = ContributionId::new("rsi.agents")
            .map_err(|error| SessionError::Backend(error.to_string()))?;
        if !sources.contains(&source) {
            return Ok(Vec::new());
        }
        let snapshot = self
            .read_resource(SessionResourceRequest::List {
                source: source.clone(),
            })
            .await?;
        let SessionResourceValue::List { entries } = &snapshot.response().value else {
            return Err(SessionError::Backend("invalid Agent catalog".into()));
        };
        Ok(entries
            .iter()
            .map(|entry| InputCompletion {
                name: entry.name.clone(),
                description: entry.description.clone(),
                replacement: format!("@{}", entry.name),
                group: CompletionGroup::Agent,
                resource: Some(SessionResourceRequest::Read {
                    source: source.clone(),
                    id: entry.id.clone(),
                }),
            })
            .collect())
    }

    /// Reads a recorded reference under this controller's finite admission and lifetime.
    pub async fn read_recorded_reference(
        &self,
        request: rsi_agent_session_protocol::ReferenceReadRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<rsi_agent_session_protocol::ReferenceTextPage> {
        request
            .validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let _permit = self
            .submissions
            .clone()
            .try_acquire_owned()
            .map_err(|_| SessionError::Capacity)?;
        tokio::select! { biased;
            () = self.stop.cancelled() => Err(SessionError::ShuttingDown),
            () = cancellation.cancelled() => Err(SessionError::ShuttingDown),
            result = self.handle.read_recorded_reference(request) => result,
        }
    }
    /// Reads one bounded resource using controller admission and retirement.
    pub async fn read_resource(&self, request: SessionResourceRequest) -> Result<ResourceSnapshot> {
        let _permit = self
            .submissions
            .clone()
            .try_acquire_owned()
            .map_err(|_| SessionError::Capacity)?;
        tokio::select! { biased;
            () = self.stop.cancelled() => Err(SessionError::ShuttingDown),
            result = crate::read_with_capacity_retry(&self.execution, || self.handle.read_resource(request.clone())) => result,
        }
    }

    /// Discovers user-invocable skills from this Session's contribution catalog.
    pub async fn skill_completions(&self, command_names: &[&str]) -> Result<Vec<InputCompletion>> {
        self.skill_completions_from(command_names, &self.resource_sources().await?)
            .await
    }
    async fn skill_completions_from(
        &self,
        command_names: &[&str],
        sources: &[ContributionId],
    ) -> Result<Vec<InputCompletion>> {
        let source = ContributionId::new("rsi.workspace-skills").expect("static resource source");
        if !sources.contains(&source) {
            return Ok(Vec::new());
        }
        let snapshot = self
            .read_resource(SessionResourceRequest::List {
                source: source.clone(),
            })
            .await?;
        let SessionResourceValue::List { entries } = &snapshot.response().value else {
            return Err(SessionError::Backend("invalid skill discovery".into()));
        };
        Ok(entries
            .iter()
            .map(|entry| InputCompletion {
                name: entry.name.clone(),
                description: if entry.model_readable {
                    entry.description.clone()
                } else {
                    format!("User only · {}", entry.description)
                },
                replacement: if command_names.contains(&entry.name.as_str())
                    || entry.name == "skill"
                {
                    format!("/skill {}", entry.name)
                } else {
                    format!("/{}", entry.name)
                },
                group: CompletionGroup::Skill,
                resource: Some(SessionResourceRequest::Read {
                    source: source.clone(),
                    id: entry.id.clone(),
                }),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fuzzy_matching_is_case_insensitive_and_ordered() {
        assert_eq!(completion_rank("ReviewCode", "rc"), Some(2));
        assert_eq!(completion_rank("ReviewCode", "REV"), Some(1));
        assert_eq!(completion_rank("ReviewCode", "reviewcode"), Some(0));
        assert_eq!(completion_rank("ReviewCode", "erw"), None);
        assert_eq!(completion_rank("中文评审", "中审"), Some(2));
    }
}
