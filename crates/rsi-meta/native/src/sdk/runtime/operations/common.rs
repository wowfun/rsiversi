use super::super::PluginRuntime;
use super::super::output_records::{OutputRecord, size_u32};
use super::super::wire_io;
use crate::{
    BasicOutput, BytesOutput, CapInput, CapOutput, NativePlugin, OutputPrefix, PLUGIN_CREATE,
    PLUGIN_IDENTITY, PLUGIN_PREPARE, PrepareOutput, STATUS_BUFFER_TOO_SMALL, STATUS_LIMIT_EXCEEDED,
};
use core::ffi::c_void;

pub(super) fn read_cap(input: *const c_void, input_size: u32) -> Result<CapInput, u32> {
    // SAFETY: `read_input` validates the exact raw frame before copying.
    let value = unsafe { wire_io::read_input::<CapInput>(input, input_size) }?;
    wire_io::validate_header(value.header, size_u32::<CapInput>(), input_size)?;
    Ok(value)
}

pub(super) fn expected_output_size(opcode: u32) -> u32 {
    match opcode {
        PLUGIN_IDENTITY => size_u32::<BytesOutput>(),
        PLUGIN_PREPARE => size_u32::<PrepareOutput>(),
        PLUGIN_CREATE => size_u32::<CapOutput>(),
        _ => size_u32::<BasicOutput>(),
    }
}

pub(super) fn write_error<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    status: u32,
    diagnostic: String,
    expected_size: u32,
    output: *mut c_void,
    output_capacity: u32,
) -> u32 {
    if output.is_null()
        || output_capacity < expected_size
        || !output.addr().is_multiple_of(align_of::<OutputPrefix>())
    {
        return STATUS_BUFFER_TOO_SMALL;
    }
    let mut outputs = runtime.outputs();
    let Ok(release) = outputs.insert(OutputRecord::diagnostic(diagnostic)) else {
        return STATUS_LIMIT_EXCEEDED;
    };
    let prefix = outputs.get(release).prefix(release, expected_size);
    // SAFETY: The checked output has at least the opcode's full declared size;
    // every output starts with this aligned prefix and the caller zeroed suffix.
    unsafe { output.cast::<OutputPrefix>().write(prefix) };
    status
}

pub(super) fn write_basic(output: *mut c_void, capacity: u32) -> Result<(), u32> {
    let value = BasicOutput {
        prefix: OutputPrefix::empty(size_u32::<BasicOutput>()),
    };
    // SAFETY: `write_output` validates the raw output range first.
    unsafe { wire_io::write_output(output, capacity, value) }
}

pub(super) fn status_error<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    status: u32,
    operation: &str,
    output_size: u32,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    write_error(
        runtime,
        status,
        format!("{operation} rejected with status {status}"),
        output_size,
        output,
        capacity,
    )
}
