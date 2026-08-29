use super::admission::{AdmissionError, AdmissionGate};
use super::cap_table::CapTable;
use super::host_channel::{CallerChannel, ChannelError, ProviderBridge};
use super::lifecycle::{
    CleanupDisposition, CleanupLease, CleanupMoveResolution, ModuleControl, NativeEndpoint,
    NativeInstance,
};
use super::output_table::OutputTable;
use super::transport::{PluginTransport, size_u32};
use crate::catalog_resources::HostResourceLedger;
use crate::panic_containment::drop_caught_payload;
use crate::{
    LoaderError, MAX_NATIVE_DIAGNOSTIC_BYTES, MAX_NATIVE_IDENTITY_BYTES, MAX_NATIVE_MESSAGE_BYTES,
    MAX_NATIVE_MESSAGE_CAPABILITIES,
};
use core::ffi::c_void;
use rsi_meta::{Capability, Context, ContractVersion, DetachedCapability, Message};
use rsi_meta_native::{
    BasicOutput, BoolOutput, BorrowedCapOutput, CAP_KIND_ACTIVATION, CAP_KIND_CALL_CHANNEL,
    CAP_KIND_EFFECT_TXN, CAP_KIND_PROVIDER_CHANNEL, CAP_KIND_SERVICE, CapId, CapInput,
    EffectDeferInput, HOST_CAP_OPEN, HOST_CAP_RELEASE, HOST_CAP_RETAIN, HOST_CHANNEL_CANCELLED,
    HOST_CHANNEL_FINISH_REQUESTS, HOST_CHANNEL_RECV, HOST_CHANNEL_SEND, HOST_CHANNEL_TERMINAL,
    HOST_EFFECT_ABORT, HOST_EFFECT_BEGIN, HOST_EFFECT_COMMIT, HOST_EFFECT_DEFER, HOST_PROVIDE,
    HOST_RELEASE_OUTPUT, HostTable, MessageInput, MessageOutput, OpenInput, OutputPrefix,
    ProvideInput, RIGHT_FINISH, RIGHT_MUTATE, RIGHT_OPEN, RIGHT_RECEIVE, RIGHT_RETAIN, RIGHT_SEND,
    RawBytes, RawMessage, ReleaseId, ReleaseOutputInput, STATUS_BUSY, STATUS_INVALID_ARGUMENT,
    STATUS_LIMIT_EXCEEDED, STATUS_OK, STATUS_PANICKED, STATUS_PROTOCOL_ERROR, STATUS_REENTRANT,
    STATUS_STALE_CAPABILITY, STATUS_TERMINAL, STATUS_UNSUPPORTED, STATUS_WRONG_CAPABILITY,
    TableHeader,
};
use std::mem::{align_of, size_of};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio::runtime::Handle;

mod status;

use status::output_error_status;

static NEXT_HOST_ISSUER: AtomicU64 = AtomicU64::new(1);

const SERVICE_RIGHTS: u32 = RIGHT_RETAIN | RIGHT_OPEN;
const CALLER_RIGHTS: u32 = RIGHT_RECEIVE | RIGHT_SEND | RIGHT_FINISH;
const PROVIDER_RIGHTS: u32 = RIGHT_RECEIVE | RIGHT_SEND;

/// Stable host table state. One raw Arc lease keeps its address valid until
/// host admission has drained and plugin FINALIZE proves no callback remains.
pub(super) struct HostLease {
    state: Arc<HostState>,
    raw: *const HostState,
}

// SAFETY: `raw` is an Arc allocation pointer whose persistent strong lease is
// released only after exclusive admission. HostState is Send + Sync.
unsafe impl Send for HostLease {}

impl HostLease {
    pub(super) fn new(
        maximum_capabilities: usize,
        maximum_outputs: usize,
        resources: Arc<HostResourceLedger>,
    ) -> Result<Self, LoaderError> {
        let issuer = next_nonzero(&NEXT_HOST_ISSUER, "host table issuer")?;
        let state = Arc::new(HostState {
            issuer,
            admission: Arc::new(AdmissionGate::new()),
            capabilities: Arc::new(CapTable::new(
                issuer,
                maximum_capabilities,
                Arc::clone(&resources),
            )),
            outputs: Arc::new(OutputTable::new(issuer, maximum_outputs, resources)),
        });
        let raw = Arc::into_raw(Arc::clone(&state));
        Ok(Self { state, raw })
    }

    pub(super) fn table(&self) -> HostTable {
        HostTable {
            header: TableHeader::new(rsi_meta_native::ABI_MINOR, HostTable::STRUCT_SIZE),
            issuer: self.state.issuer,
            state: self.raw.cast_mut().cast(),
            exchange: Some(host_exchange),
        }
    }

    pub(super) fn state(&self) -> &Arc<HostState> {
        &self.state
    }

    pub(super) fn finalize_plugin(
        &self,
        transport: &Arc<PluginTransport>,
    ) -> Result<(), LoaderError> {
        let host =
            self.state
                .admission
                .begin_exclusive()
                .map_err(|error| LoaderError::Protocol {
                    operation: "host finalization",
                    message: admission_message(error).to_owned(),
                })?;
        let result = transport.finalize();
        if result.is_ok() || transport.is_finalized() {
            host.finish();
        }
        result
    }

    pub(super) fn retire_without_plugin(&self) {
        if let Ok(claim) = self.state.admission.begin_exclusive() {
            claim.finish();
        }
    }

