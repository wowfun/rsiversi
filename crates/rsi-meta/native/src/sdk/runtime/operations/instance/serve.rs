use super::super::super::gates::InstanceGate;
use super::super::super::output_records::size_u32;
use super::super::super::panics;
use super::super::super::values::CapValue;
use super::super::super::{PluginRuntime, lock, wire_io};
use super::super::common;
use super::{MAX_PORT_BYTES, invalid, rejected};
use crate::sdk::host::{CallbackScope, provider_channel};
use crate::{
    BasicOutput, CAP_KIND_INSTANCE, CAP_KIND_PROVIDER_CHANNEL, NativeInstance, NativePlugin,
    RIGHT_MUTATE, RIGHT_RECEIVE, RIGHT_SEND, STATUS_FAILED, STATUS_OK, STATUS_TERMINAL, ServeInput,
};
use core::ffi::c_void;

pub(in crate::sdk::runtime::operations) fn serve<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    if let Err(status) = wire_io::check_output::<BasicOutput>(output, capacity) {
        return status;
    }
    // SAFETY: Exact frame validation precedes every field use.
    let input = match unsafe { wire_io::read_input::<ServeInput>(input, input_size) } {
        Ok(value)
            if wire_io::validate_header(value.header, size_u32::<ServeInput>(), input_size)
                .is_ok()
                && value.callback_id != 0 =>
        {
            value
        }
        _ => return invalid(runtime, "serve input", output, capacity),
    };
    let instance = match runtime
        .caps()
        .get(input.instance, CAP_KIND_INSTANCE, RIGHT_MUTATE)
    {
        Ok(CapValue::Instance(value)) => value,
        Ok(_) => unreachable!("cap table kind matches value"),
        Err(status) => return rejected(runtime, status, "serve", output, capacity),
    };
    let _gate = match InstanceGate::acquire(&instance, input.callback_id) {
        Ok(gate) => gate,
        Err(status) => return rejected(runtime, status, "serve gate", output, capacity),
    };
    if !instance.is_active() {
        return rejected(
            runtime,
            STATUS_TERMINAL,
            "serve inactive instance",
            output,
            capacity,
        );
    }
    let port = runtime.host();
    if port
        .validate_cap(
            input.provider,
            CAP_KIND_PROVIDER_CHANNEL,
            RIGHT_RECEIVE | RIGHT_SEND,
        )
        .is_err()
    {
        return rejected(
            runtime,
            crate::STATUS_WRONG_CAPABILITY,
            "provider channel",
            output,
            capacity,
        );
    }
    // SAFETY: Trusted caller keeps the validated port range readable synchronously.
    let port_name = match unsafe { wire_io::copy_bytes(input.port, MAX_PORT_BYTES) } {
        Ok(value) => value,
        Err(status) => return rejected(runtime, status, "serve port", output, capacity),
    };
    let callback = match runtime.callback() {
        Ok(value) => value,
        Err(status) => return rejected(runtime, status, "serve callback", output, capacity),
    };
    let scope = CallbackScope::new();
    let _seal = scope.guard();
    let mut channel = provider_channel(port, &scope, input.provider);
    let result = panics::catch(|| {
        let mut value = lock(&instance.instance);
        value
            .as_mut()
            .expect("live instance value")
            .serve(&port_name, &mut channel)
    });
    drop(channel);
    scope.seal();
    drop(callback);
    match result {
        Ok(Ok(())) => {
            common::write_basic(output, capacity).map_or_else(|status| status, |()| STATUS_OK)
        }
        Ok(Err(error)) => common::write_error(
            runtime,
            STATUS_FAILED,
            error,
            size_u32::<BasicOutput>(),
            output,
            capacity,
        ),
        Err(panic) => {
            instance.mark_terminal();
            common::write_error(
                runtime,
                crate::STATUS_PANICKED,
                format!("native serve panicked: {}", panic.diagnostic()),
                size_u32::<BasicOutput>(),
                output,
                capacity,
            )
        }
    }
}
