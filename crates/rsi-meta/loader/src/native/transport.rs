use super::admission::{Admission, AdmissionError, AdmissionGate};
use crate::{LoaderError, MAX_NATIVE_DIAGNOSTIC_BYTES};
use core::ffi::c_void;
use rsi_meta_plugin::{
    BasicOutput, CapId, CapInput, EmptyInput, FrameHeader, OutputPrefix, PLUGIN_CAP_RELEASE,
    PLUGIN_CAP_RETAIN, PLUGIN_DESTROY_FACTORY, PLUGIN_DESTROY_INSTANCE, PLUGIN_FINALIZE,
    PLUGIN_RELEASE_OUTPUT, PluginTable, ReleaseId, ReleaseOutputInput, STATUS_OK,
};
use std::mem::size_of;
use std::sync::Arc;

pub(super) trait OutputFrame: Copy {
    fn prefix(&self) -> OutputPrefix;
}

macro_rules! output_frame {
    ($($type:ty),+ $(,)?) => {
        $(impl OutputFrame for $type {
            fn prefix(&self) -> OutputPrefix { self.prefix }
        })+
    };
}

output_frame!(
    rsi_meta_plugin::BasicOutput,
    rsi_meta_plugin::BytesOutput,
    rsi_meta_plugin::CapOutput,
    rsi_meta_plugin::BorrowedCapOutput,
    rsi_meta_plugin::MessageOutput,
    rsi_meta_plugin::BoolOutput,
    rsi_meta_plugin::PrepareOutput,
);

/// The only implementation allowed to dereference a plugin table.
pub(super) struct PluginTransport {
    table: PluginTable,
    admission: Arc<AdmissionGate>,
}

// SAFETY: The ABI requires a table to be callable from arbitrary host threads.
// Every raw access first owns `admission`; plugin-side admission and its own
// operation gates protect the opaque state.
unsafe impl Send for PluginTransport {}
// SAFETY: Same table contract and admission proof as the Send implementation.
unsafe impl Sync for PluginTransport {}

impl PluginTransport {
    pub(super) fn new(table: PluginTable) -> Self {
        debug_assert!(table.is_compatible_for_host(rsi_meta_plugin::ABI_MINOR));
        Self {
            table,
            admission: Arc::new(AdmissionGate::new()),
        }
    }

    pub(super) const fn issuer(&self) -> u64 {
        self.table.issuer
    }

    pub(super) const fn factory(&self) -> CapId {
        self.table.factory
    }

    pub(super) fn is_finalized(&self) -> bool {
        self.admission.is_finalized()
    }

    pub(super) fn call<I: Copy, O: OutputFrame>(
        self: &Arc<Self>,
        opcode: u32,
        input: &I,
        operation: &'static str,
    ) -> Result<PluginReply<O>, LoaderError> {
        let admission = self
            .admission
            .try_enter()
            .map_err(|error| LoaderError::Callback {
                operation,
                message: admission_message(error).to_owned(),
            })?;
        // SAFETY: `O` is a C output record whose all-zero representation is
        // specified by the ABI. The complete aligned local record is writable.
        let mut output: O = unsafe { core::mem::zeroed() };
        let status = self.exchange(
            opcode,
            std::ptr::from_ref(input).cast(),
            size_u32::<I>(),
            (&raw mut output).cast(),
            size_u32::<O>(),
        );
        PluginReply::adopt(Arc::clone(self), admission, operation, status, output)
    }

    pub(super) fn retain(self: &Arc<Self>, capability: CapId) -> Result<(), LoaderError> {
        self.basic_cap(PLUGIN_CAP_RETAIN, capability, "capability retain")
    }

    pub(super) fn release(self: &Arc<Self>, capability: CapId) -> Result<(), LoaderError> {
        self.basic_cap(PLUGIN_CAP_RELEASE, capability, "capability release")
    }

    pub(super) fn destroy_instance(self: &Arc<Self>, capability: CapId) -> Result<(), LoaderError> {
        self.basic_cap(PLUGIN_DESTROY_INSTANCE, capability, "instance destruction")
    }

    pub(super) fn run_cleanup(self: &Arc<Self>, capability: CapId) -> Result<(), LoaderError> {
        self.basic_cap(
            rsi_meta_plugin::PLUGIN_RUN_CLEANUP,
            capability,
            "cleanup run",
        )
    }