    pub(super) fn is_finalized(&self) -> bool {
        self.state.admission.is_finalized()
    }
}

impl Drop for HostLease {
    fn drop(&mut self) {
        debug_assert!(self.state.admission.is_finalized());
        // SAFETY: `raw` was created by exactly one Arc::into_raw in `new`; the
        // permanently closed and drained host gate proves no exchange can race
        // releasing this persistent lease.
        drop(unsafe { Arc::from_raw(self.raw) });
    }
}

pub(super) struct HostState {
    issuer: u64,
    admission: Arc<AdmissionGate>,
    capabilities: Arc<CapTable<HostCapability>>,
    outputs: Arc<OutputTable<HostOutput>>,
}

impl HostState {
    pub(super) fn callback_frame(&self, runtime: Handle) -> Arc<CallbackFrame> {
        Arc::new(CallbackFrame {
            runtime,
            capabilities: Arc::downgrade(&self.capabilities),
            state: Mutex::new(CallbackState::default()),
        })
    }

    pub(super) fn insert_service(
        &self,
        capability: Capability,
    ) -> Result<OwnedHostCap, LoaderError> {
        let id = self
            .capabilities
            .insert(
                CAP_KIND_SERVICE,
                SERVICE_RIGHTS,
                Arc::new(HostCapability::Service(capability.detach())),
            )
            .map_err(|(error, _)| error)?;
        Ok(OwnedHostCap {
            id,
            table: Arc::clone(&self.capabilities),
        })
    }

    pub(super) fn activation_cap(
        frame: &Arc<CallbackFrame>,
        context: Context,
        module: Weak<ModuleControl>,
        instance: Weak<NativeInstance>,
    ) -> Result<(CapId, Arc<ActivationFrame>), LoaderError> {
        let activation = Arc::new(ActivationFrame {
            frame: Arc::clone(frame),
            context,
            module,
            instance,
            effect: Mutex::new(None),
        });
        let id = frame.insert(
            CAP_KIND_ACTIVATION,
            RIGHT_MUTATE,
            HostCapability::Activation(Arc::clone(&activation)),
        )?;
        Ok((id, activation))
    }

    pub(super) fn provider_cap(
        frame: &Arc<CallbackFrame>,
        provider: Arc<ProviderBridge>,
    ) -> Result<CapId, LoaderError> {
        frame.insert(
            CAP_KIND_PROVIDER_CHANNEL,
            PROVIDER_RIGHTS,
            HostCapability::Provider(provider),
        )
    }

    fn dispatch(
        &self,
        opcode: u32,
        input: *const c_void,
        input_size: u32,
        output: *mut c_void,
        output_capacity: u32,
    ) -> u32 {
        if opcode == HOST_RELEASE_OUTPUT {
            return self.release_output(input, input_size, output, output_capacity);
        }
        let expected = output_size(opcode);
        if expected == 0 {
            return STATUS_UNSUPPORTED;
        }
        if !valid_output(output, output_capacity, expected) {
            return rsi_meta_native::STATUS_BUFFER_TOO_SMALL;
        }
        let result = match opcode {
            HOST_CAP_RETAIN => self.cap_retain(input, input_size),
            HOST_CAP_RELEASE => self.cap_release(input, input_size),
            HOST_CAP_OPEN => self.cap_open(input, input_size),
            HOST_CHANNEL_RECV => self.channel_recv(input, input_size),
            HOST_CHANNEL_SEND => self.channel_send(input, input_size),
            HOST_CHANNEL_FINISH_REQUESTS => self.channel_finish(input, input_size),
            HOST_CHANNEL_TERMINAL => self.channel_terminal(input, input_size),
            HOST_CHANNEL_CANCELLED => self.channel_cancelled(input, input_size),
            HOST_EFFECT_BEGIN => self.effect_begin(input, input_size),
            HOST_EFFECT_DEFER => self.effect_defer(input, input_size),
            HOST_EFFECT_COMMIT => self.effect_close(input, input_size, EffectPhase::Committed),
            HOST_EFFECT_ABORT => self.effect_close(input, input_size, EffectPhase::Aborted),
            HOST_PROVIDE => self.provide(input, input_size),
            _ => Err(HostFailure::new(
                STATUS_UNSUPPORTED,
                "unknown host operation",
            )),
        };
        self.write_result(result, output, output_capacity, expected)
    }

    fn cap_retain(&self, input: *const c_void, input_size: u32) -> HostResult {
        let input = read_frame::<CapInput>(input, input_size)?;
        self.capabilities
            .retain(input.capability)
            .map_err(HostFailure::from_cap)?;
        Ok(HostOutputValue::Basic)
    }

    fn cap_release(&self, input: *const c_void, input_size: u32) -> HostResult {
        let input = read_frame::<CapInput>(input, input_size)?;
        let retired = self
            .capabilities
            .release(input.capability)
            .map_err(HostFailure::from_cap)?;
        drop(retired);
        Ok(HostOutputValue::Basic)
    }

    fn cap_open(&self, input: *const c_void, input_size: u32) -> HostResult {
        let input = read_frame::<OpenInput>(input, input_size)?;
        let service = self
            .capabilities
            .get_exact(input.service, CAP_KIND_SERVICE, SERVICE_RIGHTS)
            .map_err(HostFailure::from_cap)?;
        let HostCapability::Service(service) = service.as_ref() else {
            unreachable!("cap table metadata matches host value")
        };
        self.with_open_scope(input.scope, |frame| {
            let call = service
                .upgrade()
                .and_then(|service| service.open())
                .map_err(HostFailure::from_core)?;
            let caller = CallerChannel::new(Arc::clone(frame), frame.runtime.clone(), call);
            let capability = frame
                .insert(
                    CAP_KIND_CALL_CHANNEL,
                    CALLER_RIGHTS,
                    HostCapability::Caller(caller),
                )
                .map_err(HostFailure::from_loader)?;
            Ok(HostOutputValue::BorrowedCap(capability))
        })
    }

