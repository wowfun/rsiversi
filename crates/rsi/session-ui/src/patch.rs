use super::{
    BoxFuture, CancellationToken, Context, Deserialize, Result, Serialize, SourceRef,
    SurfaceRenderer, UiElement, UiError, UiView, controller, source_button,
};
use rsi_ui::UiModel;

#[derive(Debug)]
pub(super) struct PatchCard(pub SourceRef);
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    version: u32,
    omitted: bool,
    diffs: Vec<Diff>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Diff {
    effect: usize,
    unified_diff: String,
}

impl SurfaceRenderer for PatchCard {
    fn model(&self, context: Context) -> BoxFuture<'_, Result<UiModel>> {
        Box::pin(async move {
            let controller = controller(&context)?;
            let stop = CancellationToken::new();
            let _cancel = stop.clone().drop_guard();
            let path =
                rsi_conversation::ToolValuePath::new(vec!["evidence".into()]).expect("fixed path");
            let result = controller
                .tool_value_window(self.0, path, 0, 96 * 1024, stop)
                .await;
            let mut elements = match result {
                Ok(window) if !window.more => evidence_elements(&window.text),
                Ok(_) => vec![UiElement::Text {
                    text: "Recorded diff exceeds the display limit. Open the complete result."
                        .into(),
                }],
                Err(rsi_client::SourceReadError::Unavailable) => vec![UiElement::Text {
                    text: "No recorded diff is available for this result.".into(),
                }],
                Err(failure) => return Err(UiError::Action(failure.to_string())),
            };
            elements.push(source_button("Complete result", self.0, 0));
            Ok(UiModel::standard(UiView {
                title: "Recorded file changes".into(),
                elements,
            })?)
        })
    }
}

fn evidence_elements(text: &str) -> Vec<UiElement> {
    let evidence = serde_json::from_str::<Evidence>(text).ok().filter(|value| {
        value.version == 1
            && serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= 32 * 1024)
            && value
                .diffs
                .windows(2)
                .all(|pair| pair[0].effect < pair[1].effect)
            && value.diffs.iter().all(|diff| !diff.unified_diff.is_empty())
    });
    let Some(evidence) = evidence else {
        return vec![UiElement::Text {
            text: "Recorded diff format is unsupported or invalid. Open the complete result."
                .into(),
        }];
    };
    let mut elements = Vec::new();
    if evidence.omitted {
        elements.push(UiElement::Text { text: "Some changes have no recorded diff because of size or content limits. The complete result contains the effect ledger.".into() });
    } else if evidence.diffs.is_empty() {
        elements.push(UiElement::Text { text: "No file diff was recorded. The complete result contains the outcome and effect ledger.".into() });
    }
    let diffs = evidence
        .diffs
        .into_iter()
        .map(|diff| format!("Effect {}\n{}", diff.effect, diff.unified_diff))
        .collect::<Vec<_>>()
        .join("\n");
    if !diffs.is_empty() {
        elements.push(UiElement::Code { text: diffs });
    }
    elements
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recorded_evidence_is_bounded_and_omission_is_not_success() {
        let valid = serde_json::json!({"version":1,"omitted":true,"diffs":[{"effect":2,"unified_diff":"--- a\n+++ b\n-旧\n+新\n"}]});
        let elements = evidence_elements(&valid.to_string());
        assert!(matches!(&elements[0], UiElement::Text { text } if text.contains("Some changes")));
        assert!(matches!(&elements[1], UiElement::Code { text } if text.ends_with("+新\n")));
        for invalid in [
            serde_json::json!({"version":2,"omitted":false,"diffs":[]}),
            serde_json::json!({"version":1,"omitted":false,"diffs":[{"effect":0,"unified_diff":"x".repeat(32768)}]}),
            serde_json::json!({"version":1,"omitted":false,"diffs":[{"effect":2,"unified_diff":"x"},{"effect":2,"unified_diff":"y"}]}),
        ] {
            assert!(
                matches!(&evidence_elements(&invalid.to_string())[..], [UiElement::Text { text }] if text.contains("unsupported or invalid"))
            );
        }
    }
}
