//! Standalone std-only peer for native process/control lifecycle tests.
use std::{
    fs,
    io::{BufRead, Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if args.get(1).is_some_and(|arg| arg == "--hang") {
        loop {
            thread::park();
        }
    }
    if args.get(1).is_some_and(|arg| arg == "--watcher") {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes).unwrap();
        thread::sleep(Duration::from_millis(50));
        fs::write(&args[2], "eof").unwrap();
        return;
    }
    if args.get(1).is_some_and(|arg| arg == "--descendant") {
        let path = Path::new(&args[2]);
        for counter in 0_u64.. {
            fs::write(path, counter.to_string()).unwrap();
            thread::sleep(Duration::from_millis(10));
        }
        return;
    }
    assert_eq!(&args[1..], ["__dev", "web"]);
    let bundle = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let mode = fs::read_to_string(bundle.join("mode")).unwrap();
    let mut input = std::io::stdin().lock();
    let mut initial = String::new();
    input.read_line(&mut initial).unwrap();
    assert!(initial.contains("\"event\":\"start_backend\""));
    let run = initial
        .split("\"run_id\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let send = |message: &str| {
        println!("{{\"version\":1,\"run_id\":\"{run}\",\"message\":{message}}}");
        std::io::stdout().flush().unwrap();
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    if mode == "wrong-order" {
        send("{\"event\":\"proxy_ready\"}");
    } else {
        send(&format!(
            "{{\"event\":\"backend_ready\",\"address\":\"{address}\"}}"
        ));
    }
    let mut release = String::new();
    if input.read_line(&mut release).unwrap() == 0 {
        fs::write(bundle.join("closed"), "eof").unwrap();
        return;
    }
    assert!(release.contains("\"event\":\"release_launch\""));
    let _descendant = if mode == "descendant" {
        Some(
            Command::new(std::env::current_exe().unwrap())
                .args(["--descendant", bundle.join("heartbeat").to_str().unwrap()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    } else {
        None
    };
    send(&format!(
        "{{\"event\":\"launch_ready\",\"url\":\"http://{address}#rsi-launch={}\",\"open_browser\":false}}",
        "a".repeat(64)
    ));
    fs::write(bundle.join("launched"), "ready").unwrap();
    if mode == "unexpected-exit" {
        std::process::exit(7);
    }
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut bytes = [0; 2048];
            let _ = stream.read(&mut bytes);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });
    let mut bytes = Vec::new();
    input.read_to_end(&mut bytes).unwrap();
    thread::sleep(Duration::from_millis(50));
    fs::write(bundle.join("closed"), "eof").unwrap();
}