    fn channel_recv(&self, input: *const c_void, input_size: u32) -> HostResult {
        let input = read_frame::<CapInput>(input, input_size)?;
        let capability = self.channel(input.capability)?;
        let message = match capability.as_ref() {
            HostCapability::Caller(channel) => {
                channel.receive().map_err(HostFailure::from_channel)?
            }
            HostCapability::Provider(channel) => channel
                .receive(&channel.frame().runtime)
                .map_err(HostFailure::from_channel)?,
            _ => unreachable!("channel metadata matches value"),
        };
        message.map_or(Ok(HostOutputValue::Message(None)), |message| {
            self.export_message(message)
                .map(|record| HostOutputValue::Message(Some(record)))
        })
    }

    fn channel_send(&self, input: *const c_void, input_size: u32) -> HostResult {
        let input = read_frame::<MessageInput>(input, input_size)?;
        let channel = self.channel(input.channel)?;
        let message = self.import_message(input.message)?;
        match channel.as_ref() {
            HostCapability::Caller(channel) => {
                channel.send(message).map_err(HostFailure::from_channel)?;
            }
            HostCapability::Provider(channel) => {
                channel
                    .send(&channel.frame().runtime, message)
                    .map_err(HostFailure::from_channel)?;
            }
            _ => unreachable!("channel metadata matches value"),
        }
        Ok(HostOutputValue::Basic)
    }

    fn channel_finish(&self, input: *const c_void, input_size: u32) -> HostResult {
        let input = read_frame::<CapInput>(input, input_size)?;
        let channel = self
            .capabilities
            .get_exact(input.capability, CAP_KIND_CALL_CHANNEL, CALLER_RIGHTS)
            .map_err(HostFailure::from_cap)?;
        let HostCapability::Caller(channel) = channel.as_ref() else {
            unreachable!("channel metadata matches value")
        };
        channel.frame().ensure_open()?;
        channel.finish().map_err(HostFailure::from_channel)?;
        Ok(HostOutputValue::Basic)
    }

    fn channel_terminal(&self, input: *const c_void, input_size: u32) -> HostResult {
        let input = read_frame::<CapInput>(input, input_size)?;
        let channel = self
            .capabilities
            .get_exact(input.capability, CAP_KIND_CALL_CHANNEL, CALLER_RIGHTS)
            .map_err(HostFailure::from_cap)?;
        let HostCapability::Caller(channel) = channel.as_ref() else {
            unreachable!("channel metadata matches value")
        };
        channel.frame().ensure_open()?;
        let terminal = channel.terminal().map_err(HostFailure::from_channel)?;
        if terminal.status == STATUS_OK {
            Ok(HostOutputValue::Basic)
        } else {
            Err(HostFailure::new(terminal.status, terminal.diagnostic))
        }
    }

    fn channel_cancelled(&self, input: *const c_void, input_size: u32) -> HostResult {
        let input = read_frame::<CapInput>(input, input_size)?;
        let channel = self.channel(input.capability)?;
        let value = match channel.as_ref() {
            HostCapability::Caller(channel) => channel.cancelled(),
            HostCapability::Provider(channel) => channel
                .cancelled(&channel.frame().runtime)
                .map_err(HostFailure::from_channel)?,
            _ => unreachable!("channel metadata matches value"),
        };
        Ok(HostOutputValue::Bool(value))
    }

    fn effect_begin(&self, input: *const c_void, input_size: u32) -> HostResult {
        let input = read_frame::<CapInput>(input, input_size)?;
        let activation = self
            .capabilities
            .get_exact(input.capability, CAP_KIND_ACTIVATION, RIGHT_MUTATE)
            .map_err(HostFailure::from_cap)?;
        let HostCapability::Activation(activation) = activation.as_ref() else {
            unreachable!("activation metadata matches value")
        };
        activation.frame.ensure_open()?;
        let mut effect = activation
            .effect
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if effect.is_some() {
            return Err(HostFailure::new(
                STATUS_PROTOCOL_ERROR,
                "activation effect transaction already began",
            ));
        }
        let value = Arc::new(EffectFrame {
            frame: Arc::clone(&activation.frame),
            context: activation.context.clone(),
            module: activation.module.clone(),
            instance: activation.instance.clone(),
            phase: Mutex::new(EffectPhase::Open),
        });
        let capability = activation
            .frame
            .insert(
                CAP_KIND_EFFECT_TXN,
                RIGHT_MUTATE,
                HostCapability::Effect(Arc::clone(&value)),
            )
            .map_err(HostFailure::from_loader)?;
        *effect = Some(value);
        Ok(HostOutputValue::BorrowedCap(capability))
    }

