use super::super::super::PluginRuntime;
use super::MAX_INJECTIONS;
use crate::sdk::host::Injection;
use crate::{
    ActivateInput, CAP_KIND_SERVICE, NativePlugin, RIGHT_OPEN, RIGHT_RETAIN,
    STATUS_INVALID_ARGUMENT,
};

pub(super) fn import_injections<P: NativePlugin>(
    runtime: &PluginRuntime<P>,
    requirements: &[crate::ServiceRequirement],
    input: ActivateInput,
) -> Result<Vec<Injection>, u32> {
    let count = usize::try_from(input.injection_count).map_err(|_| STATUS_INVALID_ARGUMENT)?;
    if count != requirements.len()
        || count > MAX_INJECTIONS
        || (count != 0 && input.injections.is_null())
        || (count != 0
            && !input
                .injections
                .addr()
                .is_multiple_of(align_of::<crate::Injection>()))
    {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let bytes = count
        .checked_mul(size_of::<crate::Injection>())
        .ok_or(STATUS_INVALID_ARGUMENT)?;
    if bytes > isize::MAX as usize {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let raw = if count == 0 {
        &[][..]
    } else {
        // SAFETY: Count, multiplication, non-null, and alignment were
        // validated. The zero-count ABI representation is handled above
        // because its canonical pointer is null.
        unsafe { std::slice::from_raw_parts(input.injections, count) }
    };
    let port = runtime.host();
    let mut ordered = vec![None; count];
    for injection in raw {
        let index =
            usize::try_from(injection.requirement_index).map_err(|_| STATUS_INVALID_ARGUMENT)?;
        if index >= count || ordered[index].replace(injection.service).is_some() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        port.validate_cap(
            injection.service,
            CAP_KIND_SERVICE,
            RIGHT_RETAIN | RIGHT_OPEN,
        )
        .map_err(|error| error.status())?;
    }
    let mut imported = Vec::with_capacity(count);
    for (requirement, capability) in requirements.iter().cloned().zip(ordered) {
        let capability = capability.ok_or(STATUS_INVALID_ARGUMENT)?;
        if let Err(error) = port.retain(capability) {
            drop(imported);
            return Err(error.status());
        }
        imported.push(Injection {
            requirement,
            capability: crate::Capability::new(port, capability),
        });
    }
    Ok(imported)
}
