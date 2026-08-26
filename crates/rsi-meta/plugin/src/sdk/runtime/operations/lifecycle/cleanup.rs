use super::super::super::output_records::size_u32;
use super::super::super::values::CapValue;
use super::super::super::{PluginRuntime, wire_io};
use super::super::common;
use super::super::common::read_cap;
use crate::{
    BasicOutput, CAP_KIND_CLEANUP, NativePlugin, RIGHT_MUTATE, STATUS_FAILED, STATUS_OK,
    STATUS_PROTOCOL_ERROR,
};
use core::ffi::c_void;

pub(in crate::sdk::runtime::operations) fn run_cleanup<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    if let Err(status) = wire_io::check_output::<BasicOutput>(output, capacity) {
        return status;
    }
    let input = match read_cap(input, input_size) {
        Ok(value) => value,
        Err(status) => {
            return common::status_error(
                runtime,
                status,
                "run cleanup",
                size_u32::<BasicOutput>(),
                output,
                capacity,
            );
        }
    };
    let cleanup = match runtime
        .caps()
        .get(input.capability, CAP_KIND_CLEANUP, RIGHT_MUTATE)
    {
        Ok(CapValue::Cleanup(value)) => value,
        Ok(_) => unreachable!("cap table kind matches value"),
        Err(status) => {
            return common::status_error(
                runtime,
                status,
                "run cleanup",
                size_u32::<BasicOutput>(),
                output,
                capacity,
            );
        }
    };
    let Some(_run) = cleanup.begin() else {
        return common::write_error(
            runtime,
            STATUS_PROTOCOL_ERROR,
            "cleanup capability was already consumed".to_owned(),
            size_u32::<BasicOutput>(),
            output,
            capacity,
        );
    };
    let cleanup = cleanup
        .take_action()
        .expect("pending cleanup capability owns one action");
    match cleanup() {
        Ok(()) => match common::write_basic(output, capacity) {
            Ok(()) => STATUS_OK,
            Err(status) => status,
        },
        Err(error) => common::write_error(
            runtime,
            STATUS_FAILED,
            error,
            size_u32::<BasicOutput>(),
            output,
            capacity,
        ),
    }
}