    pub(super) fn destroy_factory(self: &Arc<Self>) -> Result<(), LoaderError> {
        self.basic_cap(
            PLUGIN_DESTROY_FACTORY,
            self.table.factory,
            "factory destruction",
        )
    }

    fn basic_cap(
        self: &Arc<Self>,
        opcode: u32,
        capability: CapId,
        operation: &'static str,
    ) -> Result<(), LoaderError> {
        let input = CapInput {
            header: frame::<CapInput>(),
            capability,
        };
        let reply = self.call::<_, BasicOutput>(opcode, &input, operation)?;
        reply.into_result().map(|_| ())
    }

    /// Runs FINALIZE with sole transport ownership. A failed attempt releases
    /// its diagnostic while ordinary admission remains closed, then reopens.
    pub(super) fn finalize(self: &Arc<Self>) -> Result<(), LoaderError> {
        let exclusive =
            self.admission
                .begin_exclusive()
                .map_err(|error| LoaderError::Callback {
                    operation: "finalize",
                    message: admission_message(error).to_owned(),
                })?;
        let input = EmptyInput {
            header: frame::<EmptyInput>(),
        };
        // SAFETY: BasicOutput has an ABI-defined all-zero empty representation.
        let mut output: BasicOutput = unsafe { core::mem::zeroed() };
        let status = self.exchange(
            PLUGIN_FINALIZE,
            (&raw const input).cast(),
            size_u32::<EmptyInput>(),
            (&raw mut output).cast(),
            size_u32::<BasicOutput>(),
        );
        let prefix = output.prefix;
        if status == STATUS_OK {
            // FINALIZE success invalidates plugin state at the return edge. No
            // parse failure may reopen admission or attempt a second exchange.
            exclusive.finish();
            validate_prefix(prefix, size_u32::<BasicOutput>(), "finalize")?;
            if !prefix.release.is_empty() {
                return Err(LoaderError::Protocol {
                    operation: "finalize",
                    message: "successful FINALIZE published a release token".to_owned(),
                });
            }
            if prefix.diagnostic.len != 0 {
                return Err(LoaderError::Protocol {
                    operation: "finalize",
                    message: "successful FINALIZE published diagnostic pointer authority"
                        .to_owned(),
                });
            }
            return Ok(());
        }

        // A failed FINALIZE leaves the table live. Adopt its release token
        // before interpreting any other output field; the guard releases it
        // while the exclusive lane is still held, including on parse errors.
        let release = valid_release_for(prefix.release, self.table.issuer)?;
        let mut release = ExclusiveOutputRelease {
            transport: self,
            release,
        };
        let parsed = validate_prefix(prefix, size_u32::<BasicOutput>(), "finalize")
            .and_then(|()| copy_bytes(prefix.diagnostic, MAX_NATIVE_DIAGNOSTIC_BYTES, "finalize"));
        let release_result = release.release();
        let diagnostic = String::from_utf8_lossy(&parsed?).into_owned();
        release_result?;
        status_result("finalize", status, &diagnostic)
    }

    fn exchange(
        &self,
        opcode: u32,
        input: *const c_void,
        input_size: u32,
        output: *mut c_void,
        output_capacity: u32,
    ) -> u32 {
        // SAFETY: Construction validated this function pointer and state. Every
        // caller owns ordinary or exclusive admission for the complete call.
        unsafe {
            self.table.exchange.expect("validated plugin exchange")(
                self.table.state,
                opcode,
                input,
                input_size,
                output,
                output_capacity,
            )
        }
    }
}

struct ExclusiveOutputRelease<'transport> {
    transport: &'transport PluginTransport,
    release: Option<ReleaseId>,
}

impl ExclusiveOutputRelease<'_> {
    fn release(&mut self) -> Result<(), LoaderError> {
        let Some(release) = self.release.take() else {
            return Ok(());
        };
        let input = ReleaseOutputInput {
            header: frame::<ReleaseOutputInput>(),
            release,
        };
        let status = self.transport.exchange(
            PLUGIN_RELEASE_OUTPUT,
            (&raw const input).cast(),
            size_u32::<ReleaseOutputInput>(),
            core::ptr::null_mut(),
            0,
        );
        status_result("finalize output release", status, "")
    }
}

impl Drop for ExclusiveOutputRelease<'_> {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

pub(super) struct PluginReply<O: OutputFrame> {
    transport: Arc<PluginTransport>,
    admission: Option<Admission>,
    operation: &'static str,
    status: u32,
    diagnostic: String,
    release: Option<ReleaseId>,
    value: O,
}

