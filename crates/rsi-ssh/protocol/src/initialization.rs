//! Bounded target program policy, distinct from resolved child environment.
use crate::{
    execution,
    frame::{FrameError, Result},
};
/// Shared explicit target configuration; values never copy the Service environment.
pub use rsi_execution::TargetProgram as ProgramPolicy;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
/// First-request initialization data. The caller has already checked live grants.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Initialization {
    /// Enables the immutable helper image as the reserved apply-patch program.
    pub apply_patch: bool,
    /// Enables the immutable helper image as the reserved project-context reader.
    pub workspace_context: bool,
    /// Enables the immutable helper image as the reserved directory picker.
    pub directory_picker: bool,
    /// Finite selection keys referenced by future resolve requests.
    pub programs: BTreeMap<String, ProgramPolicy>,
}
impl Initialization {
    /// Checks aggregate policy capacity before native resolution.
    pub fn validate(&self) -> Result<()> {
        if self
            .programs
            .len()
            .saturating_add(usize::from(self.apply_patch))
            .saturating_add(usize::from(self.workspace_context))
            .saturating_add(usize::from(self.directory_picker))
            > 128
        {
            return Err(FrameError::Capacity);
        }
        if self.programs.contains_key("apply_patch")
            || self.programs.contains_key("workspace_context")
            || self.programs.contains_key("directory_picker")
        {
            return Err(FrameError::Invalid);
        }
        let mut bytes = 0usize;
        for (selector, program) in &self.programs {
            execution::validate_selector(selector)?;
            program.validate().map_err(|_| FrameError::Invalid)?;
            bytes = bytes
                .saturating_add(selector.len())
                .saturating_add(program.command.len());
            for (key, value) in &program.environment {
                bytes = bytes
                    .saturating_add(key.len())
                    .saturating_add(value.len())
                    .saturating_add(2);
            }
            if bytes > rsi_process::MAXIMUM_PROCESS_ENVIRONMENT_BYTES {
                return Err(FrameError::Capacity);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builtin_selectors_cannot_be_replaced_and_count_toward_the_catalog_bound() {
        let policy = ProgramPolicy {
            command: "bash".into(),
            environment: vec![],
        };
        for enabled in [false, true] {
            for selector in ["apply_patch", "workspace_context", "directory_picker"] {
                assert_eq!(
                    Initialization {
                        apply_patch: enabled,
                        workspace_context: false,
                        directory_picker: false,
                        programs: BTreeMap::from([(selector.into(), policy.clone())]),
                    }
                    .validate(),
                    Err(FrameError::Invalid)
                );
            }
        }
        let mut configuration = Initialization {
            apply_patch: true,
            workspace_context: false,
            directory_picker: false,
            programs: (0..127)
                .map(|index| (format!("tool_{index}"), policy.clone()))
                .collect(),
        };
        configuration.validate().unwrap();
        configuration.programs.insert("last".into(), policy);
        assert_eq!(configuration.validate(), Err(FrameError::Capacity));
        configuration.apply_patch = false;
        configuration.validate().unwrap();
        configuration.workspace_context = true;
        assert_eq!(configuration.validate(), Err(FrameError::Capacity));
        configuration.programs.remove("last");
        configuration.validate().unwrap();
        configuration.apply_patch = true;
        assert_eq!(configuration.validate(), Err(FrameError::Capacity));
    }
    #[test]
    fn target_policy_rejects_outer_loader_hooks_and_service_or_lifecycle_environment() {
        for key in [
            "LD_PRELOAD",
            "LD_AUDIT",
            "LD_LIBRARY_PATH",
            "HOME",
            "PATH",
            "USER",
            "NOTIFY_SOCKET",
            "WATCHDOG_USEC",
            "DBUS_SESSION_BUS_ADDRESS",
            "SSH_AUTH_SOCK",
        ] {
            let policy = ProgramPolicy {
                command: "bash".into(),
                environment: vec![(key.into(), "fixture".into())],
            };
            assert!(policy.validate().is_err(), "accepted {key}");
        }
        for command in ["../bash", "-c", "a/b", "bash -c", "..", "C:\\bin\\bash"] {
            assert!(
                ProgramPolicy {
                    command: command.into(),
                    environment: vec![]
                }
                .validate()
                .is_err()
            );
        }
        ProgramPolicy {
            command: "/opt/target/node".into(),
            environment: vec![("TOKEN".into(), "explicitly-authorized".into())],
        }
        .validate()
        .unwrap();
    }
}
