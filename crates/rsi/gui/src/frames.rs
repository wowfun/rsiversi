//! One bounded presentation baseline, independent of durable replay cursors.
use crate::GuiApplication;
use rsi_api_protocol::{ApiError, ByteBudget, Result, RetainedBytes};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    sync::Arc,
};
const MAX_BYTES: usize = 32 * 1024 * 1024;
#[path = "frame_view.rs"]
mod frame_view;
pub(crate) use frame_view::CachedPane;
#[cfg(test)]
use serde_json::json;

#[derive(Clone, Debug)]
pub(crate) struct PaneStamp {
    pub generation: Option<u64>,
    pub pane: Arc<()>,
    pub renderer: Option<Arc<()>>,
    pub ui: u64,
}
impl PartialEq for PaneStamp {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.ui == other.ui
            && Arc::ptr_eq(&self.pane, &other.pane)
            && match (&self.renderer, &other.renderer) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}
#[derive(Debug)]
struct PaneView {
    stamp: PaneStamp,
    value: CachedPane,
}
#[derive(Debug, Default)]
pub(crate) struct FrameState {
    pub(crate) id: u64,
    panes: BTreeMap<crate::SurfaceId, PaneView>,
    sections: Value,
    #[cfg(test)]
    projections: BTreeMap<crate::SurfaceId, usize>,
    #[cfg(test)]
    block_projections: usize,
}
impl FrameState {
    pub fn encode(
        &mut self,
        app: &GuiApplication,
        budget: &ByteBudget,
        base: Option<&str>,
    ) -> Result<RetainedBytes> {
        let ui = *app.ui.membership_changes().borrow();
        let surfaces = app.panes.lock().expect("GUI surfaces poisoned").clone();
        let stamps = surfaces
            .iter()
            .map(|(key, pane)| (*key, pane.stamp(ui)))
            .collect();
        self.capture_views(
            budget,
            base,
            &stamps,
            || app.sections(),
            |key, previous| surfaces[&key].frame_view(&app.ui, previous),
        )
    }
    #[cfg(test)]
    fn capture(
        &mut self,
        budget: &ByteBudget,
        base: Option<&str>,
        stamps: &BTreeMap<crate::SurfaceId, PaneStamp>,
        sections: impl FnOnce() -> Value,
        mut project: impl FnMut(crate::SurfaceId) -> Value,
    ) -> Result<RetainedBytes> {
        self.capture_views(budget, base, stamps, sections, |key, _| {
            CachedPane::from_value(project(key))
        })
    }
    fn capture_views(
        &mut self,
        budget: &ByteBudget,
        base: Option<&str>,
        stamps: &BTreeMap<crate::SurfaceId, PaneStamp>,
        sections: impl FnOnce() -> Value,
        mut project: impl FnMut(crate::SurfaceId, Option<&CachedPane>) -> Result<CachedPane>,
    ) -> Result<RetainedBytes> {
        let base = base.map(frame_id).transpose()?;
        let mut snapshot =
            self.id == 0 || base != Some(self.id) || !self.panes.keys().eq(stamps.keys());
        let id = self
            .id
            .checked_add(1)
            .ok_or_else(|| ApiError::Invalid("GUI frame sequence exhausted".into()))?;
        let reservation = budget.reserve(MAX_BYTES)?;
        let mut replacements = BTreeMap::new();
        let mut size = 32;
        for (key, stamp) in stamps {
            if self.panes.get(key).is_none_or(|old| old.stamp != *stamp) {
                let previous = self.panes.get(key);
                let same_generation =
                    previous.is_some_and(|old| old.stamp.generation == stamp.generation);
                snapshot |= previous.is_some() && !same_generation;
                let value = project(
                    *key,
                    previous.filter(|_| same_generation).map(|old| &old.value),
                )?;
                #[cfg(test)]
                {
                    *self.projections.entry(*key).or_default() += 1;
                    self.block_projections += value.projected_blocks;
                }
                replacements.insert(
                    *key,
                    PaneView {
                        stamp: stamp.clone(),
                        value,
                    },
                );
            }
            size += replacements
                .get(key)
                .or(self.panes.get(key))
                .expect("projected surface")
                .value
                .bytes;
        }
        let sections = sections();
        size += encoded_size(&sections)?;
        if size > MAX_BYTES {
            return Err(ApiError::Capacity);
        }
        let frame = if snapshot {
            let surfaces = stamps
                .keys()
                .map(|key| {
                    (
                        *key,
                        &replacements
                            .get(key)
                            .or(self.panes.get(key))
                            .expect("projected surface")
                            .value,
                    )
                })
                .collect();
            reservation.encode(&Snapshot {
                kind: "snapshot",
                frame_id: id.to_string(),
                view: View {
                    sections: &sections,
                    surfaces,
                },
            })?
        } else {
            let surfaces = replacements
                .iter()
                .filter_map(|(key, replacement)| {
                    let old = &self.panes[key].value;
                    (old != &replacement.value).then(|| pane_patch(*key, old, &replacement.value))
                })
                .collect();
            reservation.encode(&Patch {
                kind: "patch",
                frame_id: id.to_string(),
                base_frame_id: self.id.to_string(),
                sections: fields(&self.sections, &sections, &[]),
                surfaces,
            })?
        };
        self.panes.retain(|key, _| stamps.contains_key(key));
        self.panes.extend(replacements);
        self.id = id;
        self.sections = sections;
        Ok(frame)
    }
}
#[derive(serde::Serialize)]
struct Snapshot<'a> {
    kind: &'static str,
    frame_id: String,
    view: View<'a>,
}
struct View<'a> {
    sections: &'a Value,
    surfaces: BTreeMap<crate::SurfaceId, &'a CachedPane>,
}
impl serde::Serialize for View<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let sections = self.sections.as_object().expect("closed view sections");
        let mut view = serializer.serialize_map(Some(sections.len() + 1))?;
        for (key, value) in sections {
            view.serialize_entry(key, value)?;
        }
        view.serialize_entry("surfaces", &self.surfaces)?;
        view.end()
    }
}
#[derive(serde::Serialize)]
struct Patch<'a> {
    kind: &'static str,
    frame_id: String,
    base_frame_id: String,
    sections: BTreeMap<&'a str, &'a Value>,
    surfaces: Vec<PanePatch<'a>>,
}
#[derive(serde::Serialize)]
struct PanePatch<'a> {
    surface: crate::SurfaceId,
    fields: BTreeMap<&'a str, &'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transcript: Option<TranscriptPatch<'a>>,
}
#[derive(serde::Serialize)]
struct TranscriptPatch<'a> {
    fields: BTreeMap<&'a str, &'a Value>,
    upsert: Vec<&'a Value>,
    remove: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    order: Option<Vec<&'a str>>,
}
fn fields<'a>(before: &Value, after: &'a Value, excluded: &[&str]) -> BTreeMap<&'a str, &'a Value> {
    after
        .as_object()
        .expect("closed view object")
        .iter()
        .filter(|(key, value)| {
            !excluded.contains(&key.as_str()) && before.get(*key) != Some(*value)
        })
        .map(|(key, value)| (key.as_str(), value))
        .collect()
}
fn pane_patch<'a>(
    surface: crate::SurfaceId,
    before: &'a CachedPane,
    after: &'a CachedPane,
) -> PanePatch<'a> {
    let transcript = match (&before.transcript, &after.transcript) {
        (Some(old), Some(new)) if old != new => {
            let old_blocks: BTreeMap<_, _> = old
                .blocks
                .iter()
                .map(|block| (block.key(), block))
                .collect();
            let new_keys: Vec<_> = new
                .blocks
                .iter()
                .map(frame_view::CachedBlock::key)
                .collect();
            let old_keys: Vec<_> = old
                .blocks
                .iter()
                .map(frame_view::CachedBlock::key)
                .collect();
            let membership: BTreeSet<_> = new_keys.iter().copied().collect();
            let upsert = new
                .blocks
                .iter()
                .filter(|block| old_blocks.get(block.key()).copied() != Some(*block))
                .map(|block| block.value.as_ref())
                .collect();
            let remove = old_keys
                .iter()
                .copied()
                .filter(|key| !membership.contains(key))
                .collect();
            Some(TranscriptPatch {
                fields: fields(&old.metadata, &new.metadata, &["blocks"]),
                upsert,
                remove,
                order: (old_keys != new_keys).then_some(new_keys),
            })
        }
        _ => None,
    };
    PanePatch {
        surface,
        fields: fields(&before.metadata, &after.metadata, &["transcript"]),
        transcript,
    }
}
fn frame_id(text: &str) -> Result<u64> {
    if text.len() > 20 {
        return Err(ApiError::Invalid("Invalid Web frame ID".into()));
    }
    let id: u64 = text
        .parse()
        .map_err(|_| ApiError::Invalid("Invalid Web frame ID".into()))?;
    if id == 0 || id.to_string() != text {
        return Err(ApiError::Invalid("Invalid Web frame ID".into()));
    }
    Ok(id)
}
fn encoded_size(value: &impl serde::Serialize) -> Result<usize> {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_BYTES - self.0 {
                return Err(std::io::Error::other("presentation baseline capacity"));
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Counter(0);
    serde_json::to_writer(&mut count, value).map_err(|_| ApiError::Capacity)?;
    Ok(count.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stamps() -> BTreeMap<crate::SurfaceId, PaneStamp> {
        [crate::SurfaceId::MAIN, crate::SurfaceId::COMPARE]
            .into_iter()
            .map(|key| {
                (
                    key,
                    PaneStamp {
                        generation: Some(1),
                        pane: Arc::new(()),
                        renderer: Some(Arc::new(())),
                        ui: 1,
                    },
                )
            })
            .collect()
    }
    fn pane(index: crate::SurfaceId) -> Value {
        json!({"generation":"1","session":format!("session-{index}"),"draft":"","transcript":{"blocks":[{"key":"a","text":"first"},{"key":"b","text":"second"}],"status":"Ready"}})
    }
    fn decoded(bytes: RetainedBytes) -> Value {
        let value = serde_json::from_slice(bytes.as_bytes()).unwrap();
        drop(bytes);
        value
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "One observable lifecycle with shared setup and assertions"
    )]
    fn frames_reuse_unchanged_panes_and_patch_only_changed_blocks() {
        let budget = ByteBudget::new(MAX_BYTES).unwrap();
        let mut frames = FrameState::default();
        let mut stamps = stamps();
        let first = decoded(
            frames
                .capture(&budget, None, &stamps, || json!({"notice":""}), pane)
                .unwrap(),
        );
        assert_eq!(first["kind"], "snapshot");
        assert_eq!(
            frames.projections,
            BTreeMap::from([(crate::SurfaceId::MAIN, 1), (crate::SurfaceId::COMPARE, 1)])
        );
        let unchanged = decoded(
            frames
                .capture(
                    &budget,
                    Some("1"),
                    &stamps,
                    || json!({"notice":"status only"}),
                    |_| panic!("unchanged pane was encoded"),
                )
                .unwrap(),
        );
        assert_eq!(unchanged["kind"], "patch");
        assert_eq!(unchanged["surfaces"], json!([]));
        assert_eq!(unchanged["sections"], json!({"notice":"status only"}));
        stamps.get_mut(&crate::SurfaceId::MAIN).unwrap().renderer = Some(Arc::new(()));
        let changed = decoded(
            frames
                .capture(
                    &budget,
                    Some("2"),
                    &stamps,
                    || json!({"notice":"status only"}),
                    |index| {
                        assert_eq!(index, crate::SurfaceId::MAIN, "other pane is not encoded");
                        let mut pane = pane(index);
                        pane["transcript"]["blocks"][1]["text"] = json!("second updated");
                        pane
                    },
                )
                .unwrap(),
        );
        assert_eq!(
            frames.projections,
            BTreeMap::from([(crate::SurfaceId::MAIN, 2), (crate::SurfaceId::COMPARE, 1)])
        );
        assert_eq!(
            changed["surfaces"],
            json!([{"surface":"main","fields":{},"transcript":{"fields":{},"upsert":[{"key":"b","text":"second updated"}],"remove":[]}}])
        );
        stamps.get_mut(&crate::SurfaceId::MAIN).unwrap().pane = Arc::new(());
        let reorder = decoded(
            frames
                .capture(
                    &budget,
                    Some("3"),
                    &stamps,
                    || json!({"notice":"status only"}),
                    |_| {
                        let mut pane = pane(crate::SurfaceId::MAIN);
                        pane["transcript"]["blocks"] =
                            json!([{"key":"c","text":"new"},{"key":"b","text":"second updated"}]);
                        pane
                    },
                )
                .unwrap(),
        );
        assert_eq!(
            reorder["surfaces"][0]["transcript"],
            json!({"fields":{},"upsert":[{"key":"c","text":"new"}],"remove":["a"],"order":["c","b"]})
        );
        let mismatch = decoded(
            frames
                .capture(
                    &budget,
                    Some("1"),
                    &stamps,
                    || json!({"notice":"status only"}),
                    |_| panic!("resync reuses its projection"),
                )
                .unwrap(),
        );
        assert_eq!(mismatch["kind"], "snapshot");
        assert_eq!(mismatch["frame_id"], "5");
        stamps
            .get_mut(&crate::SurfaceId::COMPARE)
            .unwrap()
            .generation = Some(2);
        let generation = decoded(
            frames
                .capture(
                    &budget,
                    Some("5"),
                    &stamps,
                    || json!({"notice":"status only"}),
                    |index| {
                        assert_eq!(index, crate::SurfaceId::COMPARE);
                        let mut value = pane(index);
                        value["generation"] = json!("2");
                        value
                    },
                )
                .unwrap(),
        );
        assert_eq!(generation["kind"], "snapshot");
        assert_eq!(generation["view"]["surfaces"]["compare"]["generation"], "2");
    }
    #[test]
    fn frame_admission_and_baseline_failure_do_not_advance_sequence() {
        let budget = ByteBudget::new(MAX_BYTES).unwrap();
        let mut frames = FrameState::default();
        let stamps = stamps();
        let retained = frames
            .capture(&budget, None, &stamps, || json!({}), pane)
            .unwrap();
        assert!(matches!(
            frames.capture(
                &budget,
                Some("1"),
                &stamps,
                || json!({}),
                |_| unreachable!()
            ),
            Err(ApiError::Capacity)
        ));
        assert_eq!(frames.id, 1);
        drop(retained);
        for invalid in ["0", "01", "+1", "-1", "18446744073709551616"] {
            assert!(matches!(
                frames.capture(
                    &budget,
                    Some(invalid),
                    &stamps,
                    || unreachable!(),
                    |_| unreachable!()
                ),
                Err(ApiError::Invalid(_))
            ));
        }
        let excessive = frames.capture(
            &budget,
            Some("1"),
            &stamps,
            || json!({"notice":"x".repeat(MAX_BYTES)}),
            |_| unreachable!(),
        );
        assert!(matches!(excessive, Err(ApiError::Capacity)));
        assert_eq!(frames.id, 1);
        assert_eq!(frames.sections, json!({}));
        let recovered = decoded(
            frames
                .capture(
                    &budget,
                    Some("1"),
                    &stamps,
                    || json!({}),
                    |_| unreachable!(),
                )
                .unwrap(),
        );
        assert_eq!(recovered["frame_id"], "2");
        assert_eq!(recovered["surfaces"], json!([]));
    }
}

#[cfg(test)]
#[path = "frame_cache_tests.rs"]
mod cache_tests;

#[cfg(test)]
#[path = "frame_allocations.rs"]
mod allocations;
