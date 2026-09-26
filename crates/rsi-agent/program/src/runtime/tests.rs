use super::*;
use rsi_process::DuplexRead;
use std::{collections::VecDeque, sync::atomic::AtomicUsize};

#[derive(Debug, Default)]
struct Port {
    incoming: Mutex<VecDeque<u8>>,
    written: Mutex<Vec<u8>>,
    read_bytes: AtomicUsize,
    hold_open: bool,
}
#[async_trait]
impl DuplexInput for Port {
    async fn write(&self, bytes: &[u8]) -> rsi_process::Result<usize> {
        let count = bytes.len().min(7);
        self.written
            .lock()
            .unwrap()
            .extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    async fn close(&self) -> rsi_process::Result<()> {
        Ok(())
    }
}
#[async_trait]
impl DuplexOutput for Port {
    async fn read(&self, maximum: usize) -> rsi_process::Result<DuplexRead> {
        let bytes: Vec<_> = {
            let mut incoming = self.incoming.lock().unwrap();
            let count = maximum.min(13).min(incoming.len());
            incoming.drain(..count).collect()
        };
        if bytes.is_empty() && self.hold_open {
            std::future::pending::<()>().await;
        }
        self.read_bytes.fetch_add(bytes.len(), Ordering::SeqCst);
        Ok(DuplexRead {
            eof: bytes.is_empty(),
            bytes,
        })
    }
}
fn frames(values: impl IntoIterator<Item = Value>, hold_open: bool) -> Arc<Port> {
    let port = Arc::new(Port {
        hold_open,
        ..Port::default()
    });
    for value in values {
        let bytes = serde_json::to_vec(&value).unwrap();
        port.incoming
            .lock()
            .unwrap()
            .extend(u32::try_from(bytes.len()).unwrap().to_be_bytes());
        port.incoming.lock().unwrap().extend(bytes);
    }
    port
}
#[derive(Debug, Default)]
struct Rpc {
    entered: AtomicUsize,
    settled: AtomicUsize,
    notified: tokio::sync::Notify,
    blocked: bool,
}
#[async_trait]
impl ProgramRpc for Rpc {
    fn definitions(&self) -> Value {
        json!([])
    }
    async fn call(
        &self,
        _: String,
        args: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, String> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        self.notified.notify_one();
        if self.blocked {
            cancellation.cancelled().await;
        }
        self.settled.fetch_add(1, Ordering::SeqCst);
        Ok(args)
    }
}
fn request(rpc: Arc<Rpc>) -> Request {
    use rsi_sandbox::{
        ConfinedProcess, EnforcementStamp, ProcessStdio, SandboxBackend, SandboxFileSystem,
        SandboxMode, SandboxNetwork, SandboxScratch,
    };
    let cwd = std::env::current_dir().unwrap();
    Request {
        spec: DuplexProcessSpec {
            process: ConfinedProcess {
                owner: None,
                stdio: ProcessStdio::Pipes,
                program: std::env::current_exe().unwrap(),
                arguments: vec![],
                cwd: cwd.clone(),
                stamp: EnforcementStamp {
                    requested: SandboxMode::DangerFullAccess,
                    backend: SandboxBackend::Unconfined,
                    workspace: cwd,
                    filesystem: SandboxFileSystem::Unconfined,
                    scratch: SandboxScratch::Host,
                    network: SandboxNetwork::Host,
                },
            },
            environment: vec![],
            stdout_buffer_bytes: 1024,
            stderr_max_bytes: 1024,
            termination_grace_ms: 1,
        },
        script: "return 42".into(),
        rpc,
        start: CancellationToken::new(),
        cancel: CancellationToken::new(),
        cancelled_at_settlement: AtomicBool::new(false),
        outcome: watch::channel(None).0,
    }
}
#[tokio::test]
async fn oversized_prefix_is_rejected_before_body_read_and_oversized_write_emits_nothing() {
    for size in [0, MAXIMUM_FRAME + 1] {
        let port = Arc::new(Port::default());
        port.incoming
            .lock()
            .unwrap()
            .extend(u32::try_from(size).unwrap().to_be_bytes());
        port.incoming.lock().unwrap().extend(b"body");
        assert!(
            read_frame(port.clone())
                .await
                .err()
                .unwrap()
                .contains("1 MiB")
        );
        assert_eq!(port.read_bytes.load(Ordering::SeqCst), 4);
    }
    let port = Port::default();
    let maximum = Value::String("x".repeat(MAXIMUM_FRAME - 2));
    write_frame(&port, &maximum, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(port.written.lock().unwrap().len(), MAXIMUM_FRAME + 4);
    port.written.lock().unwrap().clear();
    assert!(
        write_frame(
            &port,
            &Value::String("x".repeat(MAXIMUM_FRAME - 1)),
            &CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert!(port.written.lock().unwrap().is_empty());
}
#[tokio::test]
async fn fragmented_duplex_preserves_reply_and_exact_result_bound() {
    for extra in [0, 1] {
        let rpc = Arc::new(Rpc::default());
        let req = request(rpc.clone());
        let value =
            Value::String("中".repeat((MAXIMUM_RESULT - 2) / 3) + "xx" + &"x".repeat(extra));
        assert_eq!(
            serde_json::to_vec(&value).unwrap().len(),
            MAXIMUM_RESULT + extra
        );
        let port = frames(
            [
                json!({"type":"call","id":1,"method":"echo","arguments":{"text":"中\n\""}}),
                json!({"type":"result","value":value}),
            ],
            false,
        );
        let result = exchange(port.clone(), port.clone(), &req).await;
        if extra == 0 {
            assert_eq!(result.unwrap(), value);
        } else {
            assert!(result.unwrap_err().contains("256 KiB"));
        }
        assert_eq!(rpc.settled.load(Ordering::SeqCst), 1);
        let bytes = port.written.lock().unwrap();
        let mut offset = 0;
        let mut output = Vec::new();
        while offset < bytes.len() {
            let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;
            output.push(serde_json::from_slice::<Value>(&bytes[offset..offset + length]).unwrap());
            offset += length;
        }
        assert_eq!(output.len(), 2);
        assert_eq!(
            output[1],
            json!({"type":"reply","id":1,"value":{"text":"中\n\""}})
        );
    }
}
#[tokio::test]
async fn excessive_rpc_admission_cancels_and_joins_all_sixteen_handlers() {
    let rpc = Arc::new(Rpc {
        blocked: true,
        ..Rpc::default()
    });
    let req = request(rpc.clone());
    let port = frames(
        (1..=MAXIMUM_PROGRAM_OUTSTANDING_CALLS + 1)
            .map(|id| json!({"type":"call","id":id,"method":"echo","arguments":null})),
        true,
    );
    let result = exchange(port.clone(), port, &req).await.unwrap_err();
    assert!(result.contains("excessive"));
    assert_eq!(
        rpc.entered.load(Ordering::SeqCst),
        MAXIMUM_PROGRAM_OUTSTANDING_CALLS
    );
    assert_eq!(
        rpc.settled.load(Ordering::SeqCst),
        MAXIMUM_PROGRAM_OUTSTANDING_CALLS
    );
}
#[tokio::test]
async fn cancellation_joins_admitted_rpc_before_returning() {
    let rpc = Arc::new(Rpc {
        blocked: true,
        ..Rpc::default()
    });
    let req = request(rpc.clone());
    let port = frames(
        [json!({"type":"call","id":1,"method":"echo","arguments":null})],
        true,
    );
    let exchange = exchange(port.clone(), port, &req);
    tokio::pin!(exchange);
    tokio::select! { result = &mut exchange => panic!("unexpected settlement {result:?}"), () = rpc.notified.notified() => {} }
    req.cancel.cancel();
    assert!(exchange.await.unwrap_err().contains("cancelled"));
    assert_eq!(rpc.settled.load(Ordering::SeqCst), 1);
}