impl<O: OutputFrame> PluginReply<O> {
    fn adopt(
        transport: Arc<PluginTransport>,
        admission: Admission,
        operation: &'static str,
        status: u32,
        value: O,
    ) -> Result<Self, LoaderError> {
        let prefix = value.prefix();
        let release = valid_release_for(prefix.release, transport.issuer())?;
        let mut reply = Self {
            transport,
            admission: Some(admission),
            operation,
            status,
            diagnostic: String::new(),
            release,
            value,
        };
        validate_prefix(prefix, size_u32::<O>(), operation)?;
        if prefix.diagnostic.len != 0 && release.is_none() {
            return Err(LoaderError::Protocol {
                operation,
                message: "nonempty diagnostic has no owning release token".to_owned(),
            });
        }
        reply.diagnostic = String::from_utf8_lossy(&copy_bytes(
            prefix.diagnostic,
            MAX_NATIVE_DIAGNOSTIC_BYTES,
            operation,
        )?)
        .into_owned();
        Ok(reply)
    }

    pub(super) fn status(&self) -> u32 {
        self.status
    }

    pub(super) fn owns_payload(&self) -> bool {
        self.release.is_some()
    }

    pub(super) fn value(&self) -> &O {
        &self.value
    }

    pub(super) fn release(mut self) -> Result<(), LoaderError> {
        self.release_inner()
    }

    pub(super) fn into_result(mut self) -> Result<O, LoaderError> {
        let result = status_result(self.operation, self.status, &self.diagnostic);
        self.release_inner()?;
        result.map(|()| self.value)
    }

    fn release_inner(&mut self) -> Result<(), LoaderError> {
        let Some(release) = self.release.take() else {
            self.admission.take();
            return Ok(());
        };
        let input = ReleaseOutputInput {
            header: frame::<ReleaseOutputInput>(),
            release,
        };
        let status = self.transport.exchange(
            PLUGIN_RELEASE_OUTPUT,
            (&raw const input).cast(),
            size_u32::<ReleaseOutputInput>(),
            core::ptr::null_mut(),
            0,
        );
        self.admission.take();
        status_result("plugin output release", status, "")
    }
}

impl<O: OutputFrame> Drop for PluginReply<O> {
    fn drop(&mut self) {
        let _ = self.release_inner();
    }
}

fn valid_release_for(release: ReleaseId, issuer: u64) -> Result<Option<ReleaseId>, LoaderError> {
    if release.is_empty() {
        Ok(None)
    } else if !release.is_valid_or_empty() || release.issuer != issuer {
        Err(LoaderError::Protocol {
            operation: "output adoption",
            message: "plugin published a malformed or foreign release token".to_owned(),
        })
    } else {
        Ok(Some(release))
    }
}

fn validate_prefix(
    prefix: OutputPrefix,
    expected: u32,
    operation: &'static str,
) -> Result<(), LoaderError> {
    prefix
        .validate(expected, expected, MAX_NATIVE_DIAGNOSTIC_BYTES)
        .map_err(|error| LoaderError::Protocol {
            operation,
            message: error.to_string(),
        })
}

pub(super) fn copy_bytes(
    bytes: rsi_meta_plugin::RawBytes,
    maximum: usize,
    operation: &'static str,
) -> Result<Vec<u8>, LoaderError> {
    let length = bytes
        .checked_len(maximum)
        .map_err(|error| LoaderError::Protocol {
            operation,
            message: error.to_string(),
        })?;
    if length == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: The trusted plugin keeps the structurally validated output range
    // readable until its adopted release token is returned.
    Ok(unsafe { std::slice::from_raw_parts(bytes.ptr, length) }.to_vec())
}

