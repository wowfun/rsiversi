use super::super::super::output_records::{OutputRecord, size_u32};
use super::super::super::values::{CapValue, InstanceCell};
use super::super::super::{PluginRuntime, lock, wire_io};
use super::super::common;
use super::super::common::read_cap;
use crate::{
    CAP_KIND_INSTANCE, CAP_KIND_PREPARED, CapOutput, NativePlugin, RIGHT_MUTATE, RIGHT_RETAIN,
    STATUS_FAILED, STATUS_PROTOCOL_ERROR,
};
use core::ffi::c_void;
use std::sync::Arc;

pub(in crate::sdk::runtime::operations) fn create<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    if let Err(status) = wire_io::check_output::<CapOutput>(output, capacity) {
        return status;
    }
    let Ok(input) = read_cap(input, input_size) else {
        return super::invalid(
            runtime,
            "create input",
            size_u32::<CapOutput>(),
            output,
            capacity,
        );
    };
    let prepared = match runtime
        .caps()
        .get(input.capability, CAP_KIND_PREPARED, RIGHT_MUTATE)
    {
        Ok(CapValue::Prepared(value)) => value,
        Ok(_) => unreachable!("cap table kind matches value"),
        Err(status) => {
            return common::status_error(
                runtime,
                status,
                "create",
                size_u32::<CapOutput>(),
                output,
                capacity,
            );
        }
    };
    let Some(state) = lock(&prepared.state).take() else {
        return common::write_error(
            runtime,
            STATUS_PROTOCOL_ERROR,
            "prepared capability was already consumed".to_owned(),
            size_u32::<CapOutput>(),
            output,
            capacity,
        );
    };
    let instance = match prepared.factory.plugin.create(state) {
        Ok(value) => value,
        Err(error) => {
            return common::write_error(
                runtime,
                STATUS_FAILED,
                error,
                size_u32::<CapOutput>(),
                output,
                capacity,
            );
        }
    };
    let value = CapValue::Instance(Arc::new(InstanceCell::new(
        instance,
        prepared.requirements.clone(),
    )));
    let capability = match runtime.insert_cap(CAP_KIND_INSTANCE, RIGHT_RETAIN | RIGHT_MUTATE, value)
    {
        Ok(value) => value,
        Err(status) => {
            return common::status_error(
                runtime,
                status,
                "instance capability",
                size_u32::<CapOutput>(),
                output,
                capacity,
            );
        }
    };
    write_capability(runtime, capability, output, capacity)
}

fn write_capability<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    capability: crate::CapId,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    let mut outputs = runtime.outputs();
    let release = match outputs.insert(OutputRecord::capability(capability)) {
        Ok(value) => value,
        Err(status) => {
            drop(outputs);
            if let Err(drop_status) = runtime.release_owned_cap(capability) {
                return drop_status;
            }
            return status;
        }
    };
    let value = outputs.get(release).cap_output(release);
    // SAFETY: Caller-provided output was validated before ownership allocation.
    match unsafe { wire_io::write_output(output, capacity, value) } {
        Ok(()) => crate::STATUS_OK,
        Err(status) => status,
    }
}
