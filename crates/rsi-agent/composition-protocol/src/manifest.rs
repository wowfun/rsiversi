//! Redacted process-local evidence captured with a generation.

/// Implementation origin, without artifact paths or native digests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompositionOrigin {
    /// Statically linked factory.
    Linked,
    /// Native ABI factory.
    Native,
    /// No factory was resolved, for example a disabled leaf.
    Unresolved,
}

/// One flat desired instance captured before generation activation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompositionInstance {
    /// Stable instance identity.
    pub instance: String,
    /// Logical plugin identity.
    pub plugin: String,
    /// Effective enablement including all ancestors.
    pub enabled: bool,
    /// Resolved implementation category.
    pub origin: CompositionOrigin,
}

/// Bounded immutable manifest; intentionally has no durable codec.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompositionManifest(Vec<CompositionInstance>);
impl CompositionManifest {
    /// Validates and orders at most 8,192 unique bounded identities.
    ///
    /// # Errors
    /// Rejects duplicate, empty, oversized or control-containing identities.
    pub fn new(mut instances: Vec<CompositionInstance>) -> crate::Result<Self> {
        let identifier = |s: &str| {
            !s.is_empty()
                && s.len() <= 256
                && !s.chars().any(|c| c.is_control() || c.is_whitespace())
        };
        if instances.len() > 8192 {
            return Err(crate::AgentCompositionError::InvalidInput(
                "composition manifest exceeds 8192 entries".into(),
            ));
        }
        instances.sort_by(|a, b| a.instance.cmp(&b.instance));
        if instances
            .iter()
            .any(|row| !identifier(&row.instance) || !identifier(&row.plugin))
            || instances
                .windows(2)
                .any(|rows| rows[0].instance == rows[1].instance)
        {
            return Err(crate::AgentCompositionError::InvalidInput(
                "invalid composition manifest".into(),
            ));
        }
        Ok(Self(instances))
    }
    /// Returns the complete ordered redacted instance set.
    pub fn instances(&self) -> &[CompositionInstance] {
        &self.0
    }
}
