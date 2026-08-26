use super::super::PluginRuntime;
use super::super::output_records::size_u32;
use super::common;
use crate::{BasicOutput, NativePlugin, STATUS_INVALID_ARGUMENT};
use core::ffi::c_void;

mod activate;
mod injections;
mod serve;

pub(super) use activate::activate;
pub(super) use serve::serve;

pub(super) const MAX_PORT_BYTES: usize = 4_096;
pub(super) const MAX_INJECTIONS: usize = 1_024;

pub(super) fn invalid<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    what: &str,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    common::write_error(
        runtime,
        STATUS_INVALID_ARGUMENT,
        format!("invalid {what}"),
        size_u32::<BasicOutput>(),
        output,
        capacity,
    )
}

pub(super) fn rejected<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    status: u32,
    what: &str,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    common::status_error(
        runtime,
        status,
        what,
        size_u32::<BasicOutput>(),
        output,
        capacity,
    )
}
