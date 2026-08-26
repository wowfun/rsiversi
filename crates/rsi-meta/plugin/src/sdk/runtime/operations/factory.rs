use super::super::PluginRuntime;
use super::common;
use crate::{NativePlugin, STATUS_INVALID_ARGUMENT};
use core::ffi::c_void;

mod create;
mod identity;
mod prepare;

pub(super) use create::create;
pub(super) use identity::identity;
pub(super) use prepare::prepare;

pub(super) const MAX_CONFIG_BYTES: usize = 1_048_576;
pub(super) const MAX_IDENTITY_BYTES: usize = 4_096;
pub(super) const MAX_REQUIREMENTS: usize = 1_024;
pub(super) const MAX_REQUIREMENT_FIELD: usize = 4_096;

pub(super) fn invalid<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    what: &str,
    size: u32,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    common::write_error(
        runtime,
        STATUS_INVALID_ARGUMENT,
        format!("invalid {what}"),
        size,
        output,
        capacity,
    )
}

pub(super) fn valid_requirements(requirements: &[crate::ServiceRequirement]) -> bool {
    requirements.len() <= MAX_REQUIREMENTS
        && requirements.iter().all(|requirement| {
            !requirement.key.is_empty()
                && requirement.key.len() <= MAX_REQUIREMENT_FIELD
                && !requirement.contract.is_empty()
                && requirement.contract.len() <= MAX_REQUIREMENT_FIELD
        })
}
