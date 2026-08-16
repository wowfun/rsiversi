use std::fs;

fn crash_gate_marker() -> Vec<u8> {
    ["RSI_META_CORE_", "TEST_CRASH_", "GATE"]
        .concat()
        .into_bytes()
}

fn executable_contains(marker: &[u8]) -> bool {
    let executable = std::env::current_exe().expect("current test executable");
    let bytes = fs::read(executable).expect("read current test executable");
    bytes.windows(marker.len()).any(|window| window == marker)
}

#[test]
#[cfg(not(feature = "test-failpoints"))]
fn default_build_omits_core_process_crash_gate_marker() {
    assert!(!executable_contains(&crash_gate_marker()));
}

#[test]
#[cfg(feature = "test-failpoints")]
fn explicit_failpoint_build_contains_core_process_crash_gate_marker() {
    std::hint::black_box(rsi_meta::__TEST_CRASH_GATE_MARKER);
    assert!(executable_contains(&crash_gate_marker()));
}