    fn effect_defer(&self, input: *const c_void, input_size: u32) -> HostResult {
        let input = read_frame::<EffectDeferInput>(input, input_size)?;
        let effect = self.effect(input.transaction)?;
        let label = copy_utf8(input.label, MAX_NATIVE_IDENTITY_BYTES, "cleanup label")?;
        if input.cleanup.kind != rsi_meta_native::CAP_KIND_CLEANUP
            || input.cleanup.rights != RIGHT_MUTATE
        {
            return Err(HostFailure::new(
                STATUS_WRONG_CAPABILITY,
                "cleanup capability has wrong kind or rights",
            ));
        }
        let module = effect.module.upgrade().ok_or_else(|| {
            HostFailure::new(STATUS_STALE_CAPABILITY, "native module is retiring")
        })?;
        if input.cleanup.issuer != module.transport().issuer() {
            return Err(HostFailure::new(
                STATUS_STALE_CAPABILITY,
                "cleanup capability belongs to another plugin table",
            ));
        }
        // Serialize the complete effect mutation with COMMIT/ABORT. A snapshot
        // check would let a concurrent close succeed between validation and
        // registering the cleanup.
        let _mutation = effect.begin_mutation()?;
        let (cleanup, moved) = CleanupLease::new(module, input.cleanup);
        let resolution = CleanupMoveResolution::new(moved);
        let result = effect
            .context
            .defer(label, Box::new(move || cleanup.into_future()));
        match result {
            Ok(()) => {
                resolution.resolve(CleanupDisposition::Armed);
                Ok(HostOutputValue::Basic)
            }
            Err(error) => {
                resolution.resolve(CleanupDisposition::Rejected);
                Err(HostFailure::from_core(error))
            }
        }
    }

    fn effect_close(&self, input: *const c_void, input_size: u32, next: EffectPhase) -> HostResult {
        let input = read_frame::<CapInput>(input, input_size)?;
        let effect = self.effect(input.capability)?;
        let mut phase = effect
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *phase != EffectPhase::Open {
            return Err(HostFailure::new(
                STATUS_PROTOCOL_ERROR,
                "effect transaction is already closed",
            ));
        }
        *phase = next;
        Ok(HostOutputValue::Basic)
    }

    fn provide(&self, input: *const c_void, input_size: u32) -> HostResult {
        let input = read_frame::<ProvideInput>(input, input_size)?;
        let effect = self.effect(input.transaction)?;
        let key = copy_utf8(input.key, MAX_NATIVE_IDENTITY_BYTES, "service key")?;
        let contract = copy_utf8(
            input.contract,
            MAX_NATIVE_IDENTITY_BYTES,
            "service contract",
        )?;
        let port = copy_owned(input.port, MAX_NATIVE_IDENTITY_BYTES, "native port")?;
        let version = u32::try_from(input.version).map_err(|_| {
            HostFailure::new(STATUS_INVALID_ARGUMENT, "service version exceeds u32")
        })?;
        let instance = effect.instance.upgrade().ok_or_else(|| {
            HostFailure::new(STATUS_STALE_CAPABILITY, "native instance is retiring")
        })?;
        // Keep COMMIT/ABORT outside the entire transaction, including host
        // reservation and core publication.
        let _mutation = effect.begin_mutation()?;
        let endpoint = Arc::new(NativeEndpoint::new(Arc::downgrade(&instance), port));
        // Reserve every host-owned resource before the core supply registry can
        // observe the endpoint. Once `provide_and_capture` succeeds, both
        // fills are allocation-free and cannot reject the publication.
        let capability_slot = self
            .capabilities
            .reserve(CAP_KIND_SERVICE, SERVICE_RIGHTS)
            .map_err(HostFailure::from_loader)?;
        let output_slot = self.outputs.reserve(0).map_err(HostFailure::from_loader)?;
        let (_supply, capability) = effect
            .context
            .provide_and_capture(&key, &contract, ContractVersion(version), endpoint)
            .map_err(HostFailure::from_core)?;
        let id = capability_slot.fill(Arc::new(HostCapability::Service(capability.detach())));
        let record = Arc::new(HostOutput {
            diagnostic: Box::default(),
            bytes: Box::default(),
            capabilities: Box::default(),
            leases: vec![OwnedHostCap {
                id,
                table: Arc::clone(&self.capabilities),
            }],
        });
        let release = output_slot.fill(Arc::clone(&record));
        Ok(HostOutputValue::PublishedCap { record, release })
    }

    fn channel(&self, id: CapId) -> Result<Arc<HostCapability>, HostFailure> {
        let value = match (id.kind, id.rights) {
            (CAP_KIND_CALL_CHANNEL, CALLER_RIGHTS) => self
                .capabilities
                .get_exact(id, CAP_KIND_CALL_CHANNEL, CALLER_RIGHTS)
                .map_err(HostFailure::from_cap),
            (CAP_KIND_PROVIDER_CHANNEL, PROVIDER_RIGHTS) => self
                .capabilities
                .get_exact(id, CAP_KIND_PROVIDER_CHANNEL, PROVIDER_RIGHTS)
                .map_err(HostFailure::from_cap),
            _ => Err(HostFailure::new(
                STATUS_WRONG_CAPABILITY,
                "capability is not an exact caller or provider channel",
            )),
        }?;
        value
            .frame()
            .expect("channel capabilities always retain a callback frame")
            .ensure_open()?;
        Ok(value)
    }

