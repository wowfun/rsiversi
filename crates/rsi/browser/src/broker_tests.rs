use super::*;
use rsi_process::{
    DuplexControl, DuplexInput, DuplexOutput, DuplexRead, ProcessOutcome, ProcessOutput,
};
#[derive(Debug)]
struct Input(mpsc::Sender<Value>, std::sync::Mutex<Vec<u8>>);
#[async_trait::async_trait]
impl DuplexInput for Input {
    async fn write(&self, bytes: &[u8]) -> rsi_process::Result<usize> {
        let mut buffer = self.1.lock().unwrap();
        buffer.extend_from_slice(bytes);
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let packet = serde_json::from_slice(&buffer[..end]).unwrap();
            self.0.try_send(packet).unwrap();
            buffer.drain(..=end);
        }
        Ok(bytes.len())
    }
    async fn close(&self) -> rsi_process::Result<()> {
        Ok(())
    }
}
#[derive(Debug)]
struct Output(Mutex<mpsc::Receiver<Value>>);
#[async_trait::async_trait]
impl DuplexOutput for Output {
    async fn read(&self, _: usize) -> rsi_process::Result<DuplexRead> {
        let packet = self.0.lock().await.recv().await.unwrap();
        let mut bytes = serde_json::to_vec(&packet).unwrap();
        bytes.push(b'\n');
        Ok(DuplexRead { bytes, eof: false })
    }
}
#[derive(Debug)]
struct Control(Arc<Input>, Arc<Output>);
#[async_trait::async_trait]
impl DuplexControl for Control {
    fn pid(&self) -> u32 {
        1
    }
    fn stdin(&self) -> Arc<dyn DuplexInput> {
        self.0.clone()
    }
    fn stdout(&self) -> Arc<dyn DuplexOutput> {
        self.1.clone()
    }
    fn stderr(&self) -> Arc<dyn ProcessOutput> {
        unreachable!()
    }
    fn terminate(&self) {}
    async fn wait(&self) -> rsi_process::Result<ProcessOutcome> {
        unreachable!()
    }
    async fn wait_settlement(&self) -> rsi_process::Result<()> {
        Ok(())
    }
}
fn ports() -> (
    ManagedDuplexProcess,
    mpsc::Sender<Value>,
    mpsc::Receiver<Value>,
) {
    let (packets, output) = mpsc::channel(32);
    let (input, writes) = mpsc::channel(32);
    (
        ManagedDuplexProcess::new(Arc::new(Control(
            Arc::new(Input(input, std::sync::Mutex::new(vec![]))),
            Arc::new(Output(Mutex::new(output))),
        ))),
        packets,
        writes,
    )
}
struct PendingConnector(
    std::sync::atomic::AtomicUsize,
    std::sync::atomic::AtomicUsize,
);
struct Connecting<'a>(&'a std::sync::atomic::AtomicUsize);
impl Drop for Connecting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
#[async_trait::async_trait]
impl ProxyConnector for PendingConnector {
    async fn connect(&self, _: &str, _: u16) -> Result<tokio::net::TcpStream, String> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _held = Connecting(&self.1);
        std::future::pending().await
    }
}
#[tokio::test]
async fn eight_pending_destinations_leave_control_progress_and_retirement_available() {
    use std::sync::atomic::Ordering::SeqCst;
    let (browser, packets, mut replies) = ports();
    let (client, _client_packets, mut cdp) = ports();
    let scope = Arc::new(BridgeScope {
        browser,
        client,
        policy: BrowserPolicy {
            entry_url: "https://preview.example/".into(),
            path_prefix: "/".into(),
            dependency_hosts: std::collections::BTreeSet::new(),
        }
        .into(),
        resolver: Arc::new(rsi_retrieval::PublicDestinationResolver::new()),
        writes: Arc::new(Mutex::new(())),
        tasks: TaskTracker::new(),
        stop: CancellationToken::new(),
    });
    let (commands, _commands) = mpsc::channel(16);
    let (mcp, _mcp) = mpsc::channel(16);
    let connector = Arc::new(PendingConnector(0.into(), 0.into()));
    let bridge = tokio::spawn(bridge_with_connector(
        scope.clone(),
        commands,
        mcp,
        connector.clone(),
    ));
    for id in 1..=9 {
        packets
            .send(json!({"kind":"proxy_open","id":id,"host":"preview.example","port":443,"transport":"connect"}))
            .await
            .unwrap();
    }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), replies.recv())
            .await
            .unwrap()
            .unwrap(),
        json!({"kind":"proxy_close","id":9})
    );
    packets
        .send(json!({"kind":"cdp","value":r#"{"id":123,"result":{"value":"\ud800"}}"#}))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), cdp.recv())
            .await
            .unwrap()
            .unwrap()["value"],
        r#"{"id":123,"result":{"value":"\ud800"}}"#
    );
    assert_eq!(connector.0.load(SeqCst), 8);
    packets
        .send(json!({"kind":"proxy_close","id":1}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if replies.recv().await.unwrap()["id"] == 1 {
                break;
            }
        }
    })
    .await
    .expect("the proxy close receipt never arrived");
    packets
        .send(json!({"kind":"proxy_open","id":10,"host":"preview.example","port":443,"transport":"connect"}))
        .await
        .unwrap();
    packets
        .send(json!({"kind":"cdp","value":r#"{"id":124}"#}))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), cdp.recv())
            .await
            .unwrap()
            .unwrap()["value"],
        r#"{"id":124}"#
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while connector.0.load(SeqCst) != 9 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the settled destination did not release its socket slot for id 10");
    scope.stop.cancel();
    bridge.await.unwrap().unwrap();
    scope.tasks.close();
    scope.tasks.wait().await;
    assert_eq!(connector.1.load(SeqCst), 0);
}
