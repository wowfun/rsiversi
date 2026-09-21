use super::{
    ApiError, Catalog, CatalogRequest, ChangeKind, Commit, Failure, Grant, Grants, HostEpoch,
    Outcome, Preview, Receipt, Result, SetGrant, Target, Ticket,
};
fn invalid() -> ApiError {
    ApiError::Invalid("Invalid Profile leaf metadata".into())
}
pub(super) fn previews(values: &[Preview], epoch: &HostEpoch) -> Result<()> {
    if values.len() > 4
        || values
            .windows(2)
            .any(|pair| pair[0].ticket >= pair[1].ticket)
    {
        return Err(invalid());
    }
    values.iter().try_for_each(|value| value.validate(epoch))
}
pub(super) fn hex(value: &str, size: usize) -> Result<()> {
    if value.len() != size
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid());
    }
    Ok(())
}
/// Validates the shared bounded leaf instance identity.
pub fn validate_leaf(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(invalid());
    }
    Ok(())
}
fn profile(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 255
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || value == "standard"
    {
        return Err(invalid());
    }
    Ok(())
}
impl Target {
    /// Validates the exact root and closed writable Host source selection.
    pub fn validate(&self) -> Result<()> {
        hex(&self.root, 64)?;
        profile(&self.profile)?;
        validate_leaf(&self.leaf)
    }
}
impl Grant {
    /// Validates a scope before persistence or presentation.
    pub fn validate(&self) -> Result<()> {
        self.target.validate()
    }
}
impl SetGrant {
    /// Bounds a grant CAS before admission or durable work.
    pub fn validate(&self) -> Result<()> {
        crate::revision(&self.expected)?;
        self.scope.validate()
    }
}
impl Commit {
    /// Checks the exact Host, ticket and review digest before source access.
    pub fn validate(&self, epoch: &HostEpoch) -> Result<()> {
        Ticket {
            host_epoch: self.host_epoch.clone(),
            ticket: self.ticket.clone(),
        }
        .validate(epoch)?;
        hex(&self.digest, 64)
    }
}
impl Ticket {
    /// Checks a finite ticket against its owning Host.
    pub fn validate(&self, epoch: &HostEpoch) -> Result<()> {
        if &self.host_epoch != epoch {
            return Err(ApiError::Unavailable);
        }
        hex(&self.ticket, 32)
    }
}
impl Grants {
    /// Validates exact revision and bounded, sorted, unique scope identities.
    pub fn validate(&self) -> Result<()> {
        crate::revision(&self.revision)?;
        if self.scopes.len() > 256 || self.scopes.windows(2).any(|p| p[0] >= p[1]) {
            return Err(invalid());
        }
        self.scopes.iter().try_for_each(Grant::validate)?;
        if serde_json::to_vec(self).map_err(|_| invalid())?.len() > 64 * 1024 {
            return Err(invalid());
        }
        Ok(())
    }
}
impl Failure {
    /// Validates bounded redacted diagnostic metadata.
    pub fn validate(&self) -> Result<()> {
        if let Self::DisabledAncestor(id) = self {
            validate_leaf(id)?;
        }
        Ok(())
    }
}
impl Preview {
    /// Validates one exact Host-bound prepared proposal.
    pub fn validate(&self, epoch: &HostEpoch) -> Result<()> {
        if &self.host_epoch != epoch || (self.effective_enabled && !self.enabled) {
            return Err(invalid());
        }
        hex(&self.ticket, 32)?;
        hex(&self.digest, 64)?;
        hex(&self.source_digest, 64)?;
        validate_leaf(&self.plugin)?;
        self.target.validate()?;
        if matches!(
            (self.operation, self.enabled),
            (ChangeKind::Enable, false) | (ChangeKind::Disable, true)
        ) {
            return Err(invalid());
        }
        Ok(())
    }
}
impl Receipt {
    /// Validates identity and separate source/runtime outcome.
    pub fn validate(&self, epoch: &HostEpoch) -> Result<()> {
        self.preview.validate(epoch)?;
        if let Outcome::Failed { failure } = &self.outcome {
            failure.validate()?;
        }
        Ok(())
    }
}
impl CatalogRequest {
    /// Bounds the selected source and lexical cursor before filesystem work.
    pub fn validate(&self) -> Result<()> {
        if let Some(id) = &self.profile {
            profile(id)?;
        }
        if let Some(after) = &self.after {
            if self.profile.is_some() {
                validate_leaf(after)?;
            } else {
                profile(after)?;
            }
        }
        Ok(())
    }
}
impl Catalog {
    /// Validates response progress and excludes mixed-profile or secret fields.
    pub fn validate(&self, request: &CatalogRequest, epoch: &HostEpoch) -> Result<()> {
        request.validate()?;
        if &self.host_epoch != epoch || self.profiles.len() > 64 || self.leaves.len() > 64 {
            return Err(invalid());
        }
        if let Some(root) = &self.root {
            hex(root, 64)?;
        } else if !self.profiles.is_empty() || !self.leaves.is_empty() || self.next.is_some() {
            return Err(invalid());
        }
        let ids = if let Some(selected) = &request.profile {
            if !self.profiles.is_empty() {
                return Err(invalid());
            }
            for leaf in &self.leaves {
                leaf.target.validate()?;
                validate_leaf(&leaf.plugin)?;
                if &leaf.target.profile != selected
                    || Some(&leaf.target.root) != self.root.as_ref()
                    || leaf.allowed.len() > 3
                    || leaf.allowed.windows(2).any(|p| p[0] >= p[1])
                    || (leaf.effective_enabled && !leaf.enabled)
                {
                    return Err(invalid());
                }
            }
            self.leaves
                .iter()
                .map(|v| v.target.leaf.as_str())
                .collect::<Vec<_>>()
        } else {
            if !self.leaves.is_empty() {
                return Err(invalid());
            }
            for id in &self.profiles {
                profile(id)?;
            }
            self.profiles.iter().map(String::as_str).collect()
        };
        if ids.windows(2).any(|p| p[0] >= p[1])
            || request
                .after
                .as_ref()
                .is_some_and(|after| ids.first().is_some_and(|id| *id <= after.as_str()))
            || self
                .next
                .as_ref()
                .is_some_and(|next| ids.is_empty() || ids.last().copied() != Some(next.as_str()))
        {
            return Err(invalid());
        }
        Ok(())
    }
}
