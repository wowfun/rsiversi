use super::super::output_records::size_u32;
use super::super::{PluginRuntime, wire_io};
use super::common;
use crate::{
    BasicOutput, CAP_KIND_INSTANCE, NativePlugin, ReleaseOutputInput, STATUS_INVALID_ARGUMENT,
    STATUS_OK,
};
use core::ffi::c_void;

mod cleanup;
mod finalize;

pub(super) use cleanup::run_cleanup;
pub(super) use finalize::finalize;

pub(super) fn retain<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    mutate_cap(
        runtime,
        "cap retain",
        input,
        input_size,
        output,
        capacity,
        |runtime, id| runtime.caps().retain(id),
    )
}

pub(super) fn release<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    mutate_cap(
        runtime,
        "cap release",
        input,
        input_size,
        output,
        capacity,
        PluginRuntime::release_external_cap,
    )
}

pub(super) fn destroy<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    kind: u32,
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    mutate_cap(
        runtime,
        "cap destroy",
        input,
        input_size,
        output,
        capacity,
        |runtime, id| {
            if kind == CAP_KIND_INSTANCE {
                runtime.destroy_instance_cap(id)
            } else {
                runtime.destroy_cap(id, kind)
            }
        },
    )
}

fn mutate_cap<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    operation: &str,
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    capacity: u32,
    mutation: impl FnOnce(&PluginRuntime<P>, crate::CapId) -> Result<(), u32>,
) -> u32 {
    if let Err(status) = wire_io::check_output::<BasicOutput>(output, capacity) {
        return status;
    }
    let input = match common::read_cap(input, input_size) {
        Ok(value) => value,
        Err(status) => {
            return common::status_error(
                runtime,
                status,
                operation,
                size_u32::<BasicOutput>(),
                output,
                capacity,
            );
        }
    };
    if let Err(status) = mutation(runtime, input.capability) {
        return common::status_error(
            runtime,
            status,
            operation,
            size_u32::<BasicOutput>(),
            output,
            capacity,
        );
    }
    match common::write_basic(output, capacity) {
        Ok(()) => STATUS_OK,
        Err(status) => status,
    }
}

pub(super) fn release_output<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    input: *const c_void,
    input_size: u32,
) -> u32 {
    // SAFETY: `read_input` validates the exact release frame before copying.
    let Ok(input) = (unsafe { wire_io::read_input::<ReleaseOutputInput>(input, input_size) })
    else {
        return STATUS_INVALID_ARGUMENT;
    };
    if wire_io::validate_header(input.header, size_u32::<ReleaseOutputInput>(), input_size).is_err()
    {
        return STATUS_INVALID_ARGUMENT;
    }
    let record = match runtime.outputs().release(input.release) {
        Ok(record) => record,
        Err(status) => return status,
    };
    let mut drop_status = STATUS_OK;
    for capability in record.held_capabilities {
        if let Err(status) = runtime.release_owned_cap(capability) {
            drop_status = status;
        }
    }
    drop_status
}
