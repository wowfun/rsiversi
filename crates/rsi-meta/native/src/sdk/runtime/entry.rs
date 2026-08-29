use super::operations::Dispatch;
use super::{PluginRuntime, operations, panics};
use crate::sdk::host::HostPort;
use crate::{
    HostTable, NativePlugin, PluginTable, STATUS_INVALID_ARGUMENT, STATUS_LIMIT_EXCEEDED,
    STATUS_OK, STATUS_PANICKED, STATUS_PROTOCOL_ERROR, TableHeader,
};
use core::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ISSUER: AtomicU64 = AtomicU64::new(1);

/// Implements the one exported ABI v3 entry for a safe Rust plugin.
///
/// # Safety
///
/// `host` must identify a readable `HostTable`. `plugin_out` must identify
/// writable, aligned storage of `output_capacity` bytes. Both are borrowed only
/// for this call; a compatible host table then remains valid until finalization.
pub unsafe extern "C" fn plugin_entry<P: NativePlugin>(
    host: *const HostTable,
    plugin_out: *mut PluginTable,
    output_capacity: u32,
) -> u32 {
    if host.is_null()
        || plugin_out.is_null()
        || output_capacity < PluginTable::STRUCT_SIZE
        || !host.addr().is_multiple_of(align_of::<HostTable>())
        || !plugin_out.addr().is_multiple_of(align_of::<PluginTable>())
    {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: The checked output points to writable, aligned full table storage.
    unsafe { plugin_out.write(PluginTable::EMPTY) };
    // SAFETY: The checked input points to a readable aligned HostTable.
    let Ok(host) = HostPort::new(unsafe { host.read() }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    let issuer = match next_issuer::<P>(host.issuer()) {
        Ok(value) => value,
        Err(status) => return status,
    };
    let plugin = match panics::catch(P::default) {
        Ok(plugin) => plugin,
        Err(payload) => {
            drop(payload);
            return STATUS_PANICKED;
        }
    };
    let (runtime, factory) = match PluginRuntime::new(host, plugin, issuer) {
        Ok(value) => value,
        Err(status) => return status,
    };
    let state = Box::into_raw(Box::new(runtime));
    let table = PluginTable {
        header: TableHeader::new(crate::ABI_MINOR, PluginTable::STRUCT_SIZE),
        issuer: factory.issuer,
        state: state.cast(),
        exchange: Some(exchange::<P>),
        factory,
    };
    // SAFETY: The output remains exclusively borrowed and the table is complete.
    unsafe { plugin_out.write(table) };
    STATUS_OK
}

/// # Safety
///
/// `state` must be the live runtime allocated by `plugin_entry`; input and
/// output ranges must satisfy the selected opcode's header contract and remain
/// valid for this synchronous exchange. Calls must honor table admission.
unsafe extern "C" fn exchange<P: NativePlugin>(
    state: *mut c_void,
    opcode: u32,
    input: *const c_void,
    input_size: u32,
    output: *mut c_void,
    output_capacity: u32,
) -> u32 {
    if state.is_null() || !state.addr().is_multiple_of(align_of::<PluginRuntime<P>>()) {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: Host admission guards the raw state before this first access.
    let runtime = unsafe { &*state.cast::<PluginRuntime<P>>() };
    let Some(admission) = runtime.exchanges.enter() else {
        return STATUS_PROTOCOL_ERROR;
    };
    let dispatched = panics::catch(|| {
        operations::dispatch(runtime, opcode, input, input_size, output, output_capacity)
    });
    let Dispatch { status, finalize } = match dispatched {
        Ok(value) => value,
        Err(panic) => Dispatch {
            status: recover_panic(runtime, opcode, output, output_capacity, &panic),
            finalize: false,
        },
    };
    if finalize && status == STATUS_OK {
        admission.finish_final();
        // SAFETY: FINALIZE closed admission as the sole exchange and returned
        // all ownership. No guard or reference touches state after this drop.
        drop(unsafe { Box::from_raw(state.cast::<PluginRuntime<P>>()) });
    } else {
        drop(admission);
    }
    status
}

fn recover_panic<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    opcode: u32,
    output: *mut c_void,
    output_capacity: u32,
    panic: &panics::PanicPayload,
) -> u32 {
    let recovered = panics::catch(|| {
        operations::panic_output(runtime, opcode, output, output_capacity, panic.diagnostic())
    });
    match recovered {
        Ok(status) => status,
        Err(payload) => {
            drop(payload);
            STATUS_PANICKED
        }
    }
}

pub(super) fn next_issuer<P: NativePlugin>(host_issuer: u64) -> Result<u64, u32> {
    let module_address = exchange::<P> as *const () as usize as u64;
    loop {
        let sequence = next_sequence(&NEXT_ISSUER).ok_or(STATUS_LIMIT_EXCEEDED)?;
        let value = mix64(sequence ^ module_address.rotate_left(17) ^ host_issuer.rotate_left(31));
        if value != 0 && value != host_issuer {
            return Ok(value);
        }
    }
}

fn next_sequence(sequence: &AtomicU64) -> Option<u64> {
    sequence
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .ok()
}

const fn mix64(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issuer_sequence_exhaustion_is_stable_and_never_wraps() {
        let sequence = AtomicU64::new(u64::MAX - 1);
        assert_eq!(next_sequence(&sequence), Some(u64::MAX - 1));
        assert_eq!(sequence.load(Ordering::Relaxed), u64::MAX);
        assert_eq!(next_sequence(&sequence), None);
        assert_eq!(next_sequence(&sequence), None);
        assert_eq!(sequence.load(Ordering::Relaxed), u64::MAX);
    }
}
