use super::super::super::gates::FactoryGate;
use super::super::super::output_records::{OutputRecord, size_u32};
use super::super::super::values::{CapValue, FactoryCell, PreparedCell};
use super::super::super::{PluginRuntime, wire_io};
use super::super::common;
use super::{MAX_CONFIG_BYTES, invalid, valid_requirements};
use crate::{
    BytesInput, CAP_KIND_FACTORY, CAP_KIND_PREPARED, NativePlugin, PrepareOutput, RIGHT_MUTATE,
    RIGHT_RETAIN, STATUS_FAILED, STATUS_INVALID_ARGUMENT, STATUS_PROTOCOL_ERROR,
};
use core::ffi::c_void;
use std::sync::{Arc, Mutex};

pub(in crate::sdk::runtime::operations) fn prepare<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    if let Err(status) = wire_io::check_output::<PrepareOutput>(output, capacity) {
        return status;
    }
    // SAFETY: `read_input` validates the exact raw frame before copying it.
    let Ok(input) = (unsafe { wire_io::read_input::<BytesInput>(input, input_size) }) else {
        return invalid(
            runtime,
            "prepare input",
            size_u32::<PrepareOutput>(),
            output,
            capacity,
        );
    };
    if wire_io::validate_header(input.header, size_u32::<BytesInput>(), input_size).is_err() {
        return invalid(
            runtime,
            "prepare header",
            size_u32::<PrepareOutput>(),
            output,
            capacity,
        );
    }
    let factory = match runtime
        .caps()
        .get(input.receiver, CAP_KIND_FACTORY, RIGHT_MUTATE)
    {
        Ok(CapValue::Factory(factory)) => factory,
        Ok(_) => unreachable!("cap table kind matches value"),
        Err(status) => {
            return common::status_error(
                runtime,
                status,
                "prepare",
                size_u32::<PrepareOutput>(),
                output,
                capacity,
            );
        }
    };
    // SAFETY: The plugin is trusted to keep the validated native range readable
    // for this synchronous copy.
    let Ok(desired) = unsafe { wire_io::copy_bytes(input.bytes, MAX_CONFIG_BYTES) }
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|_| STATUS_INVALID_ARGUMENT))
    else {
        return invalid(
            runtime,
            "desired configuration",
            size_u32::<PrepareOutput>(),
            output,
            capacity,
        );
    };
    finish_prepare(runtime, factory, &desired, output, capacity)
}

fn finish_prepare<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    factory: Arc<FactoryCell<P>>,
    desired: &serde_json::Value,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    let Ok(gate) = FactoryGate::acquire(&factory) else {
        return common::status_error(
            runtime,
            crate::STATUS_BUSY,
            "prepare",
            size_u32::<PrepareOutput>(),
            output,
            capacity,
        );
    };
    let prepared = match factory.plugin.prepare(desired) {
        Ok(value) => value,
        Err(error) => {
            return common::write_error(
                runtime,
                STATUS_FAILED,
                error,
                size_u32::<PrepareOutput>(),
                output,
                capacity,
            );
        }
    };
    drop(gate);
    if !valid_requirements(&prepared.requirements) {
        return common::write_error(
            runtime,
            STATUS_PROTOCOL_ERROR,
            "prepared requirements exceed ABI bounds".to_owned(),
            size_u32::<PrepareOutput>(),
            output,
            capacity,
        );
    }
    let normalized = match serde_json::to_vec(&prepared.normalized_config) {
        Ok(bytes) if bytes.len() <= MAX_CONFIG_BYTES => bytes,
        _ => {
            return common::write_error(
                runtime,
                STATUS_PROTOCOL_ERROR,
                "normalized configuration exceeds ABI bounds".to_owned(),
                size_u32::<PrepareOutput>(),
                output,
                capacity,
            );
        }
    };
    let retained_bytes = prepared.retained_bytes;
    let requirements = prepared.requirements;
    let cell = PreparedCell {
        factory,
        state: Mutex::new(Some(prepared.state)),
        requirements: requirements.clone(),
    };
    let capability = match runtime.insert_cap(
        CAP_KIND_PREPARED,
        RIGHT_RETAIN | RIGHT_MUTATE,
        CapValue::Prepared(Arc::new(cell)),
    ) {
        Ok(value) => value,
        Err(status) => {
            return common::status_error(
                runtime,
                status,
                "prepare capability",
                size_u32::<PrepareOutput>(),
                output,
                capacity,
            );
        }
    };
    write_prepare(
        runtime,
        capability,
        normalized,
        &requirements,
        retained_bytes,
        output,
        capacity,
    )
}

fn write_prepare<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    capability: crate::CapId,
    normalized: Vec<u8>,
    requirements: &[crate::ServiceRequirement],
    retained_bytes: u64,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    let mut outputs = runtime.outputs();
    let release = match outputs.insert(OutputRecord::prepare(
        normalized,
        requirements,
        capability,
        retained_bytes,
    )) {
        Ok(value) => value,
        Err(status) => {
            drop(outputs);
            if let Err(drop_status) = runtime.release_owned_cap(capability) {
                return drop_status;
            }
            return status;
        }
    };
    let value = outputs.get(release).prepare_output(release);
    // SAFETY: Caller-provided output was validated before ownership allocation.
    match unsafe { wire_io::write_output(output, capacity, value) } {
        Ok(()) => crate::STATUS_OK,
        Err(status) => status,
    }
}
