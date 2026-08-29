use super::super::super::gates::FactoryGate;
use super::super::super::output_records::{OutputRecord, size_u32};
use super::super::super::values::CapValue;
use super::super::super::{PluginRuntime, wire_io};
use super::super::common;
use super::super::common::read_cap;
use super::{MAX_IDENTITY_BYTES, invalid};
use crate::{
    BytesOutput, CAP_KIND_FACTORY, NativePlugin, RIGHT_MUTATE, STATUS_FAILED,
    STATUS_LIMIT_EXCEEDED, STATUS_PROTOCOL_ERROR,
};
use core::ffi::c_void;

pub(in crate::sdk::runtime::operations) fn identity<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    if let Err(status) = wire_io::check_output::<BytesOutput>(output, capacity) {
        return status;
    }
    let Ok(input) = read_cap(input, input_size) else {
        return invalid(
            runtime,
            "identity input",
            size_u32::<BytesOutput>(),
            output,
            capacity,
        );
    };
    let factory = match runtime
        .caps()
        .get(input.capability, CAP_KIND_FACTORY, RIGHT_MUTATE)
    {
        Ok(CapValue::Factory(factory)) => factory,
        Ok(_) => unreachable!("cap table kind matches value"),
        Err(status) => {
            return common::status_error(
                runtime,
                status,
                "identity",
                size_u32::<BytesOutput>(),
                output,
                capacity,
            );
        }
    };
    let Ok(_gate) = FactoryGate::acquire(&factory) else {
        return common::status_error(
            runtime,
            crate::STATUS_BUSY,
            "identity",
            size_u32::<BytesOutput>(),
            output,
            capacity,
        );
    };
    let bytes = match factory.plugin.identity() {
        Ok(identity) if !identity.is_empty() && identity.len() <= MAX_IDENTITY_BYTES => {
            identity.into_bytes()
        }
        Ok(_) => {
            return common::write_error(
                runtime,
                STATUS_PROTOCOL_ERROR,
                "plugin identity is empty or oversized".to_owned(),
                size_u32::<BytesOutput>(),
                output,
                capacity,
            );
        }
        Err(error) => {
            return common::write_error(
                runtime,
                STATUS_FAILED,
                error,
                size_u32::<BytesOutput>(),
                output,
                capacity,
            );
        }
    };
    let mut outputs = runtime.outputs();
    let Ok(release) = outputs.insert(OutputRecord::bytes(bytes)) else {
        return STATUS_LIMIT_EXCEEDED;
    };
    let value = outputs.get(release).bytes_output(release);
    // SAFETY: The output range was validated before plugin code.
    match unsafe { wire_io::write_output(output, capacity, value) } {
        Ok(()) => crate::STATUS_OK,
        Err(status) => status,
    }
}
