use crate::{Result, WorkspaceError, WorkspaceRecord};
use serde::{Deserialize, Serialize};

/// Complete workspace membership for device-local ordering.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceOrderSeed {
    /// Exact complete snapshot in strictly ascending identity order.
    Available {
        /// At most 1,024 records within a 128 KiB encoded envelope.
        records: Vec<WorkspaceRecord>,
    },
    /// Membership exceeded a bound; there is deliberately no partial membership.
    TooLarge {},
}

impl WorkspaceOrderSeed {
    /// Builds bounded membership from a trusted registry's identity-ordered snapshot.
    pub fn from_records<'a>(records: impl IntoIterator<Item = &'a WorkspaceRecord>) -> Self {
        let mut retained = Vec::new();
        let mut bytes = b"{\"kind\":\"available\",\"records\":[]}".len();
        for record in records {
            let Ok(length) = rsi_api_protocol::measure_json(record, 128 * 1024) else {
                return Self::TooLarge {};
            };
            bytes += length + usize::from(!retained.is_empty());
            if retained.len() == 1024 || bytes > 128 * 1024 {
                return Self::TooLarge {};
            }
            retained.push(record.clone());
        }
        Self::Available { records: retained }
    }

    /// Validates external identity, ordering, cardinality and encoded byte bounds.
    pub fn validate(&self) -> Result<()> {
        if let Self::Available { records } = self {
            if records.len() > 1024 || records.windows(2).any(|pair| pair[0].id >= pair[1].id) {
                return Err(WorkspaceError::Corrupt(
                    "invalid workspace order membership".into(),
                ));
            }
            for record in records {
                record.validate()?;
            }
            if rsi_api_protocol::measure_json(self, 128 * 1024).is_err() {
                return Err(WorkspaceError::Corrupt(
                    "workspace order membership exceeds 128 KiB".into(),
                ));
            }
        }
        Ok(())
    }
}