    fn with_open_scope<R>(
        &self,
        id: CapId,
        operation: impl FnOnce(&Arc<CallbackFrame>) -> Result<R, HostFailure>,
    ) -> Result<R, HostFailure> {
        let value = match (id.kind, id.rights) {
            (CAP_KIND_EFFECT_TXN, RIGHT_MUTATE) => self
                .capabilities
                .get_exact(id, CAP_KIND_EFFECT_TXN, RIGHT_MUTATE)
                .map_err(HostFailure::from_cap)?,
            (CAP_KIND_PROVIDER_CHANNEL, PROVIDER_RIGHTS) => self
                .capabilities
                .get_exact(id, CAP_KIND_PROVIDER_CHANNEL, PROVIDER_RIGHTS)
                .map_err(HostFailure::from_cap)?,
            (CAP_KIND_CALL_CHANNEL, CALLER_RIGHTS) => self
                .capabilities
                .get_exact(id, CAP_KIND_CALL_CHANNEL, CALLER_RIGHTS)
                .map_err(HostFailure::from_cap)?,
            _ => {
                return Err(HostFailure::new(
                    STATUS_WRONG_CAPABILITY,
                    "CAP_OPEN scope must be an exact effect, provider, or caller channel",
                ));
            }
        };
        match value.as_ref() {
            HostCapability::Effect(effect) => {
                let _mutation = effect.begin_mutation()?;
                operation(&effect.frame)
            }
            HostCapability::Provider(provider) => {
                provider.frame().ensure_open()?;
                operation(provider.frame())
            }
            HostCapability::Caller(caller) => {
                caller.frame().ensure_open()?;
                operation(caller.frame())
            }
            _ => unreachable!("open-scope metadata matches callback value"),
        }
    }

    fn effect(&self, id: CapId) -> Result<Arc<EffectFrame>, HostFailure> {
        let value = self
            .capabilities
            .get_exact(id, CAP_KIND_EFFECT_TXN, RIGHT_MUTATE)
            .map_err(HostFailure::from_cap)?;
        let HostCapability::Effect(effect) = value.as_ref() else {
            unreachable!("effect metadata matches value")
        };
        effect.frame.ensure_open()?;
        Ok(Arc::clone(effect))
    }

    fn import_message(&self, raw: RawMessage) -> Result<Message, HostFailure> {
        let (byte_count, capability_count) = raw
            .validate_shape(MAX_NATIVE_MESSAGE_BYTES, MAX_NATIVE_MESSAGE_CAPABILITIES)
            .map_err(|error| HostFailure::new(STATUS_INVALID_ARGUMENT, error.to_string()))?;
        let bytes = copy_range(raw.bytes.ptr, byte_count);
        let ids = copy_cap_ids(raw.capabilities, capability_count);
        let values = self
            .capabilities
            .get_many_exact(&ids, CAP_KIND_SERVICE, SERVICE_RIGHTS)
            .map_err(HostFailure::from_cap)?;
        let capabilities: Vec<Capability> = values
            .into_iter()
            .map(|value| {
                let HostCapability::Service(capability) = value.as_ref() else {
                    unreachable!("service metadata matches value")
                };
                capability.upgrade().map_err(HostFailure::from_core)
            })
            .collect::<Result<_, _>>()?;
        Ok(Message::from_parts(bytes, capabilities))
    }

    fn export_message(&self, message: Message) -> Result<Arc<HostOutput>, HostFailure> {
        let (bytes, capabilities) = message.into_parts();
        let mut ids = Vec::with_capacity(capabilities.len());
        let mut leases = Vec::with_capacity(capabilities.len());
        for capability in capabilities {
            let lease = self
                .insert_service(capability)
                .map_err(HostFailure::from_loader)?;
            ids.push(lease.id);
            leases.push(lease);
        }
        Ok(Arc::new(HostOutput {
            diagnostic: Box::default(),
            bytes: bytes.into_boxed_slice(),
            capabilities: ids.into_boxed_slice(),
            leases,
        }))
    }

    fn release_output(
        &self,
        input: *const c_void,
        input_size: u32,
        output: *mut c_void,
        output_capacity: u32,
    ) -> u32 {
        if !output.is_null() || output_capacity != 0 {
            return STATUS_INVALID_ARGUMENT;
        }
        let input = match read_frame::<ReleaseOutputInput>(input, input_size) {
            Ok(value) => value,
            Err(error) => return error.status,
        };
        match self.outputs.release(input.release) {
            Ok(record) => {
                drop(record);
                STATUS_OK
            }
            Err(error) => output_error_status(error),
        }
    }

    fn write_result(
        &self,
        result: HostResult,
        output: *mut c_void,
        output_capacity: u32,
        expected: u32,
    ) -> u32 {
        match result {
            Ok(value) => self.write_success(value, output, output_capacity, expected),
            Err(failure) => {
                self.write_failure(&failure, output, output_capacity, expected);
                failure.status
            }
        }
    }

    fn write_success(
        &self,
        value: HostOutputValue,
        output: *mut c_void,
        output_capacity: u32,
        expected: u32,
    ) -> u32 {
        match value {
            HostOutputValue::Basic => write_value(
                output,
                output_capacity,
                BasicOutput {
                    prefix: empty_prefix(expected),
                },
            ),
            HostOutputValue::BorrowedCap(capability) => write_value(
                output,
                output_capacity,
                BorrowedCapOutput {
                    prefix: empty_prefix(expected),
                    capability,
                },
            ),
            HostOutputValue::PublishedCap { record, release } => {
                let capability = record.leases[0].id;
                write_value(
                    output,
                    output_capacity,
                    rsi_meta_native::CapOutput {
                        prefix: record.prefix(expected, release),
                        capability,
                    },
                )
            }
            HostOutputValue::Message(None) => write_value(
                output,
                output_capacity,
                MessageOutput {
                    prefix: empty_prefix(expected),
                    present: 0,
                    reserved: 0,
                    message: RawMessage {
                        bytes: RawBytes::EMPTY,
                        capabilities: core::ptr::null(),
                        capability_count: 0,
                    },
                },
            ),
            HostOutputValue::Message(Some(record)) => {
                let release = match self.publish(Arc::clone(&record)) {
                    Ok(value) => value,
                    Err(error) => {
                        return Self::write_capacity_failure(
                            error,
                            output,
                            output_capacity,
                            expected,
                        );
                    }
                };
                write_value(
                    output,
                    output_capacity,
                    MessageOutput {
                        prefix: record.prefix(expected, release),
                        present: 1,
                        reserved: 0,
                        message: record.raw_message(),
                    },
                )
            }
            HostOutputValue::Bool(value) => write_value(
                output,
                output_capacity,
                BoolOutput {
                    prefix: empty_prefix(expected),
                    value: u32::from(value),
                    reserved: 0,
                },
            ),
        }
    }

