//! Checked configured cost reduction; serialized amounts never pass through floats.

use rsi_agent_session_protocol::{PriceError, PriceQuote};
use rsi_ai_protocol::TokenUsage;
use serde::{Deserialize, Serialize};

/// One currency's known subtotal, serialized as an exact decimal integer string.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrencyCost {
    pub currency: String,
    #[serde(with = "decimal")]
    pub nanos: u64,
}
impl CurrencyCost {
    /// Four decimal currency units, rounded half up at presentation only.
    pub fn display(&self) -> String {
        let units = self.nanos / 100_000 + u64::from(self.nanos % 100_000 >= 50_000);
        format!("{} {}.{:04}", self.currency, units / 10_000, units % 10_000)
    }
}

/// Known per-currency subtotals and reasons that some attempts remain unpriced.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredCost {
    pub totals: Vec<CurrencyCost>,
    pub priced_attempts: u64,
    pub missing_price: u64,
    pub missing_usage: u64,
    pub missing_breakdown: u64,
    pub overflow: u64,
}
impl ConfiguredCost {
    pub(crate) fn merge(&mut self, other: &Self) -> Result<(), &'static str> {
        let mut next = self.clone();
        for total in &other.totals {
            if let Some(own) = next
                .totals
                .iter_mut()
                .find(|own| own.currency == total.currency)
            {
                own.nanos = own
                    .nanos
                    .checked_add(total.nanos)
                    .ok_or("tree configured cost overflow")?;
            } else {
                if next.totals.len() == 8 {
                    return Err("tree configured cost exceeds eight currencies");
                }
                next.totals.push(total.clone());
            }
        }
        next.totals.sort_by(|a, b| a.currency.cmp(&b.currency));
        for (own, value) in [
            (&mut next.priced_attempts, other.priced_attempts),
            (&mut next.missing_price, other.missing_price),
            (&mut next.missing_usage, other.missing_usage),
            (&mut next.missing_breakdown, other.missing_breakdown),
            (&mut next.overflow, other.overflow),
        ] {
            *own = own.checked_add(value).ok_or("tree cost counter overflow")?;
        }
        *self = next;
        Ok(())
    }
    pub fn is_complete(&self) -> bool {
        self.missing_price == 0
            && self.missing_usage == 0
            && self.missing_breakdown == 0
            && self.overflow == 0
    }
    pub(crate) fn intent(&mut self, quote: Option<&PriceQuote>) -> Result<(), &'static str> {
        if quote.is_some() {
            increment(&mut self.missing_usage)
        } else {
            increment(&mut self.missing_price)
        }
    }
    pub(crate) fn usage(
        &mut self,
        quote: Option<&PriceQuote>,
        usage: TokenUsage,
    ) -> Result<(), &'static str> {
        let Some(quote) = quote else {
            return Ok(());
        };
        self.missing_usage = self
            .missing_usage
            .checked_sub(1)
            .ok_or("cost usage lacks an intent")?;
        match quote.cost_nanos(usage) {
            Ok(nanos) => {
                if let Some(total) = self
                    .totals
                    .iter_mut()
                    .find(|total| total.currency == quote.currency())
                {
                    let Some(sum) = total.nanos.checked_add(nanos) else {
                        return increment(&mut self.overflow);
                    };
                    total.nanos = sum;
                } else {
                    if self.totals.len() == 8 {
                        return Err("cost exceeds eight currencies");
                    }
                    self.totals.push(CurrencyCost {
                        currency: quote.currency().into(),
                        nanos,
                    });
                    self.totals.sort_by(|a, b| a.currency.cmp(&b.currency));
                }
                increment(&mut self.priced_attempts)
            }
            Err(PriceError::MissingBreakdown) => increment(&mut self.missing_breakdown),
            Err(PriceError::Overflow) => increment(&mut self.overflow),
        }
    }
    pub(crate) fn validate(&self, attempts: u64) -> Result<(), &'static str> {
        if self.totals.len() > 8
            || self
                .totals
                .windows(2)
                .any(|pair| pair[0].currency >= pair[1].currency)
            || self.totals.iter().any(|total| {
                !(3..=8).contains(&total.currency.len())
                    || !total.currency.bytes().all(|byte| byte.is_ascii_uppercase())
            })
        {
            return Err("invalid cost currency totals");
        }
        let count = [
            self.priced_attempts,
            self.missing_price,
            self.missing_usage,
            self.missing_breakdown,
            self.overflow,
        ]
        .into_iter()
        .try_fold(0_u64, u64::checked_add)
        .ok_or("cost counter overflow")?;
        if count != attempts {
            return Err("cost counters differ from attempt count");
        }
        Ok(())
    }
}

fn increment(value: &mut u64) -> Result<(), &'static str> {
    *value = value.checked_add(1).ok_or("cost counter overflow")?;
    Ok(())
}
mod decimal {
    use serde::{Deserialize, Deserializer, Serializer};
    #[allow(clippy::trivially_copy_pass_by_ref)] // Serde custom serializers borrow the field.
    pub fn serialize<S: Serializer>(value: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let text = String::deserialize(d)?;
        let value: u64 = text.parse().map_err(serde::de::Error::custom)?;
        if value.to_string() != text {
            return Err(serde::de::Error::custom("noncanonical cost integer"));
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_rounding_and_wire_preserve_exact_integer_amounts() {
        for (nanos, text) in [
            (49_999, "USD 0.0000"),
            (50_000, "USD 0.0001"),
            (999_999_999, "USD 1.0000"),
            (u64::MAX, "USD 18446744073.7096"),
        ] {
            let cost = CurrencyCost {
                currency: "USD".into(),
                nanos,
            };
            assert_eq!(cost.display(), text);
            let bytes = serde_json::to_vec(&cost).unwrap();
            assert_eq!(
                serde_json::from_slice::<CurrencyCost>(&bytes).unwrap(),
                cost
            );
        }
        assert!(
            serde_json::from_str::<CurrencyCost>(r#"{"currency":"USD","nanos":"01"}"#).is_err()
        );
        assert!(serde_json::from_str::<CurrencyCost>(r#"{"currency":"USD","nanos":1}"#).is_err());
    }
}
