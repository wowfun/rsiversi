use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};

const MAX_PANIC_DIAGNOSTIC: usize = 512;

pub(super) struct PanicPayload {
    diagnostic: String,
}

impl PanicPayload {
    pub(super) fn contain(payload: Box<dyn Any + Send>) -> Self {
        let mut diagnostic = builtin_diagnostic(payload.as_ref());
        let dropped = catch_unwind(AssertUnwindSafe(|| drop(payload)));
        if let Err(secondary) = dropped {
            std::mem::forget(secondary);
            diagnostic.push_str("; panic payload destructor also panicked");
        }
        Self { diagnostic }
    }

    pub(super) fn diagnostic(&self) -> &str {
        &self.diagnostic
    }
}

pub(super) fn catch<T>(operation: impl FnOnce() -> T) -> Result<T, PanicPayload> {
    catch_unwind(AssertUnwindSafe(operation)).map_err(PanicPayload::contain)
}

pub(super) fn drop_contained<T>(value: T) -> Result<(), PanicPayload> {
    catch(|| drop(value))
}

fn builtin_diagnostic(payload: &(dyn Any + Send)) -> String {
    if let Some(value) = payload.downcast_ref::<&str>() {
        bounded(value)
    } else if let Some(value) = payload.downcast_ref::<String>() {
        bounded(value)
    } else {
        "non-string panic payload".to_owned()
    }
}

fn bounded(value: &str) -> String {
    if value.len() <= MAX_PANIC_DIAGNOSTIC {
        return value.to_owned();
    }
    let mut end = MAX_PANIC_DIAGNOSTIC;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}
