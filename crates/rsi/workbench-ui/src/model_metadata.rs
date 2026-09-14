//! Capacity snapshot, checked 2026-09-13 against the exact official model pages.
//! <https://api-docs.deepseek.com/quick_start/pricing/> (1M context, 384K output; conservative decimal token counts)
//! <https://developers.openai.com/api/docs/models/gpt-5>
//! <https://developers.openai.com/api/docs/models/gpt-5-mini>
use rsi_ai_protocol::DiscoveredModel;
use rsi_configuration_api::{DiscoveryRequest, ProviderKind};

/// Supplements missing capacity fields without adding candidates or matching custom endpoints.
/// Saved user limits must be selected before calling this helper.
pub fn supplement_model(request: &DiscoveryRequest, model: &mut DiscoveredModel) {
    let known = match (
        request.provider,
        request.endpoint.trim_end_matches('/'),
        model.id.as_str(),
    ) {
        (
            ProviderKind::Openai,
            "https://api.openai.com" | "https://api.openai.com/v1",
            "gpt-5" | "gpt-5-mini",
        ) => Some((400_000, 128_000)),
        (
            ProviderKind::Deepseek,
            "https://api.deepseek.com",
            "deepseek-flash" | "deepseek-v4-pro",
        ) => Some((1_000_000, 384_000)),
        _ => None,
    };
    if let Some((context, output)) = known {
        model.context_window_tokens.get_or_insert(context);
        model.max_output_tokens.get_or_insert(output);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn openai_version_base_and_trailing_slashes_share_official_metadata_only() {
        for base in [
            "https://api.openai.com",
            "https://api.openai.com/v1",
            "https://example.org/v1",
        ] {
            for suffix in ["", "/"] {
                let request = DiscoveryRequest {
                    provider: ProviderKind::Openai,
                    endpoint: format!("{base}{suffix}"),
                    slot: "default".into(),
                };
                let mut model = DiscoveredModel {
                    id: "gpt-5-mini".into(),
                    name: None,
                    context_window_tokens: None,
                    max_output_tokens: None,
                };
                supplement_model(&request, &mut model);
                assert_eq!(
                    model.context_window_tokens,
                    if base.starts_with("https://api.openai.com") {
                        Some(400_000)
                    } else {
                        None
                    }
                );
            }
        }
    }
    #[test]
    fn exact_official_candidates_only_and_online_capacity_wins() {
        let mut request = DiscoveryRequest {
            provider: ProviderKind::Openai,
            endpoint: "https://example.org".into(),
            slot: "default".into(),
        };
        let mut model = DiscoveredModel {
            id: "gpt-5".into(),
            name: None,
            context_window_tokens: None,
            max_output_tokens: Some(1000),
        };
        supplement_model(&request, &mut model);
        assert_eq!(model.context_window_tokens, None);
        request.endpoint = "https://api.openai.com".into();
        supplement_model(&request, &mut model);
        assert_eq!(model.context_window_tokens, Some(400_000));
        assert_eq!(model.max_output_tokens, Some(1000));
        model.id = "gpt-5-future".into();
        model.context_window_tokens = None;
        supplement_model(&request, &mut model);
        assert_eq!(model.context_window_tokens, None);
    }
}
