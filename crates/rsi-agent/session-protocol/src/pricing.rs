//! Exact configured prices; no network discovery or currency conversion.

use std::collections::BTreeSet;

use rsi_ai_protocol::{ModelRef, PreparedCallSnapshot, TokenUsage};
use serde::{Deserialize, Serialize};

use crate::{Result, SessionError};

/// One exact deployment/endpoint/model tariff in currency billionths per token.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "QuoteWire")]
pub struct PriceQuote {
    model: ModelRef,
    endpoint_fingerprint: String,
    currency: String,
    input_nanos: u64,
    output_nanos: u64,
    cache_read_nanos: Option<u64>,
    cache_write_nanos: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuoteWire {
    model: ModelRef,
    endpoint_fingerprint: String,
    currency: String,
    input_nanos: u64,
    output_nanos: u64,
    cache_read_nanos: Option<u64>,
    cache_write_nanos: Option<u64>,
}

impl TryFrom<QuoteWire> for PriceQuote {
    type Error = SessionError;
    fn try_from(wire: QuoteWire) -> Result<Self> {
        let quote = Self {
            model: wire.model,
            endpoint_fingerprint: wire.endpoint_fingerprint,
            currency: wire.currency,
            input_nanos: wire.input_nanos,
            output_nanos: wire.output_nanos,
            cache_read_nanos: wire.cache_read_nanos,
            cache_write_nanos: wire.cache_write_nanos,
        };
        quote.validate()?;
        Ok(quote)
    }
}

impl PriceQuote {
    /// Checks exact identity and bounded ASCII currency code.
    pub fn validate(&self) -> Result<()> {
        self.model
            .validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        crate::validate_identifier("price endpoint fingerprint", &self.endpoint_fingerprint)?;
        if !(3..=8).contains(&self.currency.len())
            || !self.currency.bytes().all(|byte| byte.is_ascii_uppercase())
        {
            return Err(SessionError::Invalid(
                "price currency requires 3–8 uppercase ASCII letters".into(),
            ));
        }
        Ok(())
    }

    /// Currency code; unlike codes cannot be summed.
    pub fn currency(&self) -> &str {
        &self.currency
    }

    /// Whether this tariff belongs to exactly this prepared route.
    pub fn matches(&self, snapshot: &PreparedCallSnapshot) -> bool {
        self.model.deployment() == snapshot.deployment_id
            && self.model.model() == snapshot.model
            && self.endpoint_fingerprint == snapshot.endpoint_fingerprint
    }

    /// Exact cost. A missing differentiated subset and arithmetic overflow are distinct.
    pub fn cost_nanos(&self, usage: TokenUsage) -> std::result::Result<u64, PriceError> {
        let mut ordinary = usage.input_tokens();
        let mut amount = 0_u64;
        for (rate, count) in [
            (self.cache_read_nanos, usage.cache_read_tokens()),
            (self.cache_write_nanos, usage.cache_write_tokens()),
        ] {
            if let Some(rate) = rate {
                let count = count.ok_or(PriceError::MissingBreakdown)?;
                ordinary = ordinary.checked_sub(count).ok_or(PriceError::Overflow)?;
                amount = amount
                    .checked_add(count.checked_mul(rate).ok_or(PriceError::Overflow)?)
                    .ok_or(PriceError::Overflow)?;
            }
        }
        amount = amount
            .checked_add(
                ordinary
                    .checked_mul(self.input_nanos)
                    .ok_or(PriceError::Overflow)?,
            )
            .ok_or(PriceError::Overflow)?;
        amount
            .checked_add(
                usage
                    .output_tokens()
                    .checked_mul(self.output_nanos)
                    .ok_or(PriceError::Overflow)?,
            )
            .ok_or(PriceError::Overflow)
    }
}

/// Why a configured attempt could not be priced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PriceError {
    /// Provider omitted a subset required by a differentiated tariff.
    MissingBreakdown,
    /// The exact fixed-point result does not fit the bounded accumulator.
    Overflow,
}

