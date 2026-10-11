//! Immutable exact-route request output policy.

use crate::{Result, SessionError};
use rsi_ai_protocol::{LanguageSettings, ModelRef};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One configured request cap; model capacities do not supply this value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelOutputBudget {
    /// Exact deployment and model; aliases are not inferred.
    pub model: ModelRef,
    /// Explicit maximum generated tokens per Conversation request.
    pub max_output_tokens: u32,
}

/// Bounded Session-frozen output budgets with unique exact model keys.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct OutputBudgetTable(Vec<ModelOutputBudget>);

impl<'de> Deserialize<'de> for OutputBudgetTable {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Self::new(Vec::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl OutputBudgetTable {
    /// Creates a validated unique-route table.
    pub fn new(entries: Vec<ModelOutputBudget>) -> Result<Self> {
        let table = Self(entries);
        table.validate()?;
        Ok(table)
    }

    fn validate(&self) -> Result<()> {
        if self.0.len() > 256 || crate::compact_json_len(self)? > 64 * 1024 {
            return Err(SessionError::Invalid(
                "output budgets exceed the bounded table capacity".into(),
            ));
        }
        let mut models = BTreeSet::new();
        for entry in &self.0 {
            entry
                .model
                .validate()
                .map_err(|error| SessionError::Invalid(error.to_string()))?;
            LanguageSettings::default()
                .with_max_output_tokens(entry.max_output_tokens)
                .map_err(|error| SessionError::Invalid(error.to_string()))?;
            if !models.insert(&entry.model) {
                return Err(SessionError::Invalid(
                    "duplicate exact model output budget".into(),
                ));
            }
        }
        Ok(())
    }

    /// Returns only the cap configured for this exact model.
    pub fn cap(&self, model: &ModelRef) -> Option<u32> {
        self.0
            .iter()
            .find(|entry| &entry.model == model)
            .map(|entry| entry.max_output_tokens)
    }
}
