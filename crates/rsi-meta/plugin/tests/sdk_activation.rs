#![allow(unsafe_code)] // Exercises both public native exchange ports.

use core::ffi::c_void;
use rsi_meta_plugin::*;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};

static CLEANUPS: AtomicUsize = AtomicUsize::new(0);
static TEST_LOCK: Mutex<()> = Mutex::new(());
static REENTRY_CALL: Mutex<Option<RawServeCall>> = Mutex::new(None);
static REENTRY_STATUS: AtomicU32 = AtomicU32::new(STATUS_OK);
static BLOCK: OnceLock<(Mutex<BlockState>, Condvar)> = OnceLock::new();
static CAPTURED_HOST: Mutex<Option<Host<'static>>> = Mutex::new(None);
static CAPTURED_CAPABILITY: Mutex<Option<Capability>> = Mutex::new(None);

#[derive(Default)]
struct ActivationPlugin;

impl NativePlugin for ActivationPlugin {
    type Prepared = String;
    type Instance = ActivationInstance;

    fn identity(&self) -> Result<String, String> {
        Ok("fixture.activation".to_owned())
    }

    fn prepare(&self, desired: &Value) -> Result<Prepared<Self::Prepared>, String> {
        let mode = desired["mode"].as_str().unwrap_or("commit").to_owned();
        let mut prepared = Prepared::new(
            json!({ "mode": mode }),
            mode.clone(),
            retained_string_bytes(&mode),
        )
        .requiring(ServiceRequirement::new("upstream", "fixture.upstream", 1));
        if mode == "two" {
            prepared =
                prepared.requiring(ServiceRequirement::new("secondary", "fixture.secondary", 1));
        }
        Ok(prepared)
    }

    fn create(&self, prepared: Self::Prepared) -> Result<Self::Instance, String> {
        Ok(ActivationInstance { mode: prepared })
    }
}

struct ActivationInstance {
    mode: String,
}

impl ActivationInstance {
    fn serve_channels(&self, provider: &mut ProviderChannel<'_>) -> Result<(), String> {
        let request = provider
            .receive()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "request missing".to_owned())?;
        if request.bytes != b"request" || request.capabilities.len() != 1 {
            return Err("request shape mismatch".to_owned());
        }
        let mut caller = provider
            .host()
            .open(&request.capabilities[0])
            .map_err(|error| error.to_string())?;
        self.exercise_caller(&mut caller, &request)?;

        provider.send(&request).map_err(|error| error.to_string())?;
        if provider
            .receive()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err("provider request stream did not end".to_owned());
        }
        assert_protocol(provider.receive(), "provider request EOF must be one-shot");
        let cancelled = provider.cancelled().map_err(|error| error.to_string())?;
        if cancelled != (self.mode == "channel_error") {
            return Err("provider cancellation state mismatch".to_owned());
        }
        Ok(())
    }

    fn exercise_caller(
        &self,
        caller: &mut CallChannel<'_>,
        request: &Message,
    ) -> Result<(), String> {
        assert_protocol(
            caller.terminal(),
            "caller terminal must require response EOF",
        );
        caller.send(request).map_err(|error| error.to_string())?;
        caller
            .finish_requests()
            .map_err(|error| error.to_string())?;
        assert_protocol(
            caller.finish_requests(),
            "caller request finish must be one-shot",
        );
        assert_protocol(
            caller.send(request),
            "caller send must close after request finish",
        );

        let response = caller
            .receive()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "response missing".to_owned())?;
        let nested = caller
            .host()
            .open(&response.capabilities[0])
            .map_err(|error| error.to_string())?;
        drop(nested);
        if caller
            .receive()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err("response stream did not end".to_owned());
        }
        assert_protocol(caller.receive(), "caller response EOF must be one-shot");
        let terminal = caller.terminal();
        if self.mode == "channel_error" {
            assert_eq!(
                terminal
                    .expect_err("cached cancellation must be returned")
                    .status(),
                STATUS_CANCELLED
            );
        } else {
            terminal.map_err(|error| error.to_string())?;
        }
        assert_protocol(
            caller.terminal(),
            "caller terminal observation must be one-shot",
        );
        Ok(())
    }
}

impl NativeInstance for ActivationInstance {
    fn activate(&mut self, activation: &mut Activation<'_>) -> Result<(), String> {
        assert!(activation.injection("upstream").is_some());
        let cleanup_panics = self.mode == "cleanup_panic";
        activation
            .effects()
            .defer("fixture cleanup", move || {
                CLEANUPS.fetch_add(1, Ordering::SeqCst);
                assert!(!cleanup_panics, "cleanup panic");
                Ok(())
            })
            .map_err(|error| error.to_string())?;
        let provided = activation
            .effects()
            .provide("echo", "fixture.echo", 1, b"echo")
            .map_err(|error| error.to_string())?;
        drop(provided);
        match self.mode.as_str() {
            "capture" => {
                let capability = activation
                    .injection("upstream")
                    .expect("captured injection")
                    .try_clone()
                    .map_err(|error| error.to_string())?;
                // SAFETY: This hostile test intentionally defeats the safe
                // callback lifetime so the runtime seal remains observable.
                let host =
                    unsafe { std::mem::transmute::<Host<'_>, Host<'static>>(activation.host()) };
                *lock(&CAPTURED_HOST) = Some(host);
                *lock(&CAPTURED_CAPABILITY) = Some(capability);
                activation
                    .effects()
                    .commit()
                    .map_err(|error| error.to_string())
            }
            "activation_scope" => {
                let opened = activation
                    .host()
                    .open(
                        activation
                            .injection("upstream")
                            .expect("upstream injection"),
                    )
                    .map_err(|error| error.to_string())?;
                drop(opened);
                activation
                    .effects()
                    .commit()
                    .map_err(|error| error.to_string())
            }
            "commit" | "reenter" | "block" | "channel" | "channel_error" | "cleanup_panic"
            | "serve_panic" => activation
                .effects()
                .commit()
                .map_err(|error| error.to_string()),
            "commit_error" => {
                activation
                    .effects()
                    .commit()
                    .map_err(|error| error.to_string())?;
                Err("error after commit request".to_owned())
            }
            "commit_panic" => {
                activation
                    .effects()
                    .commit()
                    .map_err(|error| error.to_string())?;
                panic!("panic after commit request")
            }
            "double_commit" => {
                activation
                    .effects()
                    .commit()
                    .map_err(|error| error.to_string())?;
                let error = activation
                    .effects()
                    .commit()
                    .expect_err("second commit request must fail");
                assert_eq!(error.status(), STATUS_PROTOCOL_ERROR);
                Ok(())
            }
            "defer_after_commit" => {
                activation
                    .effects()
                    .commit()
                    .map_err(|error| error.to_string())?;
                let error = activation
                    .effects()
                    .defer("too late", || Ok(()))
                    .expect_err("mutation after commit request must fail");
                assert_eq!(error.status(), STATUS_PROTOCOL_ERROR);
                Ok(())
            }
            "commit_fail" => activation
                .effects()
                .commit()
                .map_err(|error| error.to_string()),
            "error" => Err("activation rejected".to_owned()),
            "open" => Ok(()),
            "panic" => panic!("activation panic"),
            mode => Err(format!("unknown mode {mode}")),
        }
    }

