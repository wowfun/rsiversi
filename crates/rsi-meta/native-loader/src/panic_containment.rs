pub(crate) fn drop_caught_payload(payload: Box<dyn std::any::Any + Send>) -> bool {
    if let Err(recursive) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload)))
    {
        if let Err(final_payload) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(recursive)))
        {
            // Only a payload whose own destruction recursively panics twice is
            // impossible to destroy without escaping the Loader boundary.
            std::mem::forget(final_payload);
        }
        true
    } else {
        false
    }
}

pub(crate) fn contain_result<T>(result: std::thread::Result<T>) -> Result<T, bool> {
    match result {
        Ok(value) => Ok(value),
        Err(payload) => Err(drop_caught_payload(payload)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct PanickingPayloadDrop;

    impl Drop for PanickingPayloadDrop {
        fn drop(&mut self) {
            panic!("panic payload destructor panicked");
        }
    }

    #[test]
    fn recursive_payload_panic_is_contained() {
        let caught = std::panic::catch_unwind(|| std::panic::panic_any(PanickingPayloadDrop));
        assert_eq!(contain_result(caught), Err(true));
    }

    struct CountedPayload(Arc<AtomicUsize>);

    impl Drop for CountedPayload {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct PanicsWithCountedPayload(Arc<AtomicUsize>);

    impl Drop for PanicsWithCountedPayload {
        fn drop(&mut self) {
            std::panic::panic_any(CountedPayload(Arc::clone(&self.0)));
        }
    }

    #[test]
    fn recursive_panic_payload_is_destroyed_behind_a_second_boundary() {
        let drops = Arc::new(AtomicUsize::new(0));
        let caught = std::panic::catch_unwind({
            let drops = Arc::clone(&drops);
            move || std::panic::panic_any(PanicsWithCountedPayload(drops))
        });

        assert_eq!(contain_result(caught), Err(true));
        assert_eq!(
            drops.load(Ordering::Relaxed),
            1,
            "the recursive panic payload was leaked instead of destroyed"
        );
    }
}
