use super::SdkError;
use crate::{
    BasicOutput, BorrowedCapOutput, CapId, CapInput, CapOutput, HOST_CAP_RELEASE, HOST_CAP_RETAIN,
    HOST_RELEASE_OUTPUT, HostTable, RIGHT_RETAIN, ReleaseId, ReleaseOutputInput,
    STATUS_INVALID_ARGUMENT, STATUS_OK,
};

mod channel;
mod output;

pub(super) use output::{CALL_CHANNEL_RIGHTS, OutputGuard, frame, size_u32, validate_cap};

#[derive(Clone, Copy)]
pub(crate) struct HostPort {
    table: HostTable,
}

impl HostPort {
    pub(crate) fn new(table: HostTable) -> Result<Self, u32> {
        if table.is_compatible() {
            Ok(Self { table })
        } else {
            Err(STATUS_INVALID_ARGUMENT)
        }
    }

    pub(crate) const fn issuer(&self) -> u64 {
        self.table.issuer
    }

    pub(crate) fn call_basic<I>(&self, opcode: u32, input: &I) -> Result<(), SdkError> {
        // SAFETY: `BasicOutput` contains only integers and raw pointers, for
        // which the all-zero bit pattern is valid. A zero header also ensures
        // that a host which fails to initialize the output is rejected.
        let mut output: BasicOutput = unsafe { core::mem::zeroed() };
        let status = self.call(opcode, input, &mut output);
        let (guard, diagnostic) =
            OutputGuard::adopt(*self, output.prefix, size_u32::<BasicOutput>())?;
        let release = guard.release();
        if status != STATUS_OK {
            return Err(SdkError::new(status, diagnostic));
        }
        release
    }

    pub(crate) fn retain(&self, capability: CapId) -> Result<(), SdkError> {
        self.call_basic(
            HOST_CAP_RETAIN,
            &CapInput {
                header: frame::<CapInput>(),
                capability,
            },
        )
    }

    pub(crate) fn release(&self, capability: CapId) -> Result<(), SdkError> {
        self.call_basic(
            HOST_CAP_RELEASE,
            &CapInput {
                header: frame::<CapInput>(),
                capability,
            },
        )
    }

    pub(crate) fn owned_cap<I>(
        &self,
        opcode: u32,
        input: &I,
        kind: u32,
        rights: u32,
    ) -> Result<CapId, SdkError> {
        // SAFETY: `CapOutput` contains only integers and raw pointers, for
        // which the all-zero bit pattern is valid. Validation rejects the
        // zero sentinel unless the host writes a complete output.
        let mut output: CapOutput = unsafe { core::mem::zeroed() };
        let status = self.call(opcode, input, &mut output);
        let (guard, diagnostic) =
            OutputGuard::adopt(*self, output.prefix, size_u32::<CapOutput>())?;
        if status != STATUS_OK {
            let _ = guard.release();
            return Err(SdkError::new(status, diagnostic));
        }
        validate_cap(
            output.capability,
            self.issuer(),
            kind,
            rights | RIGHT_RETAIN,
        )?;
        if let Err(error) = self.retain(output.capability) {
            let _ = guard.release();
            return Err(error);
        }
        if let Err(error) = guard.release() {
            let _ = self.release(output.capability);
            return Err(error);
        }
        Ok(output.capability)
    }

    pub(crate) fn borrowed_cap<I>(
        &self,
        opcode: u32,
        input: &I,
        kind: u32,
        rights: u32,
    ) -> Result<CapId, SdkError> {
        // SAFETY: `BorrowedCapOutput` contains only integers and raw pointers,
        // for which the all-zero bit pattern is valid. Validation rejects the
        // zero sentinel unless the host writes a complete output.
        let mut output: BorrowedCapOutput = unsafe { core::mem::zeroed() };
        let status = self.call(opcode, input, &mut output);
        let (guard, diagnostic) =
            OutputGuard::adopt(*self, output.prefix, size_u32::<BorrowedCapOutput>())?;
        let result = if status == STATUS_OK {
            validate_cap(output.capability, self.issuer(), kind, rights)
        } else {
            Err(SdkError::new(status, diagnostic))
        };
        let release = guard.release();
        result.and(release).map(|()| output.capability)
    }

    pub(crate) fn validate_cap(
        &self,
        capability: CapId,
        kind: u32,
        rights: u32,
    ) -> Result<(), SdkError> {
        if capability.is_structurally_valid()
            && capability.issuer == self.issuer()
            && capability.kind == kind
            && capability.rights == rights
        {
            Ok(())
        } else {
            Err(SdkError::new(
                crate::STATUS_WRONG_CAPABILITY,
                "capability metadata does not match its host authority",
            ))
        }
    }

