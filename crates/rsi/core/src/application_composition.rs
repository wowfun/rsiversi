use crate::{AddonScope, StandardAddonSet, StandardComposition};

/// One Application catalog and the independent Service composition it consumes.
/// Application extras never participate in the Service Host launch identity.
#[derive(Clone, Debug)]
pub struct ApplicationComposition {
    pub(crate) service: StandardComposition,
    pub(crate) extras: StandardAddonSet,
}

impl ApplicationComposition {
    /// Freezes Application-only declarations without activating any factory.
    pub fn new(service: StandardComposition, extras: StandardAddonSet) -> rsi_host::Result<Self> {
        extras.validate_application_only()?;
        Ok(Self { service, extras })
    }

    /// Returns the unchanged inputs selecting the Service Host.
    pub const fn service(&self) -> &StandardComposition {
        &self.service
    }

    /// Returns the independently supplied Application catalog declarations.
    pub const fn extras(&self) -> &StandardAddonSet {
        &self.extras
    }

    #[cfg(unix)]
    pub(crate) fn reserved_plugins(&self) -> std::collections::BTreeSet<String> {
        self.extras
            .descriptions()
            .filter(|entry| entry.scope == AddonScope::Application)
            .map(|entry| entry.plugin.clone())
            .collect()
    }
}

impl From<StandardComposition> for ApplicationComposition {
    fn from(service: StandardComposition) -> Self {
        Self {
            service,
            extras: StandardAddonSet::default(),
        }
    }
}
