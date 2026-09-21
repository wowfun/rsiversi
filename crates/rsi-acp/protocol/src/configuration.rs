//! Operator-selected startup options; these convey no launch or prompt authority.
use crate::Error;
use serde::{Deserialize, Serialize};

/// One exact advertised ACP select-option assignment, applied in list order.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSelection {
    /// Exact peer option identifier, such as `model` or `effort`.
    pub id: String,
    /// Exact advertised string choice; never an inferred effort synonym.
    pub value: String,
}

/// Validates finite startup work and unambiguous option identities.
///
/// # Errors
/// Rejects more than eight assignments, duplicate IDs or invalid strings.
pub fn validate(selections: &[ConfigSelection]) -> Result<(), Error> {
    if selections.len() > 8 {
        return Err(Error::Limit);
    }
    let mut ids = std::collections::BTreeSet::new();
    for selection in selections {
        if !ids.insert(&selection.id)
            || [&selection.id, &selection.value]
                .iter()
                .any(|text| text.is_empty() || text.len() > 256 || text.contains('\0'))
        {
            return Err(Error::Parameters);
        }
    }
    Ok(())
}