    fn write_failure(
        &self,
        failure: &HostFailure,
        output: *mut c_void,
        output_capacity: u32,
        expected: u32,
    ) {
        let diagnostic = truncate(failure.message.as_bytes(), MAX_NATIVE_DIAGNOSTIC_BYTES);
        if diagnostic.is_empty() {
            write_prefix(output, output_capacity, empty_prefix(expected));
            return;
        }
        let record = Arc::new(HostOutput {
            diagnostic: diagnostic.into_boxed_slice(),
            bytes: Box::default(),
            capabilities: Box::default(),
            leases: Vec::new(),
        });
        if let Ok(release) = self.publish(Arc::clone(&record)) {
            write_prefix(output, output_capacity, record.prefix(expected, release));
        } else {
            write_prefix(output, output_capacity, empty_prefix(expected));
        }
    }

    fn write_capacity_failure(
        _error: LoaderError,
        output: *mut c_void,
        output_capacity: u32,
        expected: u32,
    ) -> u32 {
        write_prefix(output, output_capacity, empty_prefix(expected));
        STATUS_LIMIT_EXCEEDED
    }

    fn publish(&self, record: Arc<HostOutput>) -> Result<ReleaseId, LoaderError> {
        let bytes = record.retained_bytes()?;
        self.outputs
            .insert(bytes, record)
            .map_err(|(error, _)| error)
    }
}

pub(super) struct CallbackFrame {
    pub(super) runtime: Handle,
    capabilities: Weak<CapTable<HostCapability>>,
    state: Mutex<CallbackState>,
}

#[derive(Default)]
struct CallbackState {
    sealed: bool,
    ids: Vec<CapId>,
}

impl CallbackFrame {
    fn insert(
        self: &Arc<Self>,
        kind: u32,
        rights: u32,
        value: HostCapability,
    ) -> Result<CapId, LoaderError> {
        let table = self
            .capabilities
            .upgrade()
            .ok_or_else(|| LoaderError::Protocol {
                operation: "callback capability insertion",
                message: "host capability table is gone".to_owned(),
            })?;
        let id = table
            .insert(kind, rights, Arc::new(value))
            .map_err(|(error, _)| error)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.sealed {
            drop(state);
            let retired = table.seal(id).expect("fresh callback cap remains exact");
            drop(retired);
            return Err(LoaderError::Protocol {
                operation: "callback capability insertion",
                message: "callback frame is sealed".to_owned(),
            });
        }
        state.ids.push(id);
        Ok(id)
    }

    pub(super) fn ensure_open(&self) -> Result<(), HostFailure> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.sealed {
            Err(HostFailure::new(
                STATUS_STALE_CAPABILITY,
                "callback frame is sealed",
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn seal(&self) {
        let ids = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.sealed {
                return;
            }
            state.sealed = true;
            std::mem::take(&mut state.ids)
        };
        let Some(table) = self.capabilities.upgrade() else {
            return;
        };
        for id in ids {
            if let Ok(value) = table.seal(id) {
                drop(value);
            }
        }
    }
}

impl Drop for CallbackFrame {
    fn drop(&mut self) {
        self.seal();
    }
}

enum HostCapability {
    Service(DetachedCapability),
    Activation(Arc<ActivationFrame>),
    Effect(Arc<EffectFrame>),
    Caller(Arc<CallerChannel>),
    Provider(Arc<ProviderBridge>),
}

impl HostCapability {
    fn frame(&self) -> Option<&Arc<CallbackFrame>> {
        match self {
            Self::Service(_) => None,
            Self::Activation(value) => Some(&value.frame),
            Self::Effect(value) => Some(&value.frame),
            Self::Caller(value) => Some(value.frame()),
            Self::Provider(value) => Some(value.frame()),
        }
    }
}

pub(super) struct ActivationFrame {
    frame: Arc<CallbackFrame>,
    context: Context,
    module: Weak<ModuleControl>,
    instance: Weak<NativeInstance>,
    effect: Mutex<Option<Arc<EffectFrame>>>,
}

impl ActivationFrame {
    pub(super) fn accepted(&self) -> bool {
        self.effect
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|effect| {
                *effect
                    .phase
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    == EffectPhase::Committed
            })
    }

    pub(super) fn seal(&self) {
        self.frame.seal();
    }
}

struct EffectFrame {
    frame: Arc<CallbackFrame>,
    context: Context,
    module: Weak<ModuleControl>,
    instance: Weak<NativeInstance>,
    phase: Mutex<EffectPhase>,
}

impl EffectFrame {
    fn begin_mutation(&self) -> Result<std::sync::MutexGuard<'_, EffectPhase>, HostFailure> {
        self.frame.ensure_open()?;
        let phase = self
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *phase != EffectPhase::Open {
            return Err(HostFailure::new(
                STATUS_PROTOCOL_ERROR,
                "effect transaction is closed",
            ));
        }
        Ok(phase)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum EffectPhase {
    Open,
    Committed,
    Aborted,
}

pub(super) struct OwnedHostCap {
    pub(super) id: CapId,
    table: Arc<CapTable<HostCapability>>,
}

impl Drop for OwnedHostCap {
    fn drop(&mut self) {
        if let Ok(value) = self.table.release(self.id) {
            drop(value);
        }
    }
}

struct HostOutput {
    diagnostic: Box<[u8]>,
    bytes: Box<[u8]>,
    capabilities: Box<[CapId]>,
    leases: Vec<OwnedHostCap>,
}

impl HostOutput {
    fn retained_bytes(&self) -> Result<u64, LoaderError> {
        let bytes = self
            .diagnostic
            .len()
            .checked_add(self.bytes.len())
            .and_then(|value| {
                self.capabilities
                    .len()
                    .checked_mul(size_of::<CapId>())
                    .and_then(|caps| value.checked_add(caps))
            })
            .ok_or(LoaderError::CapacityExhausted {
                resource: "host output bytes",
                limit: u64::MAX,
            })?;
        u64::try_from(bytes).map_err(|_| LoaderError::CapacityExhausted {
            resource: "host output bytes",
            limit: u64::MAX,
        })
    }