    fn call<I, O>(&self, opcode: u32, input: &I, output: &mut O) -> u32 {
        // SAFETY: Entry validated this function and state. Typed frames remain
        // live and exclusively borrowed for the synchronous opposite-port call.
        unsafe {
            self.table.exchange.expect("validated host exchange")(
                self.table.state,
                opcode,
                core::ptr::from_ref(input).cast(),
                size_u32::<I>(),
                core::ptr::from_mut(output).cast(),
                size_u32::<O>(),
            )
        }
    }

    pub(super) fn release_output(&self, release: ReleaseId) -> Result<(), SdkError> {
        if release.is_empty() {
            return Ok(());
        }
        let input = ReleaseOutputInput {
            header: frame::<ReleaseOutputInput>(),
            release,
        };
        // SAFETY: RELEASE_OUTPUT is status-only and borrows this exact frame.
        let status = unsafe {
            self.table.exchange.expect("validated host exchange")(
                self.table.state,
                HOST_RELEASE_OUTPUT,
                (&raw const input).cast(),
                size_u32::<ReleaseOutputInput>(),
                core::ptr::null_mut(),
                0,
            )
        };
        if status == STATUS_OK {
            Ok(())
        } else {
            Err(SdkError::new(status, "host rejected output release"))
        }
    }
}

// SAFETY: Entry requires the host table's state and exchange function to remain
// valid and thread-safe until successful PLUGIN_FINALIZE. The native host is the
// audited owner of that contract.
unsafe impl Send for HostPort {}
// SAFETY: The same entry contract permits concurrent opposite-port calls; the
// host implementation owns its synchronization and never borrows Rust state.
unsafe impl Sync for HostPort {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        MAX_DIAGNOSTIC_BYTES, OutputPrefix, RawBytes, STATUS_FAILED, STATUS_PROTOCOL_ERROR,
    };
    use core::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct DiagnosticHost {
        diagnostic: Vec<u8>,
        releases: AtomicUsize,
    }

    unsafe extern "C" fn diagnostic_exchange(
        state: *mut c_void,
        opcode: u32,
        _input: *const c_void,
        _input_size: u32,
        output: *mut c_void,
        output_capacity: u32,
    ) -> u32 {
        // SAFETY: The test keeps its boxed state live through every exchange.
        let state = unsafe { &*state.cast::<DiagnosticHost>() };
        if opcode == HOST_RELEASE_OUTPUT {
            state.releases.fetch_add(1, Ordering::AcqRel);
            return STATUS_OK;
        }
        if output.is_null()
            || output_capacity < size_u32::<BasicOutput>()
            || !output.addr().is_multiple_of(align_of::<BasicOutput>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        let value = BasicOutput {
            prefix: OutputPrefix {
                struct_size: size_u32::<BasicOutput>(),
                reserved: 0,
                release: ReleaseId {
                    issuer: 7,
                    slot: 1,
                    epoch: 1,
                },
                diagnostic: RawBytes {
                    ptr: state.diagnostic.as_ptr(),
                    len: u64::try_from(state.diagnostic.len())
                        .expect("test diagnostic length fits u64"),
                },
            },
        };
        // SAFETY: The checked output is aligned and large enough for BasicOutput.
        unsafe { output.cast::<BasicOutput>().write(value) };
        STATUS_FAILED
    }

    #[test]
    fn host_diagnostic_limit_preserves_status_and_rejects_oversize() {
        let mut state = Box::new(DiagnosticHost {
            diagnostic: vec![b'x'; MAX_DIAGNOSTIC_BYTES],
            releases: AtomicUsize::new(0),
        });
        let port = HostPort::new(HostTable {
            header: crate::TableHeader::new(crate::ABI_MINOR, HostTable::STRUCT_SIZE),
            issuer: 7,
            state: (&raw mut *state).cast(),
            exchange: Some(diagnostic_exchange),
        })
        .unwrap();

        let error = port.call_basic(HOST_CAP_RETAIN, &0_u32).unwrap_err();
        assert_eq!(error.status(), STATUS_FAILED);
        assert_eq!(error.diagnostic().len(), MAX_DIAGNOSTIC_BYTES);
        assert_eq!(state.releases.load(Ordering::Acquire), 1);

        let mut oversized = Box::new(DiagnosticHost {
            diagnostic: vec![b'x'; MAX_DIAGNOSTIC_BYTES + 1],
            releases: AtomicUsize::new(0),
        });
        let port = HostPort::new(HostTable {
            header: crate::TableHeader::new(crate::ABI_MINOR, HostTable::STRUCT_SIZE),
            issuer: 7,
            state: (&raw mut *oversized).cast(),
            exchange: Some(diagnostic_exchange),
        })
        .unwrap();
        assert_eq!(
            port.call_basic(HOST_CAP_RETAIN, &0_u32)
                .unwrap_err()
                .status(),
            STATUS_PROTOCOL_ERROR
        );
        assert_eq!(oversized.releases.load(Ordering::Acquire), 1);
    }
}