    fn serve(&mut self, _: &[u8], channel: &mut ProviderChannel<'_>) -> Result<(), String> {
        match self.mode.as_str() {
            "channel" | "channel_error" => self.serve_channels(channel),
            "reenter" => {
                let call = (*lock(&REENTRY_CALL)).expect("reentry call installed");
                let (status, release) = call.invoke_port(77, b"different-port");
                call.release_output(release);
                REENTRY_STATUS.store(status, Ordering::SeqCst);
                Ok(())
            }
            "block" => {
                let (state, changed) =
                    BLOCK.get_or_init(|| (Mutex::new(BlockState::default()), Condvar::new()));
                let mut state = lock(state);
                state.entered = true;
                changed.notify_all();
                while !state.released {
                    state = changed
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                Ok(())
            }
            "serve_panic" => panic!("serve panic"),
            _ => Ok(()),
        }
    }
}

fn assert_protocol<T>(result: Result<T, SdkError>, message: &str) {
    match result {
        Err(error) => assert_eq!(error.status(), STATUS_PROTOCOL_ERROR, "{message}"),
        Ok(_) => panic!("{message}"),
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CallerRequests {
    #[default]
    Open,
    Finished,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CallerResponses {
    #[default]
    Open,
    Eof,
    Observed,
}

#[derive(Default)]
struct HostState {
    begins: usize,
    defers: usize,
    commits: usize,
    aborts: usize,
    output_releases: usize,
    injection_refs: usize,
    provided_refs: usize,
    secondary_refs: usize,
    message_refs: usize,
    reject_secondary_retain: bool,
    reject_commit: bool,
    reject_defer: bool,
    cleanup: Option<CapId>,
    rejected_cleanup: Option<CapId>,
    next_release_epoch: u64,
    message_release: Option<ReleaseId>,
    opens: usize,
    open_scopes: Vec<CapId>,
    provider_receives: usize,
    caller_receives: usize,
    provider_sends: usize,
    caller_sends: usize,
    finishes: usize,
    terminals: usize,
    cancelled_checks: usize,
    caller_requests: CallerRequests,
    caller_responses: CallerResponses,
    terminal_status: u32,
    sent_bytes: Vec<u8>,
    sent_capabilities: Vec<CapId>,
}

struct HostHarness {
    state: Box<Mutex<HostState>>,
    table: HostTable,
}

impl HostHarness {
    fn new() -> Self {
        Self::with_failures(false, false, false, STATUS_OK)
    }

    fn with_rejected_secondary(reject_secondary_retain: bool) -> Self {
        Self::with_failures(reject_secondary_retain, false, false, STATUS_OK)
    }

    fn with_rejected_commit() -> Self {
        Self::with_failures(false, true, false, STATUS_OK)
    }

    fn with_rejected_defer() -> Self {
        Self::with_failures(false, false, true, STATUS_OK)
    }

    fn with_terminal(terminal_status: u32) -> Self {
        Self::with_failures(false, false, false, terminal_status)
    }

    fn with_failures(
        reject_secondary_retain: bool,
        reject_commit: bool,
        reject_defer: bool,
        terminal_status: u32,
    ) -> Self {
        let state = Box::new(Mutex::new(HostState {
            injection_refs: 1,
            secondary_refs: 1,
            message_refs: 1,
            reject_secondary_retain,
            reject_commit,
            reject_defer,
            terminal_status,
            next_release_epoch: 1,
            ..HostState::default()
        }));
        let table = HostTable {
            header: TableHeader::new(ABI_MINOR, HostTable::STRUCT_SIZE),
            issuer: 900,
            state: (&raw const *state).cast_mut().cast(),
            exchange: Some(host_exchange),
        };
        Self { state, table }
    }

    fn snapshot(&self) -> MutexGuard<'_, HostState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

unsafe extern "C" fn host_exchange(
    state: *mut c_void,
    opcode: u32,
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    output_capacity: u32,
) -> u32 {
    if state.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: HostHarness owns this Mutex through plugin finalization.
    let state = unsafe { &*state.cast::<Mutex<HostState>>() };
    let call = HostCall {
        input,
        input_size,
        output,
        output_capacity,
    };
    match opcode {
        HOST_EFFECT_BEGIN => effect_begin(state, call),
        HOST_EFFECT_DEFER => effect_defer(state, call),
        HOST_EFFECT_COMMIT => effect_close(state, call, true),
        HOST_EFFECT_ABORT => effect_close(state, call, false),
        HOST_PROVIDE => provide(state, call),
        HOST_CAP_OPEN => cap_open(state, call),
        HOST_CHANNEL_RECV => channel_receive(state, call),
        HOST_CHANNEL_SEND => channel_send(state, call),
        HOST_CHANNEL_FINISH_REQUESTS => channel_close(state, call, true),
        HOST_CHANNEL_TERMINAL => channel_close(state, call, false),
        HOST_CHANNEL_CANCELLED => channel_cancelled(state, call),
        HOST_CAP_RETAIN => cap_ref(state, call, true),
        HOST_CAP_RELEASE => cap_ref(state, call, false),
        HOST_RELEASE_OUTPUT => release_output(state, call),
        _ => STATUS_UNSUPPORTED,
    }
}

#[derive(Clone, Copy)]
struct HostCall {
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    output_capacity: u32,
}

impl HostCall {
    fn input<T: Copy>(self) -> Option<T> {
        // SAFETY: `read` validates the complete fixed frame before copying it.
        unsafe { read(self.input, self.input_size) }
    }

    fn write<T>(self, value: T) -> u32 {
        write(self.output, self.output_capacity, value)
    }

    fn write_basic(self) -> u32 {
        write_basic(self.output, self.output_capacity)
    }
}

fn effect_begin(state: &Mutex<HostState>, call: HostCall) -> u32 {
    let Some(input) = call.input::<CapInput>() else {
        return STATUS_INVALID_ARGUMENT;
    };
    if input.capability != activation_cap() {
        return STATUS_WRONG_CAPABILITY;
    }
    lock(state).begins += 1;
    call.write(BorrowedCapOutput {
        prefix: OutputPrefix::empty(size_u32::<BorrowedCapOutput>()),
        capability: effect_cap(),
    })
}

fn effect_defer(state: &Mutex<HostState>, call: HostCall) -> u32 {
    let Some(input) = call.input::<EffectDeferInput>() else {
        return STATUS_INVALID_ARGUMENT;
    };
    if input.transaction != effect_cap()
        || input.cleanup.kind != CAP_KIND_CLEANUP
        || input.cleanup.rights != RIGHT_MUTATE
    {
        return STATUS_WRONG_CAPABILITY;
    }
    let mut state = lock(state);
    if state.reject_defer {
        state.rejected_cleanup = Some(input.cleanup);
        drop(state);
        let write_status = call.write_basic();
        return if write_status == STATUS_OK {
            STATUS_FAILED
        } else {
            write_status
        };
    }
    state.defers += 1;
    state.cleanup = Some(input.cleanup);
    call.write_basic()
}

fn effect_close(state: &Mutex<HostState>, call: HostCall, commit: bool) -> u32 {
    let Some(input) = call.input::<CapInput>() else {
        return STATUS_INVALID_ARGUMENT;
    };
    if input.capability != effect_cap() {
        return STATUS_WRONG_CAPABILITY;
    }
    let mut state = lock(state);
    if commit {
        state.commits += 1;
    } else {
        state.aborts += 1;
    }
    let reject = commit && state.reject_commit;
    drop(state);
    let written = call.write_basic();
    if written != STATUS_OK {
        written
    } else if reject {
        STATUS_FAILED
    } else {
        STATUS_OK
    }
}

fn provide(state: &Mutex<HostState>, call: HostCall) -> u32 {
    let Some(input) = call.input::<ProvideInput>() else {
        return STATUS_INVALID_ARGUMENT;
    };
    // SAFETY: The fixed input frame keeps its trusted synchronous byte range live.
    if input.transaction != effect_cap() || unsafe { bytes(input.port) } != b"echo" {
        return STATUS_WRONG_CAPABILITY;
    }
    let mut state = lock(state);
    state.provided_refs = 1;
    let release = ReleaseId {
        issuer: 900,
        slot: 1,
        epoch: state.next_release_epoch,
    };
    state.next_release_epoch += 1;
    call.write(CapOutput {
        prefix: OutputPrefix {
            struct_size: size_u32::<CapOutput>(),
            reserved: 0,
            release,
            diagnostic: RawBytes::EMPTY,
        },
        capability: provided_cap(),
    })
}

fn cap_open(state: &Mutex<HostState>, call: HostCall) -> u32 {
    let Some(input) = call.input::<OpenInput>() else {
        return STATUS_INVALID_ARGUMENT;
    };
    let valid = (input.scope == effect_cap() && input.service == injection_cap())
        || (input.scope == provider_channel_cap() && input.service == message_cap())
        || (input.scope == opened_channel_cap() && input.service == message_cap());
    if !valid {
        return STATUS_WRONG_CAPABILITY;
    }
    let mut state = lock(state);
    state.opens += 1;
    state.open_scopes.push(input.scope);
    let capability = if input.scope == opened_channel_cap() {
        nested_channel_cap()
    } else {
        opened_channel_cap()
    };
    drop(state);
    call.write(BorrowedCapOutput {
        prefix: OutputPrefix::empty(size_u32::<BorrowedCapOutput>()),
        capability,
    })
}

fn channel_receive(state: &Mutex<HostState>, call: HostCall) -> u32 {
    let Some(input) = call.input::<CapInput>() else {
        return STATUS_INVALID_ARGUMENT;
    };
    if input.capability != provider_channel_cap() && input.capability != opened_channel_cap() {
        return STATUS_WRONG_CAPABILITY;
    }
    let mut state = lock(state);
    let (receive_count, payload) = if input.capability == provider_channel_cap() {
        state.provider_receives += 1;
        (state.provider_receives, b"request".as_slice())
    } else {
        state.caller_receives += 1;
        (state.caller_receives, b"response".as_slice())
    };
    if receive_count != 1 {
        if input.capability == opened_channel_cap() {
            state.caller_responses = CallerResponses::Eof;
        }
        return call.write(MessageOutput {
            prefix: OutputPrefix::empty(size_u32::<MessageOutput>()),
            present: 0,
            reserved: 0,
            message: RawMessage {
                bytes: RawBytes::EMPTY,
                capabilities: core::ptr::null(),
                capability_count: 0,
            },
        });
    }
    let release = ReleaseId {
        issuer: 900,
        slot: 2,
        epoch: state.next_release_epoch,
    };
    state.next_release_epoch += 1;
    state.message_release = Some(release);
    state.message_refs += 1;
    call.write(MessageOutput {
        prefix: OutputPrefix {
            struct_size: size_u32::<MessageOutput>(),
            reserved: 0,
            release,
            diagnostic: RawBytes::EMPTY,
        },
        present: 1,
        reserved: 0,
        message: RawMessage {
            bytes: raw(payload),
            capabilities: (&raw const MESSAGE_CAPABILITY),
            capability_count: 1,
        },
    })
}

fn channel_send(state: &Mutex<HostState>, call: HostCall) -> u32 {
    let Some(input) = call.input::<MessageInput>() else {
        return STATUS_INVALID_ARGUMENT;
    };
    if input.channel != provider_channel_cap() && input.channel != opened_channel_cap() {
        return STATUS_WRONG_CAPABILITY;
    }
    let Ok((byte_count, cap_count)) = input.message.validate_shape(1_048_576, 1_024) else {
        return STATUS_INVALID_ARGUMENT;
    };
    // SAFETY: Shape validation checked the complete synchronous input.
    let sent_bytes =
        unsafe { std::slice::from_raw_parts(input.message.bytes.ptr, byte_count).to_vec() };
    // SAFETY: Shape validation checked alignment, count, and bounds.
    let sent_capabilities =
        unsafe { std::slice::from_raw_parts(input.message.capabilities, cap_count).to_vec() };
    let mut state = lock(state);
    if input.channel == provider_channel_cap() {
        state.provider_sends += 1;
    } else {
        state.caller_sends += 1;
    }
    state.sent_bytes = sent_bytes;
    state.sent_capabilities = sent_capabilities;
    call.write_basic()
}

fn channel_close(state: &Mutex<HostState>, call: HostCall, finish: bool) -> u32 {
    let Some(input) = call.input::<CapInput>() else {
        return STATUS_INVALID_ARGUMENT;
    };
    if input.capability != opened_channel_cap() {
        return STATUS_WRONG_CAPABILITY;
    }
    let mut state = lock(state);
    if finish {
        if state.caller_requests != CallerRequests::Open
            || state.caller_responses != CallerResponses::Open
        {
            return STATUS_PROTOCOL_ERROR;
        }
        state.caller_requests = CallerRequests::Finished;
        state.finishes += 1;
    } else {
        if state.caller_responses != CallerResponses::Eof {
            return STATUS_PROTOCOL_ERROR;
        }
        state.caller_responses = CallerResponses::Observed;
        state.terminals += 1;
    }
    let terminal_status = state.terminal_status;
    drop(state);
    let write_status = call.write_basic();
    if write_status != STATUS_OK {
        write_status
    } else if finish {
        STATUS_OK
    } else {
        terminal_status
    }
}

fn channel_cancelled(state: &Mutex<HostState>, call: HostCall) -> u32 {
    let Some(input) = call.input::<CapInput>() else {
        return STATUS_INVALID_ARGUMENT;
    };
    if input.capability != provider_channel_cap() && input.capability != opened_channel_cap() {
        return STATUS_WRONG_CAPABILITY;
    }
    let mut state = lock(state);
    state.cancelled_checks += 1;
    let cancelled = state.terminal_status == STATUS_CANCELLED;
    drop(state);
    call.write(BoolOutput {
        prefix: OutputPrefix::empty(size_u32::<BoolOutput>()),
        value: u32::from(cancelled),
        reserved: 0,
    })
}

fn cap_ref(state: &Mutex<HostState>, call: HostCall, retain: bool) -> u32 {
    let Some(input) = call.input::<CapInput>() else {
        return STATUS_INVALID_ARGUMENT;
    };
    let mut state = lock(state);
    if input.capability == secondary_cap() && retain && state.reject_secondary_retain {
        drop(state);
        let write_status = call.write_basic();
        return if write_status == STATUS_OK {
            STATUS_LIMIT_EXCEEDED
        } else {
            write_status
        };
    }
    let refs = if input.capability == injection_cap() {
        &mut state.injection_refs
    } else if input.capability == secondary_cap() {
        &mut state.secondary_refs
    } else if input.capability == provided_cap() {
        &mut state.provided_refs
    } else if input.capability == message_cap() {
        &mut state.message_refs
    } else {
        return STATUS_WRONG_CAPABILITY;
    };
    if retain {
        *refs += 1;
    } else if *refs == 0 {
        return STATUS_PROTOCOL_ERROR;
    } else {
        *refs -= 1;
    }
    call.write_basic()
}

fn release_output(state: &Mutex<HostState>, call: HostCall) -> u32 {
    let Some(input) = call.input::<ReleaseOutputInput>() else {
        return STATUS_INVALID_ARGUMENT;
    };
    let mut state = lock(state);
    if input.release.issuer != 900 {
        return STATUS_PROTOCOL_ERROR;
    }
    if input.release.slot == 1 && state.provided_refs != 0 {
        state.provided_refs -= 1;
    } else if Some(input.release) == state.message_release && state.message_refs != 0 {
        state.message_release = None;
        state.message_refs -= 1;
    } else {
        return STATUS_PROTOCOL_ERROR;
    }
    state.output_releases += 1;
    STATUS_OK
}

struct PluginHarness<'a> {
    host: &'a HostHarness,
    table: PluginTable,
    prepared: CapId,
    instance: CapId,
}

impl<'a> PluginHarness<'a> {
    fn create(host: &'a HostHarness, mode: &str) -> Self {
        let mut table = PluginTable::EMPTY;
        // SAFETY: Both tables are live and exclusively borrowed for entry.
        assert_eq!(
            unsafe {
                plugin_entry::<ActivationPlugin>(
                    &raw const host.table,
                    &raw mut table,
                    PluginTable::STRUCT_SIZE,
                )
            },
            STATUS_OK
        );
        let desired = serde_json::to_vec(&json!({ "mode": mode })).unwrap();
        let mut prepared: PrepareOutput = unsafe { core::mem::zeroed() };
        assert_eq!(
            exchange(
                table,
                PLUGIN_PREPARE,
                &BytesInput {
                    header: frame::<BytesInput>(),
                    receiver: table.factory,
                    bytes: raw(&desired),
                },
                &mut prepared,
            ),
            STATUS_OK
        );
        assert_eq!(prepared.retained_bytes, retained_string_bytes(mode));
        retain_and_release(table, prepared.prepared, prepared.prefix.release);
        let mut instance: CapOutput = unsafe { core::mem::zeroed() };
        assert_eq!(
            exchange(
                table,
                PLUGIN_CREATE,
                &CapInput {
                    header: frame::<CapInput>(),
                    capability: prepared.prepared,
                },
                &mut instance,
            ),
            STATUS_OK
        );
        retain_and_release(table, instance.capability, instance.prefix.release);
        Self {
            host,
            table,
            prepared: prepared.prepared,
            instance: instance.capability,
        }
    }

    fn activate(&self) -> (u32, ReleaseId) {
        self.activate_with(&[Injection {
            requirement_index: 0,
            service: injection_cap(),
        }])
    }

    fn activate_with(&self, injections: &[Injection]) -> (u32, ReleaseId) {
        let mut output: BasicOutput = unsafe { core::mem::zeroed() };
        let status = exchange(
            self.table,
            PLUGIN_ACTIVATE,
            &ActivateInput {
                header: frame::<ActivateInput>(),
                callback_id: 44,
                instance: self.instance,
                activation: activation_cap(),
                injections: injections.as_ptr(),
                injection_count: u64::try_from(injections.len()).unwrap(),
            },
            &mut output,
        );
        (status, output.prefix.release)
    }

    fn run_cleanup(&self) {
        self.run_cleanup_expect(STATUS_OK);
    }

    fn run_cleanup_expect(&self, expected: u32) {
        let cleanup = self.host.snapshot().cleanup.expect("deferred cleanup");
        let mut output: BasicOutput = unsafe { core::mem::zeroed() };
        assert_eq!(
            exchange(
                self.table,
                PLUGIN_CAP_RETAIN,
                &CapInput {
                    header: frame::<CapInput>(),
                    capability: cleanup,
                },
                &mut output,
            ),
            STATUS_WRONG_CAPABILITY,
            "moved cleanup lease is not retainable"
        );
        release_plugin_output(self.table, output.prefix.release);
        assert_eq!(
            exchange(
                self.table,
                PLUGIN_CAP_RELEASE,
                &CapInput {
                    header: frame::<CapInput>(),
                    capability: cleanup,
                },
                &mut output,
            ),
            STATUS_PROTOCOL_ERROR,
            "cleanup lease cannot release before its action completes"
        );
        release_plugin_output(self.table, output.prefix.release);
        assert_eq!(
            exchange(
                self.table,
                PLUGIN_RUN_CLEANUP,
                &CapInput {
                    header: frame::<CapInput>(),
                    capability: cleanup,
                },
                &mut output,
            ),
            expected
        );
        release_plugin_output(self.table, output.prefix.release);
        assert_eq!(
            exchange(
                self.table,
                PLUGIN_RUN_CLEANUP,
                &CapInput {
                    header: frame::<CapInput>(),
                    capability: cleanup,
                },
                &mut output,
            ),
            STATUS_PROTOCOL_ERROR
        );
        release_plugin_output(self.table, output.prefix.release);
        assert_eq!(
            exchange(
                self.table,
                PLUGIN_CAP_RELEASE,
                &CapInput {
                    header: frame::<CapInput>(),
                    capability: cleanup,
                },
                &mut output,
            ),
            STATUS_OK
        );
        assert_eq!(
            exchange(
                self.table,
                PLUGIN_CAP_RELEASE,
                &CapInput {
                    header: frame::<CapInput>(),
                    capability: cleanup,
                },
                &mut output,
            ),
            STATUS_PROTOCOL_ERROR
        );
        release_plugin_output(self.table, output.prefix.release);
    }

    fn close(self) {
        let mut output: BasicOutput = unsafe { core::mem::zeroed() };
        for (opcode, capability) in [
            (PLUGIN_DESTROY_INSTANCE, self.instance),
            (PLUGIN_CAP_RELEASE, self.prepared),
            (PLUGIN_DESTROY_FACTORY, self.table.factory),
        ] {
            assert_eq!(
                exchange(
                    self.table,
                    opcode,
                    &CapInput {
                        header: frame::<CapInput>(),
                        capability,
                    },
                    &mut output,
                ),
                STATUS_OK
            );
        }
        assert_eq!(
            exchange(
                self.table,
                PLUGIN_FINALIZE,
                &EmptyInput {
                    header: frame::<EmptyInput>(),
                },
                &mut output,
            ),
            STATUS_OK
        );
    }
}

#[test]
fn activation_records_one_adapter_acceptance_and_cleanup_runs_once() {
    let _serial = lock(&TEST_LOCK);
    CLEANUPS.store(0, Ordering::SeqCst);
    let host = HostHarness::new();
    let plugin = PluginHarness::create(&host, "commit");
    let (status, release) = plugin.activate();
    assert_eq!(status, STATUS_OK);
    assert!(release.is_empty());
    {
        let state = host.snapshot();
        assert_eq!(
            (state.begins, state.defers, state.commits, state.aborts),
            (1, 1, 1, 0)
        );
        assert_eq!(state.injection_refs, 1);
        assert_eq!(state.provided_refs, 0);
        assert_eq!(state.output_releases, 1);
    }
    plugin.run_cleanup();
    assert_eq!(CLEANUPS.load(Ordering::SeqCst), 1);
    plugin.close();
}

#[test]
fn activation_error_open_success_and_panic_all_abort() {
    let _serial = lock(&TEST_LOCK);
    for (mode, expected) in [
        ("error", STATUS_FAILED),
        ("open", STATUS_PROTOCOL_ERROR),
        ("panic", STATUS_PANICKED),
        ("commit_error", STATUS_FAILED),
        ("commit_panic", STATUS_PANICKED),
    ] {
        let host = HostHarness::new();
        let plugin = PluginHarness::create(&host, mode);
        let (status, release) = plugin.activate();
        assert_eq!(status, expected, "mode {mode}");
        release_plugin_output(plugin.table, release);
        assert_eq!(host.snapshot().aborts, 1, "mode {mode}");
        if mode == "panic" {
            let (retry, retry_release) = plugin.activate();
            assert_eq!(retry, STATUS_PROTOCOL_ERROR);
            release_plugin_output(plugin.table, retry_release);
            assert_eq!(host.snapshot().begins, 1, "failed activation is terminal");
        }
        plugin.run_cleanup();
        plugin.close();
    }
}

#[test]
fn commit_request_closes_mutation_and_adapter_accepts_only_after_user_success() {
    let _serial = lock(&TEST_LOCK);
    for mode in ["double_commit", "defer_after_commit"] {
        let host = HostHarness::new();
        let plugin = PluginHarness::create(&host, mode);
        assert_eq!(plugin.activate().0, STATUS_OK, "mode {mode}");
        {
            let state = host.snapshot();
            assert_eq!((state.commits, state.aborts), (1, 0), "mode {mode}");
        }
        plugin.run_cleanup();
        plugin.close();
    }
}

#[test]
fn rejected_adapter_acceptance_is_aborted_and_never_reports_activation_success() {
    let _serial = lock(&TEST_LOCK);
    let host = HostHarness::with_rejected_commit();
    let plugin = PluginHarness::create(&host, "commit_fail");
    let (status, release) = plugin.activate();
    assert_eq!(status, STATUS_PROTOCOL_ERROR);
    release_plugin_output(plugin.table, release);
    {
        let state = host.snapshot();
        assert_eq!((state.commits, state.aborts), (1, 1));
    }
    plugin.run_cleanup();
    plugin.close();
}

#[test]
fn rejected_effect_defer_keeps_and_discards_the_plugin_cleanup_lease() {
    let _serial = lock(&TEST_LOCK);
    let host = HostHarness::with_rejected_defer();
    let plugin = PluginHarness::create(&host, "defer_rejected");
    let (status, release) = plugin.activate();
    assert_eq!(status, STATUS_FAILED);
    release_plugin_output(plugin.table, release);
    let rejected = host
        .snapshot()
        .rejected_cleanup
        .expect("host observed cleanup offer");
    let mut output: BasicOutput = unsafe { core::mem::zeroed() };
    assert_eq!(
        exchange(
            plugin.table,
            PLUGIN_CAP_RELEASE,
            &CapInput {
                header: frame::<CapInput>(),
                capability: rejected,
            },
            &mut output,
        ),
        STATUS_PROTOCOL_ERROR,
        "failed move was already discarded by the plugin"
    );
    release_plugin_output(plugin.table, output.prefix.release);
    {
        let state = host.snapshot();
        assert_eq!(state.defers, 0);
        assert_eq!(state.aborts, 1);
        assert!(state.cleanup.is_none());
    }
    plugin.close();
}

#[test]
fn panicking_cleanup_finishes_before_the_host_releases_its_lease() {
    let _serial = lock(&TEST_LOCK);
    CLEANUPS.store(0, Ordering::SeqCst);
    let host = HostHarness::new();
    let plugin = PluginHarness::create(&host, "cleanup_panic");
    assert_eq!(plugin.activate().0, STATUS_OK);
    plugin.run_cleanup_expect(STATUS_PANICKED);
    assert_eq!(CLEANUPS.load(Ordering::SeqCst), 1);
    plugin.close();
}

#[test]
fn failed_multi_capability_import_rolls_back_the_accepted_prefix() {
    let _serial = lock(&TEST_LOCK);
    let host = HostHarness::with_rejected_secondary(true);
    let plugin = PluginHarness::create(&host, "two");
    let (status, release) = plugin.activate_with(&[
        Injection {
            requirement_index: 0,
            service: injection_cap(),
        },
        Injection {
            requirement_index: 1,
            service: secondary_cap(),
        },
    ]);
    assert_eq!(status, STATUS_LIMIT_EXCEEDED);
    release_plugin_output(plugin.table, release);
    {
        let state = host.snapshot();
        assert_eq!(state.injection_refs, 1, "accepted prefix was rolled back");
        assert_eq!(state.secondary_refs, 1);
        assert_eq!(state.begins, 0, "effect begin follows complete import");
    }
    plugin.close();

    let host = HostHarness::new();
    let plugin = PluginHarness::create(&host, "commit");
    let mut wrong = injection_cap();
    wrong.rights = RIGHT_OPEN;
    let (status, release) = plugin.activate_with(&[Injection {
        requirement_index: 0,
        service: wrong,
    }]);
    assert_eq!(status, STATUS_WRONG_CAPABILITY);
    release_plugin_output(plugin.table, release);
    assert_eq!(host.snapshot().injection_refs, 1);
    plugin.close();
}

#[test]
fn caller_and_provider_channels_preserve_orientation_state_and_message_ownership() {
    let _serial = lock(&TEST_LOCK);
    let host = HostHarness::new();
    let plugin = PluginHarness::create(&host, "channel");
    assert_eq!(plugin.activate().0, STATUS_OK);
    let call = RawServeCall {
        state: plugin.table.state.addr(),
        exchange: plugin.table.exchange.expect("plugin exchange"),
        instance: plugin.instance,
    };
    let (status, release) = call.invoke(91);
    assert_eq!(status, STATUS_OK);
    assert!(release.is_empty());
    {
        let state = host.snapshot();
        assert_eq!(state.opens, 2);
        assert_eq!(
            state.open_scopes,
            [provider_channel_cap(), opened_channel_cap()]
        );
        assert_eq!(state.provider_receives, 2);
        assert_eq!(state.caller_receives, 2);
        assert_eq!(state.provider_sends, 1);
        assert_eq!(state.caller_sends, 1);
        assert_eq!(state.finishes, 1);
        assert_eq!(state.terminals, 1);
        assert_eq!(state.cancelled_checks, 1);
        assert_eq!(state.sent_bytes, b"request");
        assert_eq!(state.sent_capabilities, [message_cap()]);
        assert_eq!(state.message_refs, 1, "only the host base lease remains");
        assert!(state.message_release.is_none());
    }
    plugin.run_cleanup();
    plugin.close();
}

#[test]
fn caller_terminal_preserves_the_cached_error_after_response_eof() {
    let _serial = lock(&TEST_LOCK);
    let host = HostHarness::with_terminal(STATUS_CANCELLED);
    let plugin = PluginHarness::create(&host, "channel_error");
    assert_eq!(plugin.activate().0, STATUS_OK);
    let call = RawServeCall {
        state: plugin.table.state.addr(),
        exchange: plugin.table.exchange.expect("plugin exchange"),
        instance: plugin.instance,
    };
    let (status, release) = call.invoke(92);
    assert_eq!(status, STATUS_OK);
    assert!(release.is_empty());
    {
        let state = host.snapshot();
        assert_eq!(state.caller_responses, CallerResponses::Observed);
        assert_eq!(state.terminals, 1, "duplicate observation stayed local");
        assert_eq!(state.cancelled_checks, 1);
    }
    plugin.run_cleanup();
    plugin.close();
}

#[test]
fn cap_open_names_the_exact_activation_scope() {
    let _serial = lock(&TEST_LOCK);
    let host = HostHarness::new();
    let plugin = PluginHarness::create(&host, "activation_scope");
    assert_eq!(plugin.activate().0, STATUS_OK);
    assert_eq!(host.snapshot().open_scopes, [effect_cap()]);
    plugin.run_cleanup();
    plugin.close();
}

#[test]
fn serve_rejects_caller_orientation_and_nonexact_provider_rights() {
    let _serial = lock(&TEST_LOCK);
    let host = HostHarness::new();
    let plugin = PluginHarness::create(&host, "commit");
    assert_eq!(plugin.activate().0, STATUS_OK);
    let call = RawServeCall {
        state: plugin.table.state.addr(),
        exchange: plugin.table.exchange.expect("plugin exchange"),
        instance: plugin.instance,
    };

    for wrong in [
        opened_channel_cap(),
        CapId {
            rights: RIGHT_RECEIVE | RIGHT_SEND | RIGHT_FINISH,
            ..provider_channel_cap()
        },
    ] {
        let (status, release) = call.invoke_with(93, b"echo", wrong);
        assert_eq!(status, STATUS_WRONG_CAPABILITY);
        call.release_output(release);
    }

    plugin.run_cleanup();
    plugin.close();
}

#[test]
fn serve_panic_terminalizes_the_instance_before_returning() {
    let _serial = lock(&TEST_LOCK);
    let host = HostHarness::new();
    let plugin = PluginHarness::create(&host, "serve_panic");
    assert_eq!(plugin.activate().0, STATUS_OK);
    let call = RawServeCall {
        state: plugin.table.state.addr(),
        exchange: plugin.table.exchange.expect("plugin exchange"),
        instance: plugin.instance,
    };
    let (status, release) = call.invoke(92);
    assert_eq!(status, STATUS_PANICKED);
    call.release_output(release);
    let (retry, retry_release) = call.invoke(93);
    assert_eq!(retry, STATUS_TERMINAL);
    call.release_output(retry_release);
    plugin.run_cleanup();
    plugin.close();
}

#[derive(Default)]
struct BlockState {
    entered: bool,
    released: bool,
}

#[derive(Clone, Copy)]
struct RawServeCall {
    state: usize,
    exchange: unsafe extern "C" fn(*mut c_void, u32, *const c_void, u32, *mut c_void, u32) -> u32,
    instance: CapId,
}

impl RawServeCall {
    fn invoke(self, callback_id: u64) -> (u32, ReleaseId) {
        self.invoke_port(callback_id, b"echo")
    }

    fn invoke_port(self, callback_id: u64, port: &[u8]) -> (u32, ReleaseId) {
        self.invoke_with(callback_id, port, provider_channel_cap())
    }

    fn invoke_with(self, callback_id: u64, port: &[u8], channel: CapId) -> (u32, ReleaseId) {
        let input = ServeInput {
            header: frame::<ServeInput>(),
            callback_id,
            instance: self.instance,
            provider: channel,
            port: raw(port),
        };
        let mut output: BasicOutput = unsafe { core::mem::zeroed() };
        // SAFETY: PluginHarness retains this table state through the joined call.
        let status = unsafe {
            (self.exchange)(
                self.state as *mut c_void,
                PLUGIN_SERVE_PORT,
                (&raw const input).cast(),
                size_u32::<ServeInput>(),
                (&raw mut output).cast(),
                size_u32::<BasicOutput>(),
            )
        };
        (status, output.prefix.release)
    }

    fn release_output(self, release: ReleaseId) {
        if release.is_empty() {
            return;
        }
        let input = ReleaseOutputInput {
            header: frame::<ReleaseOutputInput>(),
            release,
        };
        // SAFETY: The release frame and plugin state remain live for this call.
        assert_eq!(
            unsafe {
                (self.exchange)(
                    self.state as *mut c_void,
                    PLUGIN_RELEASE_OUTPUT,
                    (&raw const input).cast(),
                    size_u32::<ReleaseOutputInput>(),
                    core::ptr::null_mut(),
                    0,
                )
            },
            STATUS_OK
        );
    }
}

#[test]
fn same_lineage_reentry_is_distinct_from_unrelated_busy_contention() {
    let _serial = lock(&TEST_LOCK);

    let host = HostHarness::new();
    let plugin = PluginHarness::create(&host, "reenter");
    let (status, release) = plugin.activate();
    assert_eq!(status, STATUS_OK);
    assert!(release.is_empty());
    let call = RawServeCall {
        state: plugin.table.state.addr(),
        exchange: plugin.table.exchange.expect("plugin exchange"),
        instance: plugin.instance,
    };
    *lock(&REENTRY_CALL) = Some(call);
    let (status, release) = call.invoke(77);
    assert_eq!(status, STATUS_OK);
    assert!(release.is_empty());
    assert_eq!(REENTRY_STATUS.load(Ordering::SeqCst), STATUS_REENTRANT);
    *lock(&REENTRY_CALL) = None;
    plugin.run_cleanup();
    plugin.close();

    let host = HostHarness::new();
    let plugin = PluginHarness::create(&host, "block");
    assert_eq!(plugin.activate().0, STATUS_OK);
    let call = RawServeCall {
        state: plugin.table.state.addr(),
        exchange: plugin.table.exchange.expect("plugin exchange"),
        instance: plugin.instance,
    };
    let (state, changed) =
        BLOCK.get_or_init(|| (Mutex::new(BlockState::default()), Condvar::new()));
    *lock(state) = BlockState::default();
    let worker = std::thread::spawn(move || call.invoke(77));
    let mut block = lock(state);
    while !block.entered {
        block = changed
            .wait(block)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    drop(block);
    let mut finalize_output: BasicOutput = unsafe { core::mem::zeroed() };
    assert_eq!(
        exchange(
            plugin.table,
            PLUGIN_FINALIZE,
            &EmptyInput {
                header: frame::<EmptyInput>(),
            },
            &mut finalize_output,
        ),
        STATUS_PROTOCOL_ERROR,
        "an existing admitted callback blocks finalization"
    );
    release_plugin_output(plugin.table, finalize_output.prefix.release);
    let (busy, release) = call.invoke(88);
    assert_eq!(busy, STATUS_BUSY);
    call.release_output(release);
    let mut block = lock(state);
    block.released = true;
    changed.notify_all();
    drop(block);
    let (outer, release) = worker.join().expect("outer callback joins");
    assert_eq!(outer, STATUS_OK);
    assert!(release.is_empty());
    plugin.run_cleanup();
    plugin.close();
}

#[test]
fn destroy_rejects_an_instance_with_an_active_callback() {
    let _serial = lock(&TEST_LOCK);
    let host = HostHarness::new();
    let plugin = PluginHarness::create(&host, "block");
    assert_eq!(plugin.activate().0, STATUS_OK);
    let call = RawServeCall {
        state: plugin.table.state.addr(),
        exchange: plugin.table.exchange.expect("plugin exchange"),
        instance: plugin.instance,
    };
    let (state, changed) =
        BLOCK.get_or_init(|| (Mutex::new(BlockState::default()), Condvar::new()));
    *lock(state) = BlockState::default();
    let worker = std::thread::spawn(move || call.invoke(77));
    let mut block = lock(state);
    while !block.entered {
        block = changed
            .wait(block)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    drop(block);

    let mut output: BasicOutput = unsafe { core::mem::zeroed() };
    let destroy_status = exchange(
        plugin.table,
        PLUGIN_DESTROY_INSTANCE,
        &CapInput {
            header: frame::<CapInput>(),
            capability: plugin.instance,
        },
        &mut output,
    );
    release_plugin_output(plugin.table, output.prefix.release);

    let mut block = lock(state);
    block.released = true;
    changed.notify_all();
    drop(block);
    let (serve_status, release) = worker.join().expect("active callback joins");
    assert_eq!(serve_status, STATUS_OK);
    release_plugin_output(plugin.table, release);
    assert_eq!(
        destroy_status, STATUS_BUSY,
        "destroy must not consume an instance while its callback owns the gate"
    );

    plugin.run_cleanup();
    plugin.close();
}

#[test]
fn callback_host_fails_closed_after_the_callback_returns() {
    let _serial = lock(&TEST_LOCK);
    let host = HostHarness::new();
    let plugin = PluginHarness::create(&host, "capture");
    assert_eq!(plugin.activate().0, STATUS_OK);
    let captured_host = lock(&CAPTURED_HOST).take().expect("captured host");
    let capability = lock(&CAPTURED_CAPABILITY)
        .take()
        .expect("captured capability");
    let error = captured_host
        .open(&capability)
        .expect_err("callback was sealed");
    assert_eq!(error.status(), STATUS_STALE_CAPABILITY);
    drop(capability);
    plugin.run_cleanup();
    plugin.close();
}

fn activation_cap() -> CapId {
    cap(1, CAP_KIND_ACTIVATION, RIGHT_MUTATE)
}
fn injection_cap() -> CapId {
    cap(2, CAP_KIND_SERVICE, RIGHT_RETAIN | RIGHT_OPEN)
}
fn secondary_cap() -> CapId {
    cap(5, CAP_KIND_SERVICE, RIGHT_RETAIN | RIGHT_OPEN)
}
fn effect_cap() -> CapId {
    cap(3, CAP_KIND_EFFECT_TXN, RIGHT_MUTATE)
}
fn provided_cap() -> CapId {
    cap(4, CAP_KIND_SERVICE, RIGHT_RETAIN | RIGHT_OPEN)
}
fn provider_channel_cap() -> CapId {
    cap(6, CAP_KIND_PROVIDER_CHANNEL, RIGHT_RECEIVE | RIGHT_SEND)
}
static MESSAGE_CAPABILITY: CapId = CapId {
    issuer: 900,
    slot: 7,
    epoch: 1,
    kind: CAP_KIND_SERVICE,
    rights: RIGHT_RETAIN | RIGHT_OPEN,
};
fn message_cap() -> CapId {
    MESSAGE_CAPABILITY
}
fn opened_channel_cap() -> CapId {
    cap(
        8,
        CAP_KIND_CALL_CHANNEL,
        RIGHT_RECEIVE | RIGHT_SEND | RIGHT_FINISH,
    )
}
fn nested_channel_cap() -> CapId {
    cap(
        9,
        CAP_KIND_CALL_CHANNEL,
        RIGHT_RECEIVE | RIGHT_SEND | RIGHT_FINISH,
    )
}
fn cap(slot: u64, kind: u32, rights: u32) -> CapId {
    CapId {
        issuer: 900,
        slot,
        epoch: 1,
        kind,
        rights,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn frame<T>() -> FrameHeader {
    FrameHeader::new(size_u32::<T>())
}
fn size_u32<T>() -> u32 {
    u32::try_from(size_of::<T>()).unwrap()
}
fn retained_string_bytes(value: &str) -> u64 {
    u64::try_from(size_of::<String>() + value.len()).unwrap()
}
fn raw(bytes: &[u8]) -> RawBytes {
    RawBytes {
        ptr: if bytes.is_empty() {
            core::ptr::null()
        } else {
            bytes.as_ptr()
        },
        len: u64::try_from(bytes.len()).unwrap(),
    }
}
unsafe fn bytes(raw: RawBytes) -> &'static [u8] {
    if raw.len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(raw.ptr, usize::try_from(raw.len).unwrap()) }
    }
}

unsafe fn read<T: Copy>(input: *const c_void, size: u32) -> Option<T> {
    if input.is_null() || size != size_u32::<T>() || !input.addr().is_multiple_of(align_of::<T>()) {
        None
    } else {
        Some(unsafe { input.cast::<T>().read() })
    }
}
fn write<T>(output: *mut c_void, capacity: u32, value: T) -> u32 {
    if output.is_null() || capacity < size_u32::<T>() {
        STATUS_BUFFER_TOO_SMALL
    } else {
        unsafe { output.cast::<T>().write(value) };
        STATUS_OK
    }
}
fn write_basic(output: *mut c_void, capacity: u32) -> u32 {
    write(
        output,
        capacity,
        BasicOutput {
            prefix: OutputPrefix::empty(size_u32::<BasicOutput>()),
        },
    )
}

fn exchange<I, O>(table: PluginTable, opcode: u32, input: &I, output: &mut O) -> u32 {
    unsafe {
        table.exchange.unwrap()(
            table.state,
            opcode,
            std::ptr::from_ref(input).cast(),
            size_u32::<I>(),
            std::ptr::from_mut(output).cast(),
            size_u32::<O>(),
        )
    }
}
fn status_only<I>(table: PluginTable, opcode: u32, input: &I) -> u32 {
    unsafe {
        table.exchange.unwrap()(
            table.state,
            opcode,
            std::ptr::from_ref(input).cast(),
            size_u32::<I>(),
            core::ptr::null_mut(),
            0,
        )
    }
}
fn release_plugin_output(table: PluginTable, release: ReleaseId) {
    if !release.is_empty() {
        assert_eq!(
            status_only(
                table,
                PLUGIN_RELEASE_OUTPUT,
                &ReleaseOutputInput {
                    header: frame::<ReleaseOutputInput>(),
                    release
                }
            ),
            STATUS_OK
        );
    }
}
fn retain_and_release(table: PluginTable, capability: CapId, release: ReleaseId) {
    let mut output: BasicOutput = unsafe { core::mem::zeroed() };
    assert_eq!(
        exchange(
            table,
            PLUGIN_CAP_RETAIN,
            &CapInput {
                header: frame::<CapInput>(),
                capability
            },
            &mut output
        ),
        STATUS_OK
    );
    release_plugin_output(table, release);
}
