#![no_main]
libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    let Some((head, bytes)) = bytes.split_at_checked(2) else { return; };
    let selected = usize::from(u16::from_le_bytes([head[0], head[1]])) % 4096 + 1;
    let _ = rsi_mcp::exercise_frame_parser(bytes, selected);
});
