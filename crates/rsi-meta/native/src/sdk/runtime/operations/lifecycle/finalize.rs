use super::super::super::output_records::size_u32;
use super::super::super::{PluginRuntime, wire_io};
use super::super::{Dispatch, common};
use crate::{
    BasicOutput, EmptyInput, NativePlugin, STATUS_INVALID_ARGUMENT, STATUS_OK,
    STATUS_PROTOCOL_ERROR,
};
use core::ffi::c_void;

pub(in crate::sdk::runtime::operations) fn finalize<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    capacity: u32,
) -> Dispatch {
    if let Err(status) = wire_io::check_output::<BasicOutput>(output, capacity) {
        return Dispatch::status(status);
    }
    // SAFETY: `read_input` validates the exact empty frame before copying.
    let Ok(input) = (unsafe { wire_io::read_input::<EmptyInput>(input, input_size) }) else {
        return Dispatch::status(common::write_error(
            runtime,
            STATUS_INVALID_ARGUMENT,
            "invalid finalize input".to_owned(),
            size_u32::<BasicOutput>(),
            output,
            capacity,
        ));
    };
    if wire_io::validate_header(input.header, size_u32::<EmptyInput>(), input_size).is_err() {
        return Dispatch::status(common::write_error(
            runtime,
            STATUS_INVALID_ARGUMENT,
            "invalid finalize input".to_owned(),
            size_u32::<BasicOutput>(),
            output,
            capacity,
        ));
    }
    let Some(fence) = runtime.exchanges.close_if_sole() else {
        return Dispatch::status(common::write_error(
            runtime,
            STATUS_PROTOCOL_ERROR,
            "another plugin exchange is still admitted".to_owned(),
            size_u32::<BasicOutput>(),
            output,
            capacity,
        ));
    };
    let ready = runtime.callback_refs.is_empty()
        && runtime.caps().can_finalize()
        && runtime.outputs().is_empty();
    if !ready {
        return Dispatch::status(common::write_error(
            runtime,
            STATUS_PROTOCOL_ERROR,
            "plugin table still owns capabilities, outputs, callbacks, or exchanges".to_owned(),
            size_u32::<BasicOutput>(),
            output,
            capacity,
        ));
    }
    let status = match common::write_basic(output, capacity) {
        Ok(()) => STATUS_OK,
        Err(status) => status,
    };
    let dispatch = Dispatch {
        status,
        finalize: status == STATUS_OK,
    };
    if dispatch.finalize {
        fence.commit();
    }
    dispatch
}
