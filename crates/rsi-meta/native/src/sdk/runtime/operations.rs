use super::PluginRuntime;
use crate::{
    NativePlugin, PLUGIN_ACTIVATE, PLUGIN_CAP_RELEASE, PLUGIN_CAP_RETAIN, PLUGIN_CREATE,
    PLUGIN_DESTROY_FACTORY, PLUGIN_DESTROY_INSTANCE, PLUGIN_FINALIZE, PLUGIN_IDENTITY,
    PLUGIN_PREPARE, PLUGIN_RELEASE_OUTPUT, PLUGIN_RUN_CLEANUP, PLUGIN_SERVE_PORT,
    STATUS_UNSUPPORTED,
};
use core::ffi::c_void;

mod common;
mod factory;
mod instance;
mod lifecycle;

pub(super) struct Dispatch {
    pub(super) status: u32,
    pub(super) finalize: bool,
}

impl Dispatch {
    fn status(status: u32) -> Self {
        Self {
            status,
            finalize: false,
        }
    }
}

pub(super) fn dispatch<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    opcode: u32,
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    output_capacity: u32,
) -> Dispatch {
    let status = match opcode {
        PLUGIN_IDENTITY => factory::identity(runtime, input, input_size, output, output_capacity),
        PLUGIN_PREPARE => factory::prepare(runtime, input, input_size, output, output_capacity),
        PLUGIN_CREATE => factory::create(runtime, input, input_size, output, output_capacity),
        PLUGIN_ACTIVATE => instance::activate(runtime, input, input_size, output, output_capacity),
        PLUGIN_SERVE_PORT => instance::serve(runtime, input, input_size, output, output_capacity),
        PLUGIN_RUN_CLEANUP => {
            lifecycle::run_cleanup(runtime, input, input_size, output, output_capacity)
        }
        PLUGIN_CAP_RETAIN => lifecycle::retain(runtime, input, input_size, output, output_capacity),
        PLUGIN_CAP_RELEASE => {
            lifecycle::release(runtime, input, input_size, output, output_capacity)
        }
        PLUGIN_DESTROY_INSTANCE => lifecycle::destroy(
            runtime,
            crate::CAP_KIND_INSTANCE,
            input,
            input_size,
            output,
            output_capacity,
        ),
        PLUGIN_DESTROY_FACTORY => lifecycle::destroy(
            runtime,
            crate::CAP_KIND_FACTORY,
            input,
            input_size,
            output,
            output_capacity,
        ),
        PLUGIN_RELEASE_OUTPUT => lifecycle::release_output(runtime, input, input_size),
        PLUGIN_FINALIZE => {
            return lifecycle::finalize(runtime, input, input_size, output, output_capacity);
        }
        _ => common::write_error(
            runtime,
            STATUS_UNSUPPORTED,
            "unsupported plugin opcode".to_owned(),
            common::expected_output_size(opcode),
            output,
            output_capacity,
        ),
    };
    Dispatch::status(status)
}

pub(super) fn panic_output<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    opcode: u32,
    output: *mut c_void,
    output_capacity: u32,
    message: &str,
) -> u32 {
    if let Some(status) = panic_status_without_output(opcode) {
        return status;
    }
    common::write_error(
        runtime,
        crate::STATUS_PANICKED,
        format!("native plugin panicked: {message}"),
        common::expected_output_size(opcode),
        output,
        output_capacity,
    )
}

const fn panic_status_without_output(opcode: u32) -> Option<u32> {
    if opcode == PLUGIN_RELEASE_OUTPUT {
        Some(crate::STATUS_PANICKED)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_only_release_reports_panicked_without_an_output_buffer() {
        assert_eq!(
            panic_status_without_output(PLUGIN_RELEASE_OUTPUT),
            Some(crate::STATUS_PANICKED)
        );
        assert_eq!(panic_status_without_output(PLUGIN_IDENTITY), None);
    }
}
