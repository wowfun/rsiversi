//! DSH-compatible single-pass variable interpolation and presentation ordering.

use crate::{PresentedText, invalid};
use rsi_agent_composition_protocol::ContributionResult;
use std::collections::{BTreeMap, BTreeSet};

/// Position for admitted names not explicitly listed in the presentation order.
pub const TOOL_ORDER_REST: &str = "<unlisted-tools>";

pub(crate) fn variable_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

pub(crate) fn names(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

pub(crate) fn interpolate(
    section: &mut PresentedText,
    variables: &BTreeMap<String, Option<String>>,
) -> ContributionResult<String> {
    if !section.interpolate || !section.text.contains("{{") {
        return Ok(std::mem::take(&mut section.text));
    }
    let mut result = String::with_capacity(section.text.len());
    let mut rest = section.text.as_str();
    while let Some(open) = rest.find("{{") {
        result.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            result.push_str(&rest[open..]);
            return Ok(result);
        };
        let name = &after[..close];
        if !variable_name(name) {
            return Err(invalid(format!(
                "malformed prompt variable in {}",
                section.name
            )));
        }
        let value = variables.get(name).ok_or_else(|| {
            invalid(format!(
                "unknown prompt variable {name} in {}",
                section.name
            ))
        })?;
        let value = value.as_ref().ok_or_else(|| {
            invalid(format!(
                "unresolved prompt variable {name} in {}",
                section.name
            ))
        })?;
        result.push_str(value);
        rest = &after[close + 2..];
    }
    result.push_str(rest);
    Ok(result)
}

pub(crate) fn validate_order(order: Option<&[String]>) -> ContributionResult<()> {
    let Some(order) = order else { return Ok(()) };
    let mut seen = BTreeSet::new();
    for name in order {
        if !seen.insert(name.as_str()) {
            return Err(invalid(format!("duplicate Tool presentation name {name}")));
        }
    }
    if !seen.contains(TOOL_ORDER_REST) {
        return Err(invalid("Tool presentation requires <unlisted-tools>"));
    }
    Ok(())
}

pub(crate) fn ordered_tools(
    admitted: impl Iterator<Item = String>,
    known: &[String],
    order: Option<&[String]>,
) -> ContributionResult<Vec<String>> {
    let mut admitted: Vec<_> = admitted.collect();
    if admitted.iter().any(|name| name == TOOL_ORDER_REST) {
        return Err(invalid(
            "reserved Tool presentation marker used as a Tool name",
        ));
    }
    admitted.sort_by(|a, b| names(a, b));
    let Some(order) = order else {
        return Ok(admitted);
    };
    for name in order {
        if name != TOOL_ORDER_REST && !known.contains(name) {
            return Err(invalid(format!(
                "unregistered Tool presentation name {name}"
            )));
        }
    }
    let mut result = Vec::with_capacity(admitted.len());
    for name in order {
        if name == TOOL_ORDER_REST {
            result.extend(
                admitted
                    .iter()
                    .filter(|name| !order.contains(name))
                    .cloned(),
            );
        } else if admitted.contains(name) {
            result.push(name.clone());
        }
    }
    Ok(result)
}
