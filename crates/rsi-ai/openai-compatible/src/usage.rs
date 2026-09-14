use rsi_ai_protocol::TokenUsage;
use rsi_ai_transport::ChatCompletionsUsage;
use serde::Deserialize;

/// Endpoint-declared input accounting; aliases never determine this policy.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageAccounting {
    /// Prompt tokens include the disjoint cache subsets.
    #[default]
    Inclusive,
    /// Prompt tokens exclude both required cache subsets.
    Exclusive,
}

pub(crate) fn normalize(
    wire: ChatCompletionsUsage,
    accounting: UsageAccounting,
) -> Result<TokenUsage, String> {
    let mut read = None;
    for value in [
        wire.prompt_cache_hit_tokens,
        wire.cache_read_input_tokens,
        wire.prompt_tokens_details.and_then(|v| v.cached_tokens),
    ]
    .into_iter()
    .flatten()
    {
        if read.is_some_and(|known| known != value) {
            return Err("provider cache-read aliases disagree".into());
        }
        read = Some(value);
    }
    let input = match accounting {
        UsageAccounting::Inclusive => wire.prompt_tokens,
        UsageAccounting::Exclusive => {
            let read = read.ok_or("exclusive usage requires a cache-read counter")?;
            let write = wire
                .cache_creation_input_tokens
                .ok_or("exclusive usage requires cache_creation_input_tokens")?;
            wire.prompt_tokens
                .checked_add(read)
                .and_then(|v| v.checked_add(write))
                .ok_or("exclusive input usage overflows")?
        }
    };
    TokenUsage::new(
        input,
        wire.completion_tokens,
        read,
        wire.cache_creation_input_tokens,
        wire.completion_tokens_details
            .and_then(|v| v.reasoning_tokens),
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accounting_is_declared_and_aliases_must_agree() {
        let wire = json!({"prompt_tokens":10,"completion_tokens":4,"cache_read_input_tokens":3,"cache_creation_input_tokens":2});
        assert_eq!(
            normalize(
                serde_json::from_value(wire.clone()).unwrap(),
                UsageAccounting::Inclusive
            )
            .unwrap()
            .input_tokens(),
            10
        );
        assert_eq!(
            normalize(
                serde_json::from_value(wire.clone()).unwrap(),
                UsageAccounting::Exclusive
            )
            .unwrap()
            .input_tokens(),
            15
        );
        let mut conflict = wire;
        conflict["prompt_cache_hit_tokens"] = json!(4);
        assert!(
            normalize(
                serde_json::from_value(conflict).unwrap(),
                UsageAccounting::Inclusive
            )
            .is_err()
        );
        let absent = json!({"prompt_tokens":10,"completion_tokens":4});
        assert_eq!(
            normalize(
                serde_json::from_value(absent.clone()).unwrap(),
                UsageAccounting::Inclusive
            )
            .unwrap()
            .cache_read_tokens(),
            None
        );
        assert!(
            normalize(
                serde_json::from_value(absent).unwrap(),
                UsageAccounting::Exclusive
            )
            .is_err()
        );
    }

    #[test]
    fn exclusive_accounting_accepts_each_read_alias_and_requires_write_evidence() {
        for read in [
            json!({"prompt_cache_hit_tokens": 3}),
            json!({"cache_read_input_tokens": 3}),
            json!({"prompt_tokens_details": {"cached_tokens": 3}}),
        ] {
            let mut wire =
                json!({"prompt_tokens":10,"completion_tokens":4,"cache_creation_input_tokens":2});
            wire.as_object_mut()
                .unwrap()
                .extend(read.as_object().unwrap().clone());
            let usage = normalize(
                serde_json::from_value(wire.clone()).unwrap(),
                UsageAccounting::Exclusive,
            )
            .unwrap();
            assert_eq!(usage.input_tokens(), 15);
            assert_eq!(usage.cache_read_tokens(), Some(3));
            wire.as_object_mut()
                .unwrap()
                .remove("cache_creation_input_tokens");
            assert!(
                normalize(
                    serde_json::from_value(wire).unwrap(),
                    UsageAccounting::Exclusive
                )
                .is_err()
            );
        }
    }

    #[test]
    fn exclusive_zero_cache_activity_requires_explicit_zero_counters() {
        let wire = serde_json::json!({
            "prompt_tokens": 10, "completion_tokens": 4,
            "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0,
        });
        let usage = normalize(
            serde_json::from_value(wire.clone()).unwrap(),
            UsageAccounting::Exclusive,
        )
        .unwrap();
        assert_eq!(usage.input_tokens(), 10);
        assert_eq!(usage.cache_read_tokens(), Some(0));
        assert_eq!(usage.cache_write_tokens(), Some(0));
        for field in ["cache_read_input_tokens", "cache_creation_input_tokens"] {
            for absent in [true, false] {
                let mut unknown = wire.clone();
                if absent {
                    unknown.as_object_mut().unwrap().remove(field);
                } else {
                    unknown[field] = serde_json::Value::Null;
                }
                assert!(
                    normalize(
                        serde_json::from_value(unknown).unwrap(),
                        UsageAccounting::Exclusive,
                    )
                    .is_err()
                );
            }
        }
    }

    #[test]
    fn impossible_subsets_and_overflows_are_rejected() {
        for wire in [
            json!({"prompt_tokens":1,"completion_tokens":1,"cache_read_input_tokens":2}),
            json!({"prompt_tokens":1,"completion_tokens":1,"completion_tokens_details":{"reasoning_tokens":2}}),
            json!({"prompt_tokens":u64::MAX,"completion_tokens":1}),
        ] {
            assert!(
                normalize(
                    serde_json::from_value(wire).unwrap(),
                    UsageAccounting::Inclusive
                )
                .is_err()
            );
        }
    }
}
