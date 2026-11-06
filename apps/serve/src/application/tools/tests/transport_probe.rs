use super::*;
use std::io::{BufRead, Read, Write};

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Bind { path: std::path::PathBuf },
    Start { request: Value },
}

#[derive(Deserialize)]
struct Channel {
    port: u16,
    key: String,
}

fn report(value: Value) {
    println!("RSI_TOOL_TRANSPORT {value}");
    std::io::stdout().flush().unwrap();
}

/// The benchmark controller owns the isolated Host and captured registrations.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "requires an isolated application listener and stdin fixture invocations"]
async fn native_tool_transport() {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut view = None;
    let mut invocations = 0;
    report(json!({"ready": true}));
    loop {
        let mut line = Vec::new();
        let count = Read::take(&mut input, (MAX_RESPONSE + 1) as u64)
            .read_until(b'\n', &mut line)
            .unwrap();
        if count == 0 {
            break;
        }
        assert!(line.len() <= MAX_RESPONSE, "probe request exceeds capacity");
        match serde_json::from_slice::<Command>(&line).expect("invalid probe command") {
            Command::Bind { path } => {
                let mut bytes = Vec::new();
                Read::take(std::fs::File::open(path).unwrap(), 4097)
                    .read_to_end(&mut bytes)
                    .unwrap();
                assert!(bytes.len() <= 4096, "listener descriptor exceeds capacity");
                let channel: Channel = serde_json::from_slice(&bytes).unwrap();
                let endpoint = Arc::new(Endpoint {
                    listener: Listener {
                        protocol: 14,
                        generation: "transport-probe".into(),
                        port: channel.port,
                    },
                    key: SecretValue::new(channel.key).unwrap(),
                    client: client.clone(),
                    retired: CancellationToken::new(),
                });
                let (_, current) = watch::channel(Some(endpoint.clone()));
                view = Some(captured_view(endpoint, current));
                report(json!({"bound": true}));
            }
            Command::Start { request } => {
                invocations += 1;
                let view = view.as_ref().expect("bind a listener before invoking it");
                let identity = json!({
                    "session":request["session"], "effect":request["effect"],
                    "registration":request["registration"], "identity":request["identity"],
                });
                let stop = CancellationToken::new();
                let started = std::time::Instant::now();
                let result = tokio::time::timeout(
                    Duration::from_secs(120),
                    view.start_and_observe(&request, &identity, &stop),
                )
                .await
                .expect("native tool probe exceeded its fixture deadline");
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                match result {
                    Ok(result) => report(json!({"ms": ms, "result": result})),
                    Err(error) => report(json!({"ms": ms, "error": error.to_string()})),
                }
            }
        }
    }
    assert!(invocations > 0, "probe received no tool invocations");
}
