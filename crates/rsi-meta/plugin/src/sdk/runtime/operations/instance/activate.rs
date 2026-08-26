use super::super::super::gates::InstanceGate;
use super::super::super::output_records::size_u32;
use super::super::super::panics;
use super::super::super::values::{CapValue, InstanceCell};
use super::super::super::{PluginRuntime, lock, wire_io};
use super::super::common;
use super::injections::import_injections;
use super::{invalid, rejected};
use crate::sdk::host::{CallbackScope, HostPort, Injection, activation};
use crate::{
    ActivateInput, BasicOutput, CAP_KIND_ACTIVATION, CAP_KIND_EFFECT_TXN, CAP_KIND_INSTANCE,
    CapInput, HOST_EFFECT_BEGIN, NativeInstance, NativePlugin, RIGHT_MUTATE, STATUS_FAILED,
    STATUS_OK, STATUS_PROTOCOL_ERROR,
};
use core::ffi::c_void;

pub(in crate::sdk::runtime::operations) fn activate<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    capacity: u32,
) -> u32 {
    if let Err(status) = wire_io::check_output::<BasicOutput>(output, capacity) {
        return status;
    }
    let Ok(input) = read_activate(input, input_size) else {
        return invalid(runtime, "activate input", output, capacity);
    };
    let instance = match runtime
        .caps()
        .get(input.instance, CAP_KIND_INSTANCE, RIGHT_MUTATE)
    {
        Ok(CapValue::Instance(value)) => value,
        Ok(_) => unreachable!("cap table kind matches value"),
        Err(status) => return rejected(runtime, status, "activate", output, capacity),
    };
    let _gate = match InstanceGate::acquire(&instance, input.callback_id) {
        Ok(gate) => gate,
        Err(status) => return rejected(runtime, status, "activate gate", output, capacity),
    };
    if !instance.begin_activation() {
        return rejected(
            runtime,
            STATUS_PROTOCOL_ERROR,
            "activate lifecycle",
            output,
            capacity,
        );
    }
    let callback = match runtime.callback() {
        Ok(value) => value,
        Err(status) => {
            instance.mark_terminal();
            return rejected(runtime, status, "activation callback", output, capacity);
        }
    };
    let scope = CallbackScope::new();
    let _seal = scope.guard();
    let port = runtime.host();
    if port
        .validate_cap(input.activation, CAP_KIND_ACTIVATION, RIGHT_MUTATE)
        .is_err()
    {
        instance.mark_terminal();
        return rejected(
            runtime,
            crate::STATUS_WRONG_CAPABILITY,
            "activation capability",
            output,
            capacity,
        );
    }
    let injections = match import_injections(runtime, &instance.requirements, input) {
        Ok(value) => value,
        Err(status) => {
            instance.mark_terminal();
            return rejected(runtime, status, "activation injections", output, capacity);
        }
    };
    let transaction = match port.borrowed_cap(
        HOST_EFFECT_BEGIN,
        &CapInput {
            header: crate::FrameHeader::new(size_u32::<CapInput>()),
            capability: input.activation,
        },
        CAP_KIND_EFFECT_TXN,
        RIGHT_MUTATE,
    ) {
        Ok(value) => value,
        Err(error) => {
            instance.mark_terminal();
            return common::write_error(
                runtime,
                error.status(),
                error.to_string(),
                size_u32::<BasicOutput>(),
                output,
                capacity,
            );
        }
    };
    let result = invoke_activation(runtime, &instance, port, &scope, transaction, injections);
    scope.seal();
    drop(callback);
    match result {
        Ok(()) => {
            instance.mark_active();
            common::write_basic(output, capacity).map_or_else(|status| status, |()| STATUS_OK)
        }
        Err(failure) => {
            instance.mark_terminal();
            common::write_error(
                runtime,
                failure.status,
                failure.diagnostic,
                size_u32::<BasicOutput>(),
                output,
                capacity,
            )
        }
    }
}

fn read_activate(input: *const c_void, input_size: u32) -> Result<ActivateInput, u32> {
    // SAFETY: Exact frame validation precedes every field use.
    let value = unsafe { wire_io::read_input::<ActivateInput>(input, input_size) }?;
    wire_io::validate_header(value.header, size_u32::<ActivateInput>(), input_size)?;
    if value.callback_id == 0 {
        return Err(crate::STATUS_INVALID_ARGUMENT);
    }
    Ok(value)
}

struct ActivationFailure {
    status: u32,
    diagnostic: String,
}

fn invoke_activation<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    instance: &InstanceCell<P::Instance>,
    port: HostPort,
    scope: &CallbackScope,
    transaction: crate::CapId,
    injections: Vec<Injection>,
) -> Result<(), ActivationFailure> {
    let mut activation = activation(port, scope, transaction, runtime, injections);
    let result = panics::catch(|| {
        lock(&instance.instance)
            .as_mut()
            .expect("live instance value")
            .activate(&mut activation)
    });
    let commit_result = if matches!(&result, Ok(Ok(()))) && activation.commit_requested() {
        Some(activation.finish_commit())
    } else {
        None
    };
    let committed = activation.committed();
    drop(activation);
    match (result, commit_result) {
        (Ok(Ok(())), Some(Ok(()))) if committed => Ok(()),
        (Ok(Ok(())), Some(Err(error))) => Err(ActivationFailure {
            status: STATUS_PROTOCOL_ERROR,
            diagnostic: format!("host rejected activation commit: {error}"),
        }),
        (Ok(Ok(())), _) => Err(ActivationFailure {
            status: STATUS_PROTOCOL_ERROR,
            diagnostic: "activation returned success without exactly one commit request".to_owned(),
        }),
        (Ok(Err(error)), _) => Err(ActivationFailure {
            status: STATUS_FAILED,
            diagnostic: error,
        }),
        (Err(panic), _) => Err(ActivationFailure {
            status: crate::STATUS_PANICKED,
            diagnostic: format!("native activation panicked: {}", panic.diagnostic()),
        }),
    }
}
