use super::*;

#[test]
fn intercept_bound_applies_to_the_accumulated_overlay() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_frame_bytes: 8,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let first = runtime.root().intercept("echo", Value::from("a")).unwrap();
    assert_eq!(
        first.intercept("echo", Value::from("a")).unwrap_err(),
        MetaError::PayloadTooLarge { maximum: 8 }
    );
}
