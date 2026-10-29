use std::{
    io::{Read as _, Write as _},
    process::Command,
    time::Duration,
};

#[test]
fn plugin_download_uses_the_cli_runtime_and_reports_http_failure_without_panicking() {
    let fixture = tempfile::tempdir_in(rsi_workspace_path::canonical_native_path(
        std::env::temp_dir().canonicalize().unwrap(),
    ))
    .unwrap();
    let config = fixture.path().join("config");
    let state = fixture.path().join("state");
    let cache = fixture.path().join("cache");
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rsi"));
        command
            .args(["--config-home"])
            .arg(&config)
            .arg("--state-home")
            .arg(&state)
            .arg("--cache-home")
            .arg(&cache);
        command
    };
    let initialized = command().args(["config", "init"]).output().unwrap();
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(config.join("rsi.toml"))
        .unwrap()
        .write_all(
            format!(
                "\n[repository]\nname='fixture'\nurl='http://{address}/'\npublic_key='{}'\n",
                "00".repeat(32)
            )
            .as_bytes(),
        )
        .unwrap();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("download client did not connect: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            assert!(request.len() < 16 * 1024);
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    let output = command()
        .args(["plugin", "install", "web"])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!error.contains("panicked"), "{error}");
    assert!(error.contains("repository returned HTTP 404"), "{error}");
}
