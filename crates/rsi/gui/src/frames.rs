//! One bounded presentation baseline, independent of durable replay cursors.
use crate::GuiApplication;
use rsi_api_protocol::{ApiError, ByteBudget, Result, RetainedBytes};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, io::Write, sync::Arc};
const MAX_BYTES: usize = 32 * 1024 * 1024;

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
    value: Value,
    bytes: usize,
}
#[derive(Debug, Default)]
pub(crate) struct FrameState {
    pub(crate) id: u64,
    panes: BTreeMap<crate::SurfaceId, PaneView>,
    sections: Value,
    #[cfg(test)]
    projections: BTreeMap<crate::SurfaceId, usize>,
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
        self.capture(
            budget,
            base,
            &stamps,
            || app.sections(),
            |key| surfaces[&key].view(&app.ui),
        )
    }
    fn capture(
        &mut self,
        budget: &ByteBudget,
        base: Option<&str>,
        stamps: &BTreeMap<crate::SurfaceId, PaneStamp>,
        sections: impl FnOnce() -> Value,
        mut project: impl FnMut(crate::SurfaceId) -> Value,
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
                snapshot |= self
                    .panes
                    .get(key)
                    .is_some_and(|old| old.stamp.generation != stamp.generation);
                let value = project(*key);
                let bytes = encoded_size(&value)?;
                replacements.insert(
                    *key,
                    PaneView {
                        stamp: stamp.clone(),
                        value,
                        bytes,
                    },
                );
                #[cfg(test)]
                {
                    *self.projections.entry(*key).or_default() += 1;
                }
            }
            size += replacements
                .get(key)
                .or(self.panes.get(key))
                .expect("projected surface")
                .bytes;
        }
        let sections = sections();
        size += encoded_size(&sections)?;
        if size > MAX_BYTES {
            return Err(ApiError::Capacity);
        }
        let wire = if snapshot {
            let mut view = sections.clone();
            view.as_object_mut().expect("closed sections").insert(
                "surfaces".into(),
                Value::Object(
                    stamps
                        .keys()
                        .map(|key| {
                            (
                                key.to_string(),
                                replacements
                                    .get(key)
                                    .or(self.panes.get(key))
                                    .expect("projected surface")
                                    .value
                                    .clone(),
                            )
                        })
                        .collect(),
                ),
            );
            json!({"kind":"snapshot","frame_id":id.to_string(),"view":view})
        } else {
            let surfaces: Vec<_> = replacements
                .iter()
                .filter_map(|(key, replacement)| {
                    let old = &self.panes[key].value;
                    (old != &replacement.value).then(|| pane_patch(*key, old, &replacement.value))
                })
                .collect();
            json!({"kind":"patch","frame_id":id.to_string(),"base_frame_id":self.id.to_string(),"sections":fields(&self.sections,&sections,&[]),"surfaces":surfaces})
        };
        let frame = reservation.encode(&wire)?;
        self.panes.retain(|key, _| stamps.contains_key(key));
        self.panes.extend(replacements);
        self.id = id;
        self.sections = sections;
        Ok(frame)
    }
}

fn frame_id(text: &str) -> Result<u64> {
    if text.len() > 20 {
        return Err(ApiError::Invalid("Invalid Web frame ID".into()));
    }
    let id: u64 = text
        .parse()
        .map_err(|_| ApiError::Invalid("Invalid Web frame ID".into()))?;
    if text.len() > 20 || id == 0 || id.to_string() != text {
        return Err(ApiError::Invalid("Invalid Web frame ID".into()));
    }
    Ok(id)
}
fn fields(before: &Value, after: &Value, excluded: &[&str]) -> Map<String, Value> {
    after
        .as_object()
        .expect("closed view object")
        .iter()
        .filter(|(key, value)| {
            !excluded.contains(&key.as_str()) && before.get(*key) != Some(*value)
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}
fn pane_patch(index: crate::SurfaceId, before: &Value, after: &Value) -> Value {
    let mut patch = json!({"surface":index,"fields":fields(before,after,&["transcript"])});
    let (old, new) = (&before["transcript"], &after["transcript"]);
    if old != new {
        let old_blocks: BTreeMap<_, _> = old["blocks"]
            .as_array()
            .expect("baseline blocks")
            .iter()
            .map(|block| (block["key"].as_str().expect("block key"), block))
            .collect();
        let new_blocks = new["blocks"].as_array().expect("current blocks");
        let new_keys: Vec<_> = new_blocks
            .iter()
            .map(|block| block["key"].as_str().expect("block key"))
            .collect();
        let old_keys: Vec<_> = old["blocks"]
            .as_array()
            .expect("baseline blocks")
            .iter()
            .map(|block| block["key"].as_str().expect("block key"))
            .collect();
        let upsert: Vec<_> = new_blocks
            .iter()
            .filter(|block| {
                old_blocks
                    .get(block["key"].as_str().expect("block key"))
                    .copied()
                    != Some(*block)
            })
            .collect();
        let remove: Vec<_> = old_keys
            .iter()
            .filter(|key| !new_keys.contains(key))
            .collect();
        patch["transcript"] =
            json!({"fields":fields(old,new,&["blocks"]),"upsert":upsert,"remove":remove});
        if old_keys != new_keys {
            patch["transcript"]["order"] = json!(new_keys);
        }
    }
    patch
}
fn encoded_size(value: &Value) -> Result<usize> {
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
