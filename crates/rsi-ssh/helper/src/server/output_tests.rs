use super::*;
use rsi_process::{ProcessRead, Result};
use rsi_ssh_protocol::{execution::Prepared, rpc::Outcome};
use rsi_ssh_transport::{Role, StreamId};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct CountingTail {
    bytes: Vec<u8>,
    copied: AtomicUsize,
}
impl ProcessOutput for CountingTail {
    fn read_from(&self, offset: u64) -> Result<ProcessRead> {
        let start = usize::try_from(offset.saturating_sub(37))
            .unwrap()
            .min(self.bytes.len());
        let bytes = self.bytes[start..].to_vec();
        self.copied.fetch_add(bytes.len(), Ordering::Relaxed);
        Ok(ProcessRead {
            bytes,
            oldest_offset: 37,
            next_offset: 37 + self.bytes.len() as u64,
            lossy: offset < 37,
            full_output: None,
        })
    }
    fn peek_tail(&self, _: usize) -> Result<ProcessRead> {
        panic!("newest-only reads would discard retained output")
    }
}

#[tokio::test]
async fn fragmented_capture_copies_each_retained_byte_once_and_preserves_offsets() {
    let (left, right) = tokio::io::duplex(1024);
    let (lr, lw) = tokio::io::split(left);
    let (rr, rw) = tokio::io::split(right);
    let (client, _) = Connection::start(lr, lw, Role::Client, 1).unwrap();
    let (helper, _) = Connection::start(rr, rw, Role::Helper, 1).unwrap();
    let id = StreamId::new(0, 1).unwrap();
    let mut receive = client.open_receiver(id).unwrap();
    let send = helper.open_sender(id).unwrap();
    let source = Arc::new(CountingTail {
        bytes: vec![91; 4 * 1024 * 1024],
        copied: AtomicUsize::new(0),
    });
    let entry = Arc::new(Entry {
        prepared: Prepared {
            handle: 1,
            stdin: 65,
            stdout: 64,
            stderr: None,
            enforcement: rsi_sandbox::EnforcementStamp {
                requested: rsi_sandbox::SandboxMode::DangerFullAccess,
                backend: rsi_sandbox::SandboxBackend::Unconfined,
                workspace: "/workspace".into(),
                filesystem: rsi_sandbox::SandboxFileSystem::Unconfined,
                scratch: rsi_sandbox::SandboxScratch::Host,
                network: rsi_sandbox::SandboxNetwork::Host,
            },
        },
        plan: std::sync::Mutex::new(None),
        native: std::sync::Mutex::new(None),
        outcome: tokio::sync::watch::channel(Some(Ok(Outcome {
            exit_code: Some(0),
            signal: None,
        })))
        .0,
        settlement: tokio::sync::watch::channel(Some(Ok(()))).0,
        cancel: tokio_util::sync::CancellationToken::new(),
        terminated: false.into(),
        task: std::sync::Mutex::new(None),
    });
    let task = tokio::spawn(tail(source.clone(), send, entry));
    let mut bytes = Vec::new();
    let mut next = 37;
    while let Some(chunk) = receive.next().await.unwrap() {
        let (offset, part) = rsi_ssh_protocol::rpc::decode_tail(&chunk.bytes).unwrap();
        assert_eq!(offset, next);
        next += part.len() as u64;
        bytes.extend_from_slice(part);
    }
    task.await.unwrap();
    assert_eq!(bytes, source.bytes);
    assert_eq!(source.copied.load(Ordering::Relaxed), source.bytes.len());
    client.close();
    helper.close();
    client.settled().await;
    helper.settled().await;
}
