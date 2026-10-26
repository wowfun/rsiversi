#![no_main]
libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    let _ = rsi_apply_patch::exercise_patch_parser(bytes);
});
