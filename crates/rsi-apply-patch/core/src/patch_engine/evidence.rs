//! Optional presentation data derived exclusively from committed preflight bytes.

use super::{PatchEffect, PatchEffectKind, PreparedOperation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub(crate) const MAXIMUM_EVIDENCE_BYTES: usize = 32 * 1024;
const MAXIMUM_OPERATION_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchEvidence {
    pub(super) version: u32,
    pub omitted: bool,
    pub diffs: Vec<EffectDiff>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EffectDiff {
    /// Index in the committed effect ledger, including directory entries.
    pub effect: usize,
    pub unified_diff: String,
}

impl Default for PatchEvidence {
    fn default() -> Self {
        Self {
            version: 1,
            omitted: false,
            diffs: Vec::new(),
        }
    }
}

impl PatchEvidence {
    pub fn validate(&self, effects: &[PatchEffect], allowance: usize) -> Result<(), String> {
        let encoded = serde_json::to_vec(self)
            .map_err(|error| error.to_string())?
            .len();
        if self.version != 1 || encoded > MAXIMUM_EVIDENCE_BYTES {
            return Err("unsupported or oversized patch evidence".into());
        }
        // The empty envelope is mandatory metadata, even with a zero allowance.
        if !self.diffs.is_empty() && encoded > allowance {
            return Err("patch evidence exceeds its invocation allowance".into());
        }
        let mut previous = None;
        let mut per_operation = BTreeMap::<usize, usize>::new();
        for diff in &self.diffs {
            let effect = effects
                .get(diff.effect)
                .ok_or("patch evidence refers to an absent effect")?;
            if previous.is_some_and(|index| index >= diff.effect)
                || effect.kind == PatchEffectKind::Mkdir
                || diff.unified_diff.is_empty()
                || !display_safe(&diff.unified_diff)
            {
                return Err("patch evidence is unordered, duplicated or invalid".into());
            }
            previous = Some(diff.effect);
            let bytes = per_operation.entry(effect.operation).or_default();
            *bytes += serde_json::to_vec(diff)
                .map_err(|error| error.to_string())?
                .len()
                + 1;
            if *bytes > MAXIMUM_OPERATION_BYTES {
                return Err("patch operation evidence exceeds its byte bound".into());
            }
        }
        let file_effects = effects
            .iter()
            .filter(|effect| effect.kind != PatchEffectKind::Mkdir)
            .count();
        if !self.omitted && self.diffs.len() != file_effects {
            return Err("patch evidence omitted an effect without its omission marker".into());
        }
        Ok(())
    }

    pub(super) fn build(
        prepared: &[PreparedOperation],
        effects: &[PatchEffect],
        allowance: usize,
    ) -> Self {
        let allowance = allowance.min(MAXIMUM_EVIDENCE_BYTES);
        let mut result = Self::default();
        let mut per_operation = BTreeMap::<usize, usize>::new();
        for (index, effect) in effects.iter().enumerate() {
            if effect.kind == PatchEffectKind::Mkdir {
                continue;
            }
            let Some(operation) = prepared.get(effect.operation) else {
                result.omitted = true;
                continue;
            };
            let (before, after) = match effect.kind {
                PatchEffectKind::Add | PatchEffectKind::MoveWrite => {
                    (None, operation.content.as_deref())
                }
                PatchEffectKind::Update => {
                    (operation.expected.as_deref(), operation.content.as_deref())
                }
                PatchEffectKind::Delete | PatchEffectKind::MoveDelete => {
                    (operation.expected.as_deref(), None)
                }
                PatchEffectKind::Mkdir => unreachable!(),
            };
            let remaining =
                MAXIMUM_OPERATION_BYTES - *per_operation.get(&effect.operation).unwrap_or(&0);
            let Some(unified_diff) =
                unified_diff(&effect.path, before, after, remaining.min(allowance))
            else {
                result.omitted = true;
                continue;
            };
            let diff = EffectDiff {
                effect: index,
                unified_diff,
            };
            let bytes = serde_json::to_vec(&diff)
                .expect("bounded evidence serializes")
                .len()
                + 1;
            if bytes > remaining {
                result.omitted = true;
                continue;
            }
            result.diffs.push(diff);
            // false is one byte longer than true, so a later omission cannot overflow.
            if serde_json::to_vec(&result)
                .expect("bounded evidence serializes")
                .len()
                > allowance
            {
                result.diffs.pop();
                result.omitted = true;
            } else {
                *per_operation.entry(effect.operation).or_default() += bytes;
            }
        }
        debug_assert!(result.validate(effects, allowance).is_ok());
        result
    }
}

fn display_safe(text: &str) -> bool {
    !text
        .chars()
        .any(|c| (c <= '\u{1f}' && !matches!(c, '\t' | '\n' | '\r')) || c == '\u{7f}')
}

/// Linear prefix/suffix matching avoids quadratic diff work on adversarial files.
/// A complete replacement hunk is omitted as a unit if it cannot fit.
fn unified_diff(
    path: &str,
    before: Option<&[u8]>,
    after: Option<&[u8]>,
    maximum: usize,
) -> Option<String> {
    let old = std::str::from_utf8(before.unwrap_or_default()).ok()?;
    let new = std::str::from_utf8(after.unwrap_or_default()).ok()?;
    if !display_safe(old) || !display_safe(new) || !display_safe(path) {
        return None;
    }
    let mut prefix = 0;
    for (a, b) in old.split_inclusive('\n').zip(new.split_inclusive('\n')) {
        if a != b {
            break;
        }
        prefix += a.len();
    }
    let mut suffix = 0;
    for (a, b) in old[prefix..]
        .split_inclusive('\n')
        .rev()
        .zip(new[prefix..].split_inclusive('\n').rev())
    {
        if a != b {
            break;
        }
        suffix += a.len();
    }
    let leading = old[..prefix]
        .split_inclusive('\n')
        .rev()
        .take(3)
        .map(str::len)
        .sum::<usize>();
    let trailing = old[old.len() - suffix..]
        .split_inclusive('\n')
        .take(3)
        .map(str::len)
        .sum::<usize>();
    let start = prefix - leading;
    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    let old_count = old[start..old_end + trailing].split_inclusive('\n').count();
    let new_count = new[start..new_end + trailing].split_inclusive('\n').count();
    let line = old[..start].split_inclusive('\n').count() + 1;
    // Check raw bytes before allocating output. Encoded metadata/escaping is checked by the caller.
    let size = (old_end - prefix)
        + (new_end - prefix)
        + leading
        + trailing
        + old_count
        + new_count
        + 128
        + path.len() * 2;
    if size > maximum {
        return None;
    }
    let mut out = String::with_capacity(size);
    writeln!(
        out,
        "--- {}\n+++ {}",
        if before.is_some() { path } else { "/dev/null" },
        if after.is_some() { path } else { "/dev/null" }
    )
    .ok()?;
    writeln!(
        out,
        "@@ -{},{old_count} +{},{new_count} @@",
        if old_count == 0 { line - 1 } else { line },
        if new_count == 0 { line - 1 } else { line }
    )
    .ok()?;
    for (mark, text) in [
        (' ', &old[start..prefix]),
        ('-', &old[prefix..old_end]),
        ('+', &new[prefix..new_end]),
        (' ', &old[old_end..old_end + trailing]),
    ] {
        for line in text.split_inclusive('\n') {
            out.push(mark);
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push_str("\n\\ No newline at end of file\n");
            }
            if out.len() > maximum {
                return None;
            }
        }
    }
    Some(out)
}
