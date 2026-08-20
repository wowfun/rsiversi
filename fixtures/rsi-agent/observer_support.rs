pub const QUERY: &[u8] = br#"{"kind":"snapshot","version":0}"#;

pub fn snapshot(
    open_attempts: u64,
    accepted_opens: u64,
    data_frames: u64,
    max_concurrent_streams: u64,
) -> Vec<u8> {
    format!(
        "{{\"accepted_opens\":{accepted_opens},\"data_frames\":{data_frames},\"kind\":\"snapshot\",\"max_concurrent_streams\":{max_concurrent_streams},\"open_attempts\":{open_attempts},\"version\":0}}"
    )
    .into_bytes()
}
