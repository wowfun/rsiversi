use crate::{
    MAXIMUM_MODEL_REFERENCES, MAXIMUM_VIEW_BYTES, ProtocolError, Result, UiElement, UiReference,
    UiView, bounded, name_valid,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// A schema address understood by the selected renderer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSchema {
    /// Stable nominal schema name.
    pub name: String,
    /// Explicit positive version, independently of the renderer ABI.
    pub version: u16,
}
/// An action explicitly exposed by the current model.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelAction {
    /// Bundle-local operation name.
    pub name: String,
    /// Plain accessible label.
    pub title: String,
}
/// A source address interpreted only by the exact model owner.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSource {
    /// Model-local source identity; never a filesystem path or executable address.
    pub name: String,
    /// Plain accessible label.
    pub title: String,
    /// Declared content type for bounded byte reads.
    pub media_type: String,
}
/// Renderer-neutral snapshot payload produced by a contribution.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiModel {
    /// Stable renderer identity, resolved through the owner's admitted manifest.
    pub renderer: String,
    /// Nominal model schema.
    pub schema: ModelSchema,
    /// Bounded domain data; never executed by the host.
    pub data: Value,
    /// Explicit displayed action membership.
    pub actions: Vec<ModelAction>,
    /// Explicit source-read membership.
    pub sources: Vec<ModelSource>,
    /// Optional standard declarative presentation.
    pub standard_view: Option<UiView>,
}
impl UiModel {
    /// Builds a standard model, collecting the view's distinct button actions.
    pub fn standard(view: UiView) -> Result<Self> {
        view.validate()?;
        let mut actions = BTreeMap::new();
        for element in &view.elements {
            if let UiElement::Button { action, label, .. } = element {
                actions
                    .entry(action.clone())
                    .or_insert_with(|| label.clone());
            }
        }
        let model = Self {
            renderer: "rsi.standard".into(),
            schema: ModelSchema {
                name: "rsi.standard.view".into(),
                version: 1,
            },
            data: Value::Null,
            actions: actions
                .into_iter()
                .map(|(name, title)| ModelAction { name, title })
                .collect(),
            sources: Vec::new(),
            standard_view: Some(view),
        };
        model.validate()?;
        Ok(model)
    }
    /// Validates complete model data before publication.
    pub fn validate(&self) -> Result<()> {
        self.validate_semantics()?;
        bounded(self, MAXIMUM_VIEW_BYTES)
    }
    fn validate_semantics(&self) -> Result<()> {
        let invalid = || ProtocolError("invalid model identity or references".into());
        if !name_valid(&self.renderer)
            || !name_valid(&self.schema.name)
            || self.schema.version == 0
            || self.actions.len() > MAXIMUM_MODEL_REFERENCES
            || self.sources.len() > MAXIMUM_MODEL_REFERENCES
        {
            return Err(invalid());
        }
        let mut names = BTreeSet::new();
        for action in &self.actions {
            if !name_valid(&action.name) || action.title.len() > 256 || !names.insert(&action.name)
            {
                return Err(invalid());
            }
        }
        if let Some(view) = &self.standard_view {
            view.validate_semantics()?;
            for element in &view.elements {
                if let UiElement::Button { action, .. } = element
                    && !names.contains(action)
                {
                    return Err(ProtocolError(
                        "standard view action is absent from model".into(),
                    ));
                }
            }
        }
        names.clear();
        for source in &self.sources {
            if !name_valid(&source.name)
                || source.title.len() > 256
                || source.media_type.is_empty()
                || source.media_type.len() > 128
                || !source.media_type.bytes().all(|b| b.is_ascii_graphic())
                || !names.insert(&source.name)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
/// An exact live presentation, independently of contribution and target lifetimes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresentationIdentity {
    /// Exact registry, target and contribution generation that produced the model.
    pub reference: UiReference,
    /// Fresh presentation epoch assigned by the owner.
    pub epoch: String,
}
/// One immutable model revision; old readers retain their snapshot's admission.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSnapshot {
    /// Exact live presentation identity.
    pub presentation: PresentationIdentity,
    /// Presentation-local monotonic counter, unrelated to Facts and Profile revisions.
    pub revision: u64,
    /// Complete bounded model.
    pub model: UiModel,
}
impl ModelSnapshot {
    /// Validates the full wire envelope, not merely its inner model.
    pub fn validate(&self) -> Result<()> {
        self.validate_semantics()?;
        bounded(self, MAXIMUM_VIEW_BYTES)
    }
    /// Validates and writes one complete bounded envelope into caller-admitted storage.
    /// Partial output must be discarded when validation or writing fails.
    pub fn write_json(&self, writer: &mut impl std::io::Write) -> Result<()> {
        self.validate_semantics()?;
        crate::view::write_bounded(self, MAXIMUM_VIEW_BYTES, writer)
    }
    fn validate_semantics(&self) -> Result<()> {
        let reference = &self.presentation.reference;
        if self.revision == 0
            || !name_valid(&self.presentation.epoch)
            || [
                &reference.application,
                &reference.target,
                &reference.contribution,
                &reference.name,
            ]
            .iter()
            .any(|name| !name_valid(name))
        {
            return Err(ProtocolError("invalid presentation identity".into()));
        }
        self.model.validate_semantics()
    }
}
/// Invocation address fenced by the displayed presentation and snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresentationAction {
    /// Exact presentation epoch.
    pub presentation: PresentationIdentity,
    /// Exact displayed revision.
    pub revision: u64,
    /// Action explicitly exposed by that revision.
    pub action: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_writer_bounds_the_escaped_envelope_and_propagates_destination_failure() {
        let mut snapshot = ModelSnapshot {
            presentation: PresentationIdentity {
                reference: UiReference {
                    application: "app".into(),
                    target: "target".into(),
                    contribution: "bundle".into(),
                    name: "panel".into(),
                },
                epoch: "epoch".into(),
            },
            revision: 1,
            model: UiModel::standard(UiView::default()).unwrap(),
        };
        for size in [0, 100, MAXIMUM_VIEW_BYTES / 6 - 200, MAXIMUM_VIEW_BYTES / 6] {
            snapshot.model.data = Value::String("\u{0001}".repeat(size));
            let mut output = Vec::new();
            let result = snapshot.write_json(&mut output);
            assert_eq!(result.is_ok(), snapshot.validate().is_ok());
            assert!(output.len() <= MAXIMUM_VIEW_BYTES);
            if result.is_ok() {
                assert_eq!(
                    serde_json::from_slice::<ModelSnapshot>(&output)
                        .unwrap()
                        .model
                        .data,
                    snapshot.model.data
                );
            }
        }
        snapshot.model.data = Value::Null;
        assert!(snapshot.write_json(&mut [0_u8; 16].as_mut_slice()).is_err());
        snapshot.revision = 0;
        let mut output = Vec::new();
        assert!(snapshot.write_json(&mut output).is_err());
        assert!(output.is_empty(), "semantic rejection precedes writing");
    }
    #[test]
    fn model_membership_and_full_escaped_envelope_are_bounded() {
        let view = UiView {
            title: "Actions".into(),
            elements: vec![UiElement::Button {
                action: "save".into(),
                label: "Save".into(),
                value: Value::Null,
            }],
        };
        let mut model = UiModel::standard(view).unwrap();
        model.actions.clear();
        assert!(model.validate().is_err());
        model.standard_view = None;
        model.data = Value::String("\u{0001}".repeat(MAXIMUM_VIEW_BYTES / 6));
        assert!(model.validate().is_err());
        model.data = Value::Null;
        model.actions = vec![
            ModelAction {
                name: "save".into(),
                title: "Save".into()
            };
            2
        ];
        assert!(model.validate().is_err());
    }
}
