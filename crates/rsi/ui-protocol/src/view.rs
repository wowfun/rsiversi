use crate::{MAXIMUM_ELEMENTS, MAXIMUM_INPUT_BYTES, MAXIMUM_VIEW_BYTES, ProtocolError, Result};
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
    /// Validates names, cardinalities and the complete encoded envelope.
    pub fn validate(&self) -> Result<()> {
        if self.fields.len() > 32 || self.fields.keys().any(|key| !name_valid(key)) {
            return Err(ProtocolError("invalid form fields".into()));
        }
        bounded(self, MAXIMUM_INPUT_BYTES)
    }
}
/// Flat, closed view shared by Web cards/details and TUI cards/menus.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiView {
    /// Card or detail title.
    pub title: String,
    /// Ordered content; no HTML, script, arbitrary URL or secret input.
    pub elements: Vec<UiElement>,
}
/// Safe declarative presentation primitive.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
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
    /// Validates names, cardinalities and the complete encoded envelope.
    pub fn validate(&self) -> Result<()> {
        self.validate_semantics()?;
        bounded(self, MAXIMUM_VIEW_BYTES)
    }
    pub(crate) fn validate_semantics(&self) -> Result<()> {
        #[derive(Serialize)]
        struct Input<'a> {
            value: &'a Value,
            fields: &'a BTreeMap<&'a String, &'a String>,
        }
        if self.title.len() > 256 || self.elements.len() > MAXIMUM_ELEMENTS {
            return Err(ProtocolError("view exceeds element or title limit".into()));
        }
        let mut fields = BTreeMap::new();
        for element in &self.elements {
            match element {
                UiElement::Input { name, value, .. }
                    if !name_valid(name) || fields.insert(name, value).is_some() =>
                {
                    return Err(ProtocolError("invalid or duplicate view input".into()));
                }
                UiElement::Button { action, .. } if !name_valid(action) => {
                    return Err(ProtocolError("invalid view action name".into()));
                }
                _ => {}
            }
        }
        if fields.len() > 32 {
            return Err(ProtocolError("too many view inputs".into()));
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
        Ok(())
    }
}
/// Bound view; buttons resolve only through the same bundle and target.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundView {
    /// Exact originating surface or renderer reference.
    pub reference: UiReference,
    /// Validated declarative presentation.
    pub view: UiView,
    /// Exact action references for the view's buttons.
    pub actions: BTreeMap<String, UiReference>,
}
/// Menu entry over a concrete target and contribution generation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceDescriptor {
    /// Stable contribution name used to select a fresh target binding.
    pub bundle: String,
    /// Opaque bound reference to open.
    pub reference: UiReference,
    /// Plain menu label.
    pub title: String,
}

/// Checks a bounded protocol identity segment.
pub fn name_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}
/// Counts encoded JSON within a fixed bound without allocating a payload.
pub fn bounded(value: &impl Serialize, maximum: usize) -> Result<()> {
    write_bounded(value, maximum, &mut std::io::sink())
}
pub(crate) fn write_bounded(
    value: &impl Serialize,
    maximum: usize,
    writer: &mut impl Write,
) -> Result<()> {
    struct Bounded<'a, W> {
        remaining: usize,
        writer: &'a mut W,
    }
    impl<W: Write> Write for Bounded<'_, W> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.remaining = self
                .remaining
                .checked_sub(bytes.len())
                .ok_or_else(|| std::io::Error::other("quota"))?;
            self.writer.write_all(bytes)?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.writer.flush()
        }
    }
    serde_json::to_writer(
        Bounded {
            remaining: maximum,
            writer,
        },
        value,
    )
    .map_err(|error| ProtocolError(format!("could not write bounded UI data: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_views_reject_unknown_fields_at_every_structural_level() {
        use serde_json::json;
        let reference = json!({"application":"a", "target":"t", "contribution":"c", "name":"n"});
        let view = json!({"title":"View", "elements":[]});
        for mut value in [
            view.clone(),
            json!({"title":"View", "elements":[{"kind":"text", "text":"hello"}]}),
        ] {
            if value["elements"].as_array().unwrap().is_empty() {
                value["unexpected"] = json!(true);
            } else {
                value["elements"][0]["unexpected"] = json!(true);
            }
            assert!(serde_json::from_value::<UiView>(value.clone()).is_err());
            let mut model =
                serde_json::to_value(crate::UiModel::standard(UiView::default()).unwrap()).unwrap();
            model["standard_view"] = value;
            assert!(serde_json::from_value::<crate::UiModel>(model).is_err());
        }
        for element in [
            json!({"kind":"text", "text":"hello"}),
            json!({"kind":"code", "text":"code"}),
            json!({"kind":"field", "label":"Name", "value":"Value"}),
            json!({"kind":"input", "name":"input", "label":"Input", "value":"", "multiline":false}),
            json!({"kind":"button", "action":"submit", "label":"Submit", "value":null}),
        ] {
            assert!(serde_json::from_value::<UiElement>(element.clone()).is_ok());
            let mut invalid = element;
            invalid["unexpected"] = json!(true);
            assert!(serde_json::from_value::<UiElement>(invalid).is_err());
        }
        let mut bound = json!({"reference":reference, "view":view, "actions":{}});
        assert!(serde_json::from_value::<BoundView>(bound.clone()).is_ok());
        bound["unexpected"] = json!(true);
        assert!(serde_json::from_value::<BoundView>(bound).is_err());
        let mut surface = json!({"reference":reference, "bundle":"bundle", "title":"Surface"});
        assert!(serde_json::from_value::<SurfaceDescriptor>(surface.clone()).is_ok());
        surface["unexpected"] = json!(true);
        assert!(serde_json::from_value::<SurfaceDescriptor>(surface).is_err());
    }
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