pub(super) fn status_result(
    operation: &'static str,
    status: u32,
    diagnostic: &str,
) -> Result<(), LoaderError> {
    if status == STATUS_OK {
        return Ok(());
    }
    if status == rsi_meta_plugin::STATUS_BUSY {
        return Err(LoaderError::Busy { operation });
    }
    if status == rsi_meta_plugin::STATUS_REENTRANT {
        return Err(LoaderError::Reentrant { operation });
    }
    let name = match status {
        rsi_meta_plugin::STATUS_INVALID_ARGUMENT => "invalid argument",
        rsi_meta_plugin::STATUS_FAILED => "failed",
        rsi_meta_plugin::STATUS_PANICKED => "panicked",
        rsi_meta_plugin::STATUS_PROTOCOL_ERROR => "protocol error",
        rsi_meta_plugin::STATUS_UNSUPPORTED => "unsupported",
        rsi_meta_plugin::STATUS_STALE_CAPABILITY => "stale capability",
        rsi_meta_plugin::STATUS_WRONG_CAPABILITY => "wrong capability",
        rsi_meta_plugin::STATUS_LIMIT_EXCEEDED => "limit exceeded",
        rsi_meta_plugin::STATUS_CANCELLED => "cancelled",
        rsi_meta_plugin::STATUS_TERMINAL => "terminal",
        rsi_meta_plugin::STATUS_BUFFER_TOO_SMALL => "buffer too small",
        _ => "unknown status",
    };
    Err(LoaderError::Callback {
        operation,
        message: if diagnostic.is_empty() {
            format!("{name} (status {status})")
        } else {
            format!("{name}: {diagnostic}")
        },
    })
}

pub(super) const fn frame<T>() -> FrameHeader {
    FrameHeader::new(size_u32::<T>())
}

#[allow(clippy::cast_possible_truncation)]
pub(super) const fn size_u32<T>() -> u32 {
    size_of::<T>() as u32
}