/// Bounded immutable Session price table; empty means unconfigured.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Vec<PriceQuote>", into = "Vec<PriceQuote>")]
pub struct PriceTable(Vec<PriceQuote>);

impl From<PriceTable> for Vec<PriceQuote> {
    fn from(table: PriceTable) -> Self {
        table.0
    }
}
impl TryFrom<Vec<PriceQuote>> for PriceTable {
    type Error = SessionError;
    fn try_from(quotes: Vec<PriceQuote>) -> Result<Self> {
        let table = Self(quotes);
        table.validate()?;
        Ok(table)
    }
}
impl PriceTable {
    /// Checks entry, encoded-byte, currency and exact-key uniqueness bounds.
    pub fn validate(&self) -> Result<()> {
        if self.0.len() > 256
            || serde_json::to_vec(&self.0)
                .map_err(|error| SessionError::Encoding(error.to_string()))?
                .len()
                > 64 * 1024
        {
            return Err(SessionError::Invalid(
                "price table exceeds 256 quotes or 64 KiB".into(),
            ));
        }
        let mut keys = BTreeSet::new();
        let mut currencies = BTreeSet::new();
        for quote in &self.0 {
            quote.validate()?;
            if !keys.insert((&quote.model, &quote.endpoint_fingerprint)) {
                return Err(SessionError::Invalid("duplicate exact price route".into()));
            }
            currencies.insert(quote.currency());
        }
        if currencies.len() > 8 {
            return Err(SessionError::Invalid(
                "price table exceeds eight currencies".into(),
            ));
        }
        Ok(())
    }

    /// Whether any configured quotes exist.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Exact match only; no model aliases or endpoint fallback.
    pub fn resolve(&self, snapshot: &PreparedCallSnapshot) -> Option<&PriceQuote> {
        self.0.iter().find(|quote| quote.matches(snapshot))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wire() -> serde_json::Value {
        json!({"model":{"deployment":"fixture","model":"text"},"endpoint_fingerprint":"endpoint", "currency":"USD", "input_nanos":2500, "output_nanos":10000,"cache_read_nanos":250})
    }

    #[test]
    fn exact_rates_require_only_the_subsets_they_replace() {
        let quote: PriceQuote = serde_json::from_value(wire()).unwrap();
        assert_eq!(
            quote.cost_nanos(TokenUsage::new(100, 20, Some(80), None, None).unwrap()),
            Ok(270_000)
        );
        assert_eq!(
            quote.cost_nanos(TokenUsage::new(100, 20, None, None, None).unwrap()),
            Err(PriceError::MissingBreakdown)
        );
        let mut flat = wire();
        flat.as_object_mut().unwrap().remove("cache_read_nanos");
        let flat: PriceQuote = serde_json::from_value(flat).unwrap();
        assert_eq!(
            flat.cost_nanos(TokenUsage::new(100, 20, None, None, None).unwrap()),
            Ok(450_000)
        );
        assert_eq!(
            flat.cost_nanos(TokenUsage::new(u64::MAX, 0, None, None, None).unwrap()),
            Err(PriceError::Overflow)
        );
    }

    #[test]
    fn decoding_rejects_unknown_fields_duplicates_and_unbounded_tables() {
        let mut invalid = wire();
        invalid["extra"] = json!(0);
        assert!(serde_json::from_value::<PriceQuote>(invalid).is_err());
        let mut invalid = wire();
        invalid["currency"] = json!("usd");
        assert!(serde_json::from_value::<PriceQuote>(invalid).is_err());
        assert!(serde_json::from_value::<PriceTable>(json!([wire(), wire()])).is_err());
        assert!(serde_json::from_value::<PriceTable>(json!(vec![wire(); 257])).is_err());
        let quotes: Vec<_> = (0..9)
            .map(|i| {
                let mut q = wire();
                q["model"]["model"] = json!(format!("text-{i}"));
                q["currency"] = json!(format!("AA{}", char::from(b'A' + i)));
                q
            })
            .collect();
        assert!(serde_json::from_value::<PriceTable>(json!(quotes)).is_err());
    }
}
