use super::BoundedDiagnostic;
use std::any::Any;
use std::panic::AssertUnwindSafe;

pub(super) type PanicPayload = Box<dyn Any + Send + 'static>;

pub(super) fn drop_catching_unwind<T>(value: T) -> bool {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| drop(value)));
    let Err(payload) = result else {
        return false;
    };
    if let Err(payload) = std::panic::catch_unwind(AssertUnwindSafe(|| drop(payload))) {
        // A recursively panicking payload cannot be destroyed safely. Forget
        // only this final hostile value so the ownership path can continue.
        std::mem::forget(payload);
    }
    true
}

pub(super) fn drop_caught_payload(payload: PanicPayload) -> bool {
    drop_catching_unwind(payload)
}

pub(super) fn caught_panic(
    payload: PanicPayload,
    ordinary: &'static str,
    payload_destruction: &'static str,
    maximum: usize,
) -> BoundedDiagnostic {
    let message = if drop_caught_payload(payload) {
        payload_destruction
    } else {
        ordinary
    };
    BoundedDiagnostic::from_string(message.to_owned(), maximum)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecursivelyPanickingDrop;

    impl Drop for RecursivelyPanickingDrop {
        fn drop(&mut self) {
            std::panic::panic_any(Self);
        }
    }

    fn contain_escaped_payload(payload: PanicPayload) {
        let first = std::panic::catch_unwind(AssertUnwindSafe(|| drop(payload)));
        if let Err(payload) = first
            && let Err(payload) = std::panic::catch_unwind(AssertUnwindSafe(|| drop(payload)))
        {
            std::mem::forget(payload);
        }
    }

    #[test]
    fn recursively_panicking_payload_destruction_does_not_escape() {
        let escaped = match std::panic::catch_unwind(AssertUnwindSafe(|| {
            drop_caught_payload(Box::new(RecursivelyPanickingDrop))
        })) {
            Ok(drop_panicked) => {
                assert!(drop_panicked);
                false
            }
            Err(payload) => {
                contain_escaped_payload(payload);
                true
            }
        };
        assert!(
            !escaped,
            "recursive payload destruction escaped containment"
        );
    }
}
