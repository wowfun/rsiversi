use crate::{AddonScope, StandardAddonSet, StandardComposition};

/// One Application catalog and the independent Service composition it consumes.
/// Application extras never participate in the Service Host launch identity.
#[derive(Clone, Debug)]
pub struct ApplicationComposition {
    pub(crate) service: StandardComposition,
    pub(crate) extras: StandardAddonSet,
    pub(crate) catalog: std::sync::Arc<dyn crate::ApplicationCatalogProvider>,
}

impl ApplicationComposition {
    /// Freezes Application-only declarations without activating any factory.
    pub fn new(
        service: StandardComposition,
        catalog: std::sync::Arc<dyn crate::ApplicationCatalogProvider>,
        extras: StandardAddonSet,
    ) -> rsi_host::Result<Self> {
        extras.validate_application_only()?;
        if service.application_metadata() != catalog.metadata() {
            return Err(rsi_host::HostError::Bootstrap(
                "application and Service catalog metadata differ".into(),
            ));
        }
        Ok(Self {
            service,
            extras,
            catalog,
        })
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