    fn prefix(&self, struct_size: u32, release: ReleaseId) -> OutputPrefix {
        OutputPrefix {
            struct_size,
            reserved: 0,
            release,
            diagnostic: raw_bytes(&self.diagnostic),
        }
    }

    fn raw_message(&self) -> RawMessage {
        RawMessage {
            bytes: raw_bytes(&self.bytes),
            capabilities: if self.capabilities.is_empty() {
                core::ptr::null()
            } else {
                self.capabilities.as_ptr()
            },
            capability_count: u64::try_from(self.capabilities.len()).unwrap_or(u64::MAX),
        }
    }
}

enum HostOutputValue {
    Basic,
    BorrowedCap(CapId),
    PublishedCap {
        record: Arc<HostOutput>,
        release: ReleaseId,
    },
    Message(Option<Arc<HostOutput>>),
    Bool(bool),
}

type HostResult = Result<HostOutputValue, HostFailure>;

pub(super) struct HostFailure {
    status: u32,
    message: String,
}

impl HostFailure {
    fn new(status: u32, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    #[allow(clippy::needless_pass_by_value)] // `map_err` transfers the one-shot error to formatting.
    fn from_core(error: rsi_meta::MetaError) -> Self {
        let status = match error {
            rsi_meta::MetaError::Busy { .. } => STATUS_BUSY,
            rsi_meta::MetaError::Reentrant { .. } => STATUS_REENTRANT,
            rsi_meta::MetaError::StaleCapability
            | rsi_meta::MetaError::StaleContext { .. }
            | rsi_meta::MetaError::StaleService { .. }
            | rsi_meta::MetaError::FiberDisposed { .. } => STATUS_STALE_CAPABILITY,
            rsi_meta::MetaError::CapacityExhausted { .. }
            | rsi_meta::MetaError::PayloadTooLarge { .. } => STATUS_LIMIT_EXCEEDED,
            rsi_meta::MetaError::InvalidInput(_) | rsi_meta::MetaError::InvalidConfig(_) => {
                STATUS_INVALID_ARGUMENT
            }
            rsi_meta::MetaError::Cancelled => rsi_meta_native::STATUS_CANCELLED,
            rsi_meta::MetaError::Timeout(_) => STATUS_TERMINAL,
            _ => rsi_meta_native::STATUS_FAILED,
        };
        Self::new(status, error.to_string())
    }

    fn from_channel(error: ChannelError) -> Self {
        match error {
            ChannelError::Stale => Self::new(STATUS_STALE_CAPABILITY, "callback channel is stale"),
            ChannelError::Busy => Self::new(STATUS_BUSY, "callback channel operation is busy"),
            ChannelError::Protocol(message) => Self::new(STATUS_PROTOCOL_ERROR, message),
            ChannelError::Core(error) => Self::from_core(error),
        }
    }

    #[allow(clippy::needless_pass_by_value)] // `map_err` transfers the one-shot error to formatting.
    fn from_loader(error: LoaderError) -> Self {
        match error {
            LoaderError::Busy { .. } => Self::new(STATUS_BUSY, error.to_string()),
            LoaderError::Reentrant { .. } => Self::new(STATUS_REENTRANT, error.to_string()),
            LoaderError::CapacityExhausted { .. } => {
                Self::new(STATUS_LIMIT_EXCEEDED, error.to_string())
            }
            _ => Self::new(STATUS_PROTOCOL_ERROR, error.to_string()),
        }
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
    let raw = state.cast::<HostState>().cast_const();
    // SAFETY: HostLease retains one raw strong reference until the permanently
    // closed host gate has drained. This temporary Arc pins state for the call.
    unsafe { Arc::increment_strong_count(raw) };
    // SAFETY: The increment above transferred one temporary strong reference.
    let state = unsafe { Arc::from_raw(raw) };
    let Ok(_admission) = state.admission.try_enter() else {
        return STATUS_STALE_CAPABILITY;
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.dispatch(opcode, input, input_size, output, output_capacity)
    }));
    match result {
        Ok(status) => status,
        Err(payload) => {
            drop_caught_payload(payload);
            STATUS_PANICKED
        }
    }
}

fn read_frame<T: Copy>(input: *const c_void, input_size: u32) -> Result<T, HostFailure> {
    if input.is_null()
        || input_size != size_u32::<T>()
        || !input.addr().is_multiple_of(align_of::<T>())
    {
        return Err(HostFailure::new(
            STATUS_INVALID_ARGUMENT,
            "host input frame has the wrong address, alignment, or size",
        ));
    }
    // SAFETY: The trusted plugin supplies a readable aligned exact-size frame
    // for the synchronous exchange, and T is Copy.
    let value = unsafe { input.cast::<T>().read() };
    // SAFETY: The same exact-size readable frame validated above begins with
    // the ABI FrameHeader shared by every host input record.
    let header = unsafe { input.cast::<rsi_meta_native::FrameHeader>().read() };
    if !header.is_compatible(size_u32::<T>(), input_size) {
        return Err(HostFailure::new(
            STATUS_INVALID_ARGUMENT,
            "host input frame header is incompatible",
        ));
    }
    Ok(value)
}

fn copy_utf8(raw: RawBytes, maximum: usize, name: &'static str) -> Result<String, HostFailure> {
    let bytes = copy_owned(raw, maximum, name)?;
    String::from_utf8(bytes)
        .map_err(|_| HostFailure::new(STATUS_INVALID_ARGUMENT, format!("{name} is not UTF-8")))
}

fn copy_owned(raw: RawBytes, maximum: usize, name: &'static str) -> Result<Vec<u8>, HostFailure> {
    let length = raw
        .checked_len(maximum)
        .map_err(|error| HostFailure::new(STATUS_INVALID_ARGUMENT, format!("{name}: {error}")))?;
    Ok(copy_range(raw.ptr, length))
}

fn copy_range(pointer: *const u8, length: usize) -> Vec<u8> {
    if length == 0 {
        Vec::new()
    } else {
        // SAFETY: The raw range was structurally validated and the trusted
        // plugin keeps it readable for this synchronous copy.
        unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec()
    }
}

fn copy_cap_ids(pointer: *const CapId, length: usize) -> Vec<CapId> {
    if length == 0 {
        Vec::new()
    } else {
        // SAFETY: RawMessage::validate_shape checked count, pointer, and
        // alignment; the trusted plugin retains the array synchronously.
        unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec()
    }
}

fn output_size(opcode: u32) -> u32 {
    match opcode {
        HOST_CAP_RETAIN
        | HOST_CAP_RELEASE
        | HOST_CHANNEL_SEND
        | HOST_CHANNEL_FINISH_REQUESTS
        | HOST_CHANNEL_TERMINAL
        | HOST_EFFECT_DEFER
        | HOST_EFFECT_COMMIT
        | HOST_EFFECT_ABORT => size_u32::<BasicOutput>(),
        HOST_CAP_OPEN | HOST_EFFECT_BEGIN => size_u32::<BorrowedCapOutput>(),
        HOST_CHANNEL_RECV => size_u32::<MessageOutput>(),
        HOST_CHANNEL_CANCELLED => size_u32::<BoolOutput>(),
        HOST_PROVIDE => size_u32::<rsi_meta_native::CapOutput>(),
        _ => 0,
    }
}

fn valid_output(output: *mut c_void, capacity: u32, expected: u32) -> bool {
    !output.is_null()
        && capacity >= expected
        && output.addr().is_multiple_of(align_of::<OutputPrefix>())
}

fn write_value<T: Copy>(output: *mut c_void, capacity: u32, value: T) -> u32 {
    if !valid_output(output, capacity, size_u32::<T>()) {
        return rsi_meta_native::STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: The caller provided a writable aligned output range of at least
    // the exact value size; writes never exceed the declared capacity.
    unsafe { output.cast::<T>().write(value) };
    STATUS_OK
}

fn write_prefix(output: *mut c_void, capacity: u32, prefix: OutputPrefix) {
    if valid_output(output, capacity, prefix.struct_size) {
        // SAFETY: Every host output begins with OutputPrefix and the validated
        // range is large enough for that common prefix.
        unsafe { output.cast::<OutputPrefix>().write(prefix) };
    }
}

fn empty_prefix(struct_size: u32) -> OutputPrefix {
    OutputPrefix::empty(struct_size)
}

fn raw_bytes(bytes: &[u8]) -> RawBytes {
    RawBytes {
        ptr: if bytes.is_empty() {
            core::ptr::null()
        } else {
            bytes.as_ptr()
        },
        len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    }
}

fn truncate(bytes: &[u8], maximum: usize) -> Vec<u8> {
    let mut end = bytes.len().min(maximum);
    while end != 0 && std::str::from_utf8(&bytes[..end]).is_err() {
        end -= 1;
    }
    bytes[..end].to_vec()
}

fn next_nonzero(counter: &AtomicU64, resource: &'static str) -> Result<u64, LoaderError> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .map_err(|_| LoaderError::CapacityExhausted {
            resource,
            limit: u64::MAX,
        })
}

fn admission_message(error: AdmissionError) -> &'static str {
    match error {
        AdmissionError::Closed => "host table is finalizing",
        AdmissionError::Saturated => "host table admission saturated",
        AdmissionError::Finalized => "host table is finalized",
    }
}
