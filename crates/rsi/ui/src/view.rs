use crate::{MAXIMUM_ELEMENTS, MAXIMUM_INPUT_BYTES, MAXIMUM_VIEW_BYTES, Result, UiError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write;

/// Explicit kind of actual Meta target.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// Application-wide capabilities.
    Application,
    /// One isolated Shell surface's capabilities.
    Surface,
}
/// Generation-bound surface or action address, never an authorization credential.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiReference {
    /// Fresh application nonce.
    pub application: String,
    /// Exact registered target generation.
    pub target: String,
    /// Exact contribution registration generation.
    pub contribution: String,
    /// Bundle-local surface or action name.
    pub name: String,
}
/// Data delivered to an action after registry bounds and generation checks.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionInput {
    /// Button's explicit business payload.
    #[serde(default)]
    pub value: Value,
    /// Current form values, validated by the action's business contract.
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}
impl ActionInput {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.fields.len() > 32 || self.fields.keys().any(|key| !name_valid(key)) {
            return Err(UiError::Invalid("invalid form fields".into()));
        }
        bounded(self, MAXIMUM_INPUT_BYTES)
    }
}
/// Flat, closed view shared by Web cards/details and TUI cards/menus.
#[derive(Clone, Debug, Default, Serialize)]
pub struct UiView {
    /// Card or detail title.
    pub title: String,
    /// Ordered content; no HTML, script, arbitrary URL or secret input.
    pub elements: Vec<UiElement>,
}
/// Safe declarative presentation primitive.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiElement {
    /// Plain text preserving hard line breaks.
    Text {
        /// Plain content.
        text: String,
    },
    /// Monospaced plain text.
    Code {
        /// Plain content.
        text: String,
    },
    /// Labeled read-only value.
    Field {
        /// Human label.
        label: String,
        /// Plain value.
        value: String,
    },
    /// Editable non-secret text submitted with an explicit button.
    Input {
        /// Form key.
        name: String,
        /// Human label.
        label: String,
        /// Initial text.
        value: String,
        /// Allow hard newlines.
        multiline: bool,
    },
    /// Bundle-local named action; registry binds its target before presentation.
    Button {
        /// Action name.
        action: String,
        /// Human label.
        label: String,
        /// Bounded explicit business payload.
        value: Value,
    },
}
impl UiView {
    pub(crate) fn validate(&self) -> Result<()> {
        #[derive(Serialize)]
        struct Input<'a> {
            value: &'a Value,
            fields: &'a BTreeMap<&'a String, &'a String>,
        }
        if self.title.len() > 256 || self.elements.len() > MAXIMUM_ELEMENTS {
            return Err(UiError::Invalid(
                "view exceeds element or title limit".into(),
            ));
        }
        let mut fields = BTreeMap::new();
        for element in &self.elements {
            match element {
                UiElement::Input { name, value, .. }
                    if !name_valid(name) || fields.insert(name, value).is_some() =>
                {
                    return Err(UiError::Invalid("invalid or duplicate view input".into()));
                }
                UiElement::Button { action, .. } if !name_valid(action) => {
                    return Err(UiError::Invalid("invalid view action name".into()));
                }
                _ => {}
            }
        }
        if fields.len() > 32 {
            return Err(UiError::Invalid("too many view inputs".into()));
        }
        bounded(
            &Input {
                value: &Value::Null,
                fields: &fields,
            },
            MAXIMUM_INPUT_BYTES,
        )?;
        for element in &self.elements {
            if let UiElement::Button { value, .. } = element {
                bounded(
                    &Input {
                        value,
                        fields: &fields,
                    },
                    MAXIMUM_INPUT_BYTES,
                )?;
            }
        }
        bounded(self, MAXIMUM_VIEW_BYTES)
    }
}
/// Bound view; buttons resolve only through the same bundle and target.
#[derive(Clone, Debug, Serialize)]
pub struct BoundView {
    /// Exact originating surface or renderer reference.
    pub reference: UiReference,
    /// Validated declarative presentation.
    pub view: UiView,
    /// Exact action references for the view's buttons.
    pub actions: BTreeMap<String, UiReference>,
}
/// Menu entry over a concrete target and contribution generation.
#[derive(Clone, Debug, Serialize)]
pub struct SurfaceDescriptor {
    /// Opaque bound reference to open.
    pub reference: UiReference,
    /// Plain menu label.
    pub title: String,
}

pub(crate) fn name_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}
pub(crate) fn bounded(value: &impl Serialize, maximum: usize) -> Result<()> {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| std::io::Error::other("quota"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(maximum), value)
        .map_err(|_| UiError::Invalid("encoded UI data exceeds its limit".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_form_and_button_payload_must_fit_the_action_envelope() {
        for (field, payload) in [
            ("x".repeat(MAXIMUM_INPUT_BYTES), Value::Null),
            ("x".repeat(32 * 1024), Value::String("y".repeat(32 * 1024))),
            ("\u{0001}".repeat(12 * 1024), Value::Null),
        ] {
            let input = ActionInput {
                value: payload.clone(),
                fields: BTreeMap::from([("value".into(), field.clone())]),
            };
            assert!(input.validate().is_err());
            let view = UiView {
                title: "Form".into(),
                elements: vec![
                    UiElement::Input {
                        name: "value".into(),
                        label: "Value".into(),
                        value: field,
                        multiline: true,
                    },
                    UiElement::Button {
                        action: "apply".into(),
                        label: "Apply".into(),
                        value: payload,
                    },
                ],
            };
            assert!(
                view.validate().is_err(),
                "a view must not publish defaults that cannot enter any action"
            );
        }
    }
}