fn admission_message(error: AdmissionError) -> &'static str {
    match error {
        AdmissionError::Closed => "plugin transport is finalizing",
        AdmissionError::Saturated => "plugin transport admission saturated",
        AdmissionError::Finalized => "plugin transport is finalized",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_meta_plugin::{
        ABI_MINOR, CAP_KIND_FACTORY, RIGHT_MUTATE, RIGHT_RETAIN, STATUS_BUSY, STATUS_FAILED,
        STATUS_REENTRANT, TableHeader,
    };
    use std::sync::Mutex;

    const ISSUER: u64 = 41;
    const RELEASE: ReleaseId = ReleaseId {
        issuer: ISSUER,
        slot: 7,
        epoch: 3,
    };

    #[derive(Clone, Copy)]
    enum FinalizeOutput {
        Empty,
        OwnedRelease,
        InvalidHeader,
        UnownedDiagnostic,
        InvalidHeaderWithRelease,
        InvalidDiagnosticWithRelease,
    }

    struct MockState {
        response: Mutex<(u32, FinalizeOutput)>,
        calls: Mutex<Vec<u32>>,
    }

    impl FinalizeOutput {
        fn prefix(self) -> OutputPrefix {
            let mut prefix = OutputPrefix::empty(size_u32::<BasicOutput>());
            match self {
                Self::Empty => {}
                Self::OwnedRelease => prefix.release = RELEASE,
                Self::InvalidHeader => prefix.struct_size = 0,
                Self::UnownedDiagnostic => {
                    prefix.diagnostic = rsi_meta_plugin::RawBytes {
                        ptr: b"diagnostic".as_ptr(),
                        len: 10,
                    };
                }
                Self::InvalidHeaderWithRelease => {
                    prefix.release = RELEASE;
                    prefix.struct_size = 0;
                }
                Self::InvalidDiagnosticWithRelease => {
                    prefix.release = RELEASE;
                    prefix.diagnostic = rsi_meta_plugin::RawBytes {
                        ptr: core::ptr::null(),
                        len: 1,
                    };
                }
            }
            prefix
        }
    }

    unsafe extern "C" fn mock_exchange(
        state: *mut c_void,
        opcode: u32,
        _input: *const c_void,
        _input_size: u32,
        output: *mut c_void,
        output_capacity: u32,
    ) -> u32 {
        // SAFETY: Each test keeps its boxed MockState alive until after the
        // transport is dropped, and the mock table points at that allocation.
        let state = unsafe { &*state.cast::<MockState>() };
        state.calls.lock().expect("mock calls lock").push(opcode);
        match opcode {
            PLUGIN_FINALIZE => {
                if output_capacity != size_u32::<BasicOutput>() || output.is_null() {
                    return rsi_meta_plugin::STATUS_PROTOCOL_ERROR;
                }
                let (status, response) = *state.response.lock().expect("mock response lock");
                // SAFETY: PluginTransport supplied an aligned, writable
                // BasicOutput record with the checked exact capacity.
                unsafe {
                    output.cast::<BasicOutput>().write(BasicOutput {
                        prefix: response.prefix(),
                    });
                }
                status
            }
            PLUGIN_RELEASE_OUTPUT => STATUS_OK,
            _ => rsi_meta_plugin::STATUS_PROTOCOL_ERROR,
        }
    }

    struct MockPlugin {
        transport: Arc<PluginTransport>,
        state: Box<MockState>,
    }

    impl MockPlugin {
        fn new(status: u32, output: FinalizeOutput) -> Self {
            let mut state = Box::new(MockState {
                response: Mutex::new((status, output)),
                calls: Mutex::new(Vec::new()),
            });
            let table = PluginTable {
                header: TableHeader::new(ABI_MINOR, PluginTable::STRUCT_SIZE),
                issuer: ISSUER,
                state: (&raw mut *state).cast(),
                exchange: Some(mock_exchange),
                factory: CapId {
                    issuer: ISSUER,
                    slot: 1,
                    epoch: 1,
                    kind: CAP_KIND_FACTORY,
                    rights: RIGHT_RETAIN | RIGHT_MUTATE,
                },
            };
            Self {
                transport: Arc::new(PluginTransport::new(table)),
                state,
            }
        }

        fn calls(&self) -> Vec<u32> {
            self.state.calls.lock().expect("mock calls lock").clone()
        }

        fn set_response(&self, status: u32, output: FinalizeOutput) {
            *self.state.response.lock().expect("mock response lock") = (status, output);
        }
    }

    #[test]
    fn successful_finalize_with_release_closes_without_releasing_output() {
        let plugin = MockPlugin::new(STATUS_OK, FinalizeOutput::OwnedRelease);

        assert!(matches!(
            plugin.transport.finalize(),
            Err(LoaderError::Protocol {
                operation: "finalize",
                ..
            })
        ));
        assert!(plugin.transport.is_finalized());
        assert_eq!(plugin.calls(), vec![PLUGIN_FINALIZE]);

        assert!(matches!(
            plugin.transport.finalize(),
            Err(LoaderError::Callback {
                operation: "finalize",
                ..
            })
        ));
        assert_eq!(plugin.calls(), vec![PLUGIN_FINALIZE]);
    }

    #[test]
    fn malformed_successful_finalize_outputs_close_without_a_second_exchange() {
        for output in [
            FinalizeOutput::InvalidHeader,
            FinalizeOutput::UnownedDiagnostic,
        ] {
            let plugin = MockPlugin::new(STATUS_OK, output);

            assert!(matches!(
                plugin.transport.finalize(),
                Err(LoaderError::Protocol {
                    operation: "finalize",
                    ..
                })
            ));
            assert!(plugin.transport.is_finalized());
            assert_eq!(plugin.calls(), vec![PLUGIN_FINALIZE]);
        }
    }

    #[test]
    fn failed_finalize_releases_adopted_output_once_even_when_parsing_fails() {
        for output in [
            FinalizeOutput::InvalidHeaderWithRelease,
            FinalizeOutput::InvalidDiagnosticWithRelease,
        ] {
            let plugin = MockPlugin::new(STATUS_FAILED, output);

            assert!(matches!(
                plugin.transport.finalize(),
                Err(LoaderError::Protocol {
                    operation: "finalize",
                    ..
                })
            ));
            assert!(!plugin.transport.is_finalized());
            assert_eq!(plugin.calls(), vec![PLUGIN_FINALIZE, PLUGIN_RELEASE_OUTPUT]);
        }
    }

    #[test]
    fn failed_finalize_releases_output_reopens_and_can_retry_successfully() {
        let plugin = MockPlugin::new(STATUS_FAILED, FinalizeOutput::OwnedRelease);

        assert!(plugin.transport.finalize().is_err());
        assert!(!plugin.transport.is_finalized());
        assert_eq!(plugin.calls(), vec![PLUGIN_FINALIZE, PLUGIN_RELEASE_OUTPUT]);

        plugin.set_response(STATUS_OK, FinalizeOutput::Empty);
        assert!(plugin.transport.finalize().is_ok());
        assert!(plugin.transport.is_finalized());
        assert_eq!(
            plugin.calls(),
            vec![PLUGIN_FINALIZE, PLUGIN_RELEASE_OUTPUT, PLUGIN_FINALIZE]
        );
    }

    #[test]
    fn busy_and_reentrant_statuses_remain_distinct() {
        assert!(matches!(
            status_result("operation", STATUS_BUSY, "ignored"),
            Err(LoaderError::Busy {
                operation: "operation"
            })
        ));
        assert!(matches!(
            status_result("operation", STATUS_REENTRANT, "ignored"),
            Err(LoaderError::Reentrant {
                operation: "operation"
            })
        ));
    }
}
