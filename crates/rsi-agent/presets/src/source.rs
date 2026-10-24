use rsi_meta_profile::{ProfileCandidate, ProfileCompiler, ProfileError, ProfileProgram};
use std::collections::BTreeSet;
use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fresh_and_named_isolation_use_the_registered_agent_markers() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("agent.profile.toml");
        let compiler = AgentPresetProfileCompiler::new(
            ProfileCompiler::new(
                rsi_meta_profile::ProfileEnvironment::without_paths("test", Default::default())
                    .unwrap(),
                Default::default(),
            ),
            ["test.child"],
        )
        .with_isolation_markers(["test.local".into()], ["test.event".into()]);
        for (lane, key) in [("local", "test.local"), ("events", "test.event")] {
            for named in [false, true] {
                for known in [false, true] {
                    let selected = if known { key } else { "unknown.marker" };
                    let value = if named {
                        format!("{{ key = '{selected}', label = 'shared' }}")
                    } else {
                        format!("'{selected}'")
                    };
                    std::fs::write(&path, format!("format = 1\n[[steps]]\nkind = 'group'\nid = 'scope'\n[steps.isolation]\n{lane} = [{value}]\n[[steps.nodes]]\nkind = 'plugin'\nid = 'leaf'\nplugin = 'test.child'\n")).unwrap();
                    let result = compiler.compile(&path);
                    assert_eq!(result.is_ok(), known, "{lane} named={named}: {result:?}");
                }
            }
        }
    }
}

/// Maximum UTF-8 bytes retained for one redacted Profile-health reason.
pub const MAX_PROFILE_HEALTH_REASON_BYTES: usize = 256;

/// Frozen pure source compiler and Agent-contribution policy for preset health.
///
/// This module sees only exact Agent contribution identities. It does not own
/// or expose the Host factory catalog and never prepares or activates a plugin.
#[derive(Clone, Debug)]
pub struct AgentPresetProfileCompiler {
    compiler: ProfileCompiler,
    allowed_plugins: BTreeSet<String>,
    local_markers: BTreeSet<String>,
    event_markers: BTreeSet<String>,
}

impl AgentPresetProfileCompiler {
    /// Freezes one Profile compiler and the exact enabled-contribution allowlist.
    pub fn new<I, S>(compiler: ProfileCompiler, allowed_plugins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            compiler,
            allowed_plugins: allowed_plugins.into_iter().map(Into::into).collect(),
            local_markers: BTreeSet::new(),
            event_markers: BTreeSet::new(),
        }
    }

    /// Supplies names from the same Agent declarations used by the runtime resolver.
    #[must_use]
    pub fn with_isolation_markers(
        mut self,
        local: impl IntoIterator<Item = String>,
        events: impl IntoIterator<Item = String>,
    ) -> Self {
        self.local_markers = local.into_iter().collect();
        self.event_markers = events.into_iter().collect();
        self
    }

    pub(crate) fn compile(
        &self,
        path: impl AsRef<Path>,
    ) -> std::result::Result<ProfileCandidate, String> {
        let candidate = self
            .compiler
            .compile(&ProfileProgram::from_file(path.as_ref()))
            .map_err(|error| profile_health_reason(&error))?;
        for leaf in candidate.leaves() {
            if !self.allowed_plugins.contains(leaf.plugin().as_str()) {
                return Err(bound_reason(
                    "Profile references an unknown Agent contribution".to_owned(),
                ));
            }
            for isolation in leaf.isolations() {
                use rsi_meta_profile::IsolationLane;
                let fresh = isolation
                    .local()
                    .iter()
                    .map(|key| (IsolationLane::Local, key.as_str()))
                    .chain(
                        isolation
                            .events()
                            .iter()
                            .map(|key| (IsolationLane::Event, key.as_str())),
                    );
                let named = isolation
                    .named()
                    .iter()
                    .map(|entry| (entry.lane(), entry.key()));
                for (lane, key) in fresh.chain(named) {
                    let known = match lane {
                        IsolationLane::Local => self.local_markers.contains(key),
                        IsolationLane::Event => self.event_markers.contains(key),
                        IsolationLane::Portable => true,
                    };
                    if !known {
                        return Err(
                            "the Agent Profile references an unknown isolation marker".into()
                        );
                    }
                }
            }
        }
        Ok(candidate)
    }
}

fn profile_health_reason(error: &ProfileError) -> String {
    let reason = match error {
        ProfileError::PathNotAbsolute { .. } | ProfileError::InvalidEnvironment(_) => {
            "the frozen Agent Profile environment is invalid".to_owned()
        }
        ProfileError::CapacityExceeded { resource, maximum } => {
            format!("the Agent Profile exceeds the {resource} maximum of {maximum}")
        }
        ProfileError::Source { .. } => {
            "the Agent Profile source agent.profile.toml or a required include is unavailable"
                .to_owned()
        }
        ProfileError::IncludeCycle { .. } => {
            "the Agent Profile contains an include cycle".to_owned()
        }
        ProfileError::UnsupportedFormat { format } => {
            format!("the Agent Profile format {format} is unsupported")
        }
        ProfileError::NonJsonToml { .. } => {
            "the Agent Profile contains a non-JSON TOML value".to_owned()
        }
        ProfileError::InvalidProgram(_) => "the Agent Profile program is invalid".to_owned(),
        ProfileError::MissingPatchTarget { .. } => {
            "the Agent Profile references a missing patch target".to_owned()
        }
        ProfileError::Expression { .. } => {
            "an Agent Profile expression could not be evaluated".to_owned()
        }
        ProfileError::DuplicateInstance { .. } => {
            "the Agent Profile contains a duplicate instance identity".to_owned()
        }
        _ => "the Agent Profile could not be preflighted".to_owned(),
    };
    bound_reason(reason)
}

fn bound_reason(mut reason: String) -> String {
    if reason.len() <= MAX_PROFILE_HEALTH_REASON_BYTES {
        return reason;
    }
    let mut end = MAX_PROFILE_HEALTH_REASON_BYTES;
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason.truncate(end);
    reason
}
