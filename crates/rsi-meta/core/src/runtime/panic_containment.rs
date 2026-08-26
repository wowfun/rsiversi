pub(crate) fn drop_catching_unwind<T>(value: T) -> bool {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(value)));
    let Err(payload) = result else {
        return false;
    };
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload))) {
        // A panic payload whose own destructor panics cannot be destroyed
        // safely. Forget only the final hostile payload after both contained
        // attempts so cleanup and terminal publication can continue.
        std::mem::forget(payload);
    }
    true
}

/// Consumes a caught unwind payload through the recursive destructor boundary
/// before callers inspect the terminal result.
pub(crate) fn contain_panic_result<T>(result: std::thread::Result<T>) -> Result<T, bool> {
    match result {
        Ok(value) => Ok(value),
        Err(payload) => Err(drop_catching_unwind(payload)),
    }
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

    #[test]
    fn hostile_recursive_panic_payload_does_not_escape_drop_containment() {
        assert!(drop_catching_unwind(RecursivelyPanickingDrop));
    }

    struct PanickingPayloadDrop;

    impl Drop for PanickingPayloadDrop {
        fn drop(&mut self) {
            panic!("panic payload destructor panicked");
        }
    }

    #[test]
    fn caught_result_consumes_a_hostile_payload_before_returning() {
        let caught = std::panic::catch_unwind(|| std::panic::panic_any(PanickingPayloadDrop));
        assert_eq!(contain_panic_result(caught), Err(true));
    }
}
