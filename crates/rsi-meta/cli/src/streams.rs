use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};

use crate::host::{BoxHostServiceStream, SharedHost};
use crate::protocol::{
    CONTROL_PROTOCOL, CommandEnvelope, STREAM_PROTOCOL, ServiceOpenRequest, StreamEnvelope,
    StreamId, StreamKind,
};

const STREAM_COMMAND_CAPACITY: usize = 32;
const STREAM_OUTPUT_CAPACITY: usize = 128;
const MAX_STREAMS_PER_CONNECTION: usize = 128;
const STREAM_DATA_MAGIC: &[u8; 4] = b"RSD0";
const STREAM_DATA_HEADER_BYTES: usize = STREAM_DATA_MAGIC.len() + 2 + 8 + 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StreamDataLimitExceeded;

impl std::fmt::Display for StreamDataLimitExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("binary DATA frame exceeds the configured message limit")
    }
}

impl std::error::Error for StreamDataLimitExceeded {}

#[derive(Debug)]
pub enum WireEnvelope {
    Control(CommandEnvelope),
    Stream(StreamEnvelope),
}

pub fn decode_wire_envelope(encoded: &str) -> Result<WireEnvelope> {
    let value: Value = serde_json::from_str(encoded).context("decode wire envelope")?;
    let protocol = value
        .get("protocol")
        .and_then(Value::as_str)
        .context("wire envelope protocol is required")?;
    match protocol {
        CONTROL_PROTOCOL => serde_json::from_value(value)
            .map(WireEnvelope::Control)
            .context("decode control envelope"),
        STREAM_PROTOCOL => serde_json::from_value(value)
            .map(WireEnvelope::Stream)
            .context("decode stream envelope"),
        other => bail!("unsupported wire protocol {other:?}"),
    }
}

/// Encodes one validated DATA envelope as an RSD0 binary record.
///
/// # Errors
///
/// Returns an error when the envelope is not canonical DATA, carries
/// extensions, or exceeds an RSD0 length field.
pub fn encode_stream_data(frame: &StreamEnvelope) -> Result<Vec<u8>> {
    encode_stream_data_bounded(frame, usize::MAX)
}

pub(crate) fn encode_stream_data_bounded(
    frame: &StreamEnvelope,
    maximum_bytes: usize,
) -> Result<Vec<u8>> {
    frame.validate().context("validate DATA stream envelope")?;
    if frame.kind != StreamKind::Data {
        bail!("binary stream encoding requires a DATA frame");
    }
    if !frame.extensions.is_empty() {
        bail!("binary DATA frames cannot carry extensions");
    }
    let sequence = frame.sequence.context("DATA sequence is required")?;
    let data = frame.data.as_deref().context("DATA bytes are required")?;
    let stream_len = u16::try_from(frame.stream_id.len()).context("stream_id is too large")?;
    let data_len = u32::try_from(data.len()).context("DATA payload is too large")?;
    let capacity = STREAM_DATA_HEADER_BYTES
        .checked_add(frame.stream_id.len())
        .and_then(|bytes| bytes.checked_add(data.len()))
        .context("DATA frame size overflow")?;
    if capacity > maximum_bytes {
        return Err(StreamDataLimitExceeded.into());
    }
    let mut encoded = Vec::with_capacity(capacity);
    encoded.extend_from_slice(STREAM_DATA_MAGIC);
    encoded.extend_from_slice(&stream_len.to_be_bytes());
    encoded.extend_from_slice(&sequence.to_be_bytes());
    encoded.extend_from_slice(&data_len.to_be_bytes());
    encoded.extend_from_slice(frame.stream_id.as_bytes());
    encoded.extend_from_slice(data);
    Ok(encoded)
}

/// Decodes and validates one complete RSD0 binary DATA record.
///
/// # Errors
///
/// Returns an error for an invalid header, inconsistent lengths, an invalid
/// stream identifier, trailing bytes, or a non-canonical DATA envelope.
pub fn decode_stream_data(encoded: &[u8]) -> Result<StreamEnvelope> {
    if encoded.len() < STREAM_DATA_HEADER_BYTES || !encoded.starts_with(STREAM_DATA_MAGIC) {
        bail!("invalid binary DATA frame header");
    }
    let stream_len = usize::from(u16::from_be_bytes([encoded[4], encoded[5]]));
    let sequence = u64::from_be_bytes(
        encoded[6..14]
            .try_into()
            .context("invalid binary DATA sequence field")?,
    );
    let data_len = usize::try_from(u32::from_be_bytes(
        encoded[14..18]
            .try_into()
            .context("invalid binary DATA length field")?,
    ))
    .context("DATA payload length is unsupported")?;
    let stream_end = STREAM_DATA_HEADER_BYTES
        .checked_add(stream_len)
        .context("DATA frame size overflow")?;
    let data_end = stream_end
        .checked_add(data_len)
        .context("DATA frame size overflow")?;
    if data_end != encoded.len() {
        bail!("binary DATA frame length does not match its header");
    }
    let stream_id = std::str::from_utf8(&encoded[STREAM_DATA_HEADER_BYTES..stream_end])
        .context("binary DATA stream_id is not UTF-8")?;
    let mut frame = StreamEnvelope::new(
        StreamId::new(stream_id).context("binary DATA stream_id is invalid")?,
        StreamKind::Data,
    );
    frame.sequence = Some(sequence);
    frame.data = Some(encoded[stream_end..].to_vec());
    frame.validate().context("validate binary DATA frame")?;
    Ok(frame)
}

pub fn is_stream_data(encoded: &[u8]) -> bool {
    encoded.starts_with(STREAM_DATA_MAGIC)
}

#[derive(Debug)]
pub struct StreamRouter {
    host: SharedHost,
    streams: BTreeMap<StreamId, StreamEntry>,
    output_sender: mpsc::Sender<StreamEnvelope>,
    output_receiver: mpsc::Receiver<StreamEnvelope>,
    completion_sender: mpsc::Sender<(StreamId, u64)>,
    completion_receiver: mpsc::Receiver<(StreamId, u64)>,
    tasks: JoinSet<()>,
    next_task_id: u64,
}

#[derive(Debug)]
struct StreamEntry {
    input: mpsc::Sender<StreamEnvelope>,
    task: AbortHandle,
    task_id: u64,
}

impl StreamRouter {
    pub fn new(host: SharedHost) -> Self {
        let (output_sender, output_receiver) = mpsc::channel(STREAM_OUTPUT_CAPACITY);
        let (completion_sender, completion_receiver) = mpsc::channel(MAX_STREAMS_PER_CONNECTION);
        Self {
            host,
            streams: BTreeMap::new(),
            output_sender,
            output_receiver,
            completion_sender,
            completion_receiver,
            tasks: JoinSet::new(),
            next_task_id: 1,
        }
    }

    pub async fn recv(&mut self) -> Option<StreamEnvelope> {
        loop {
            self.reap_completed();
            tokio::select! {
                completed = self.completion_receiver.recv() => {
                    if let Some((stream_id, task_id)) = completed
                        && self.streams.get(&stream_id).is_some_and(|entry| entry.task_id == task_id)
                    {
                        self.streams.remove(&stream_id);
                    }
                }
                frame = self.output_receiver.recv() => {
                    if let Some(frame) = &frame
                        && matches!(frame.kind, StreamKind::End | StreamKind::Cancel)
                    {
                        self.streams.remove(&frame.stream_id);
                    }
                    return frame;
                }
            }
        }
    }

    pub fn route(&mut self, frame: StreamEnvelope) -> Result<()> {
        self.reap_completed();
        let stream_id = frame.stream_id.clone();
        if let Err(error) = frame.validate().context("validate stream envelope") {
            self.abort_stream(&stream_id);
            return Err(error);
        }
        if frame.kind == StreamKind::Open {
            return self.open(frame);
        }
        let result = self
            .streams
            .get(&stream_id)
            .with_context(|| format!("unknown connection stream {stream_id:?}"))?
            .input
            .try_send(frame);
        if let Err(error) = result {
            self.abort_stream(&stream_id);
            return Err(anyhow::anyhow!("route stream frame: {error}"));
        }
        Ok(())
    }

    fn open(&mut self, frame: StreamEnvelope) -> Result<()> {
        if self.streams.contains_key(&frame.stream_id) {
            self.abort_stream(&frame.stream_id);
            bail!("connection stream {:?} is already open", frame.stream_id);
        }
        if self.streams.len() >= MAX_STREAMS_PER_CONNECTION {
            bail!("connection stream limit of {MAX_STREAMS_PER_CONNECTION} has been reached");
        }
        if frame.sequence.is_some() || frame.credit_bytes.is_some() {
            bail!("OPEN frames cannot carry sequence or credit_bytes");
        }
        let payload = frame.payload.context("OPEN frame payload is required")?;
        let request: ServiceOpenRequest =
            serde_json::from_value(payload).context("decode OPEN service request")?;
        let stream = self.host.open_service(request)?;
        let provider = stream.provider().0.clone();
        let external_id = frame.stream_id;
        let (sender, receiver) = mpsc::channel(STREAM_COMMAND_CAPACITY);

        let mut opened = StreamEnvelope::new(external_id.clone(), StreamKind::Open);
        opened.payload = Some(json!({"provider": provider}));
        self.output_sender
            .try_send(opened)
            .context("queue OPEN acknowledgement")?;

        let task_id = self.next_task_id;
        self.next_task_id = self
            .next_task_id
            .checked_add(1)
            .expect("connection stream task id exhausted u64");
        let output = self.output_sender.clone();
        let completion = self.completion_sender.clone();
        let task = self.tasks.spawn(run_stream(
            external_id.clone(),
            task_id,
            stream,
            receiver,
            output,
            completion,
        ));
        self.streams.insert(
            external_id,
            StreamEntry {
                input: sender,
                task,
                task_id,
            },
        );
        Ok(())
    }

    fn reap_completed(&mut self) {
        while let Ok((stream_id, task_id)) = self.completion_receiver.try_recv() {
            if self
                .streams
                .get(&stream_id)
                .is_some_and(|entry| entry.task_id == task_id)
            {
                self.streams.remove(&stream_id);
            }
        }
        while self.tasks.try_join_next().is_some() {}
    }

    fn abort_stream(&mut self, stream_id: &StreamId) {
        if let Some(entry) = self.streams.remove(stream_id) {
            entry.task.abort();
        }
    }

    pub async fn disconnect(mut self) {
        // Aborting drops every generation-pinned core ServiceStream. Its Drop
        // contract sends a host-owned `client_disconnected` cancellation, so a
        // dead transport cannot leave an orphaned plugin stream.
        self.tasks.abort_all();
        while self.tasks.join_next().await.is_some() {}
    }
}

enum StreamActivity {
    Input(Option<StreamEnvelope>),
    Output(Option<Result<StreamEnvelope>>),
}

async fn run_stream(
    external_id: StreamId,
    task_id: u64,
    mut stream: BoxHostServiceStream,
    mut input: mpsc::Receiver<StreamEnvelope>,
    output: mpsc::Sender<StreamEnvelope>,
    completion: mpsc::Sender<(StreamId, u64)>,
) {
    let mut next_input_sequence = 1_u64;
    loop {
        let activity = tokio::select! {
            frame = input.recv() => StreamActivity::Input(frame),
            frame = stream.recv() => StreamActivity::Output(frame),
        };
        let result = match activity {
            StreamActivity::Input(Some(frame)) => {
                handle_input(&mut *stream, frame, &mut next_input_sequence).await
            }
            StreamActivity::Input(None) | StreamActivity::Output(None) => break,
            StreamActivity::Output(Some(Ok(mut frame))) => {
                frame.stream_id.clone_from(&external_id);
                let terminal = matches!(frame.kind, StreamKind::End | StreamKind::Cancel);
                if output.send(frame).await.is_err() || terminal {
                    break;
                }
                continue;
            }
            StreamActivity::Output(Some(Err(error))) => Err(error),
        };

        match result {
            Ok(InputOutcome::Continue) => {}
            Ok(InputOutcome::Terminal(frame)) => {
                let mut frame = *frame;
                frame.stream_id.clone_from(&external_id);
                let _ = output.send(frame).await;
                break;
            }
            Err(error) => {
                let _ = output
                    .send(cancel_envelope(
                        &external_id,
                        "stream_error",
                        &format!("{error:#}"),
                    ))
                    .await;
                break;
            }
        }
    }
    // The lane is bounded to the maximum number of active actors. `try_send`
    // cannot deadlock a terminating stream if the transport itself is stalled.
    let _ = completion.try_send((external_id, task_id));
}

enum InputOutcome {
    Continue,
    Terminal(Box<StreamEnvelope>),
}

async fn handle_input(
    stream: &mut dyn crate::host::HostServiceStream,
    frame: StreamEnvelope,
    next_sequence: &mut u64,
) -> Result<InputOutcome> {
    match frame.kind {
        StreamKind::Data => {
            require_sequence(&frame, next_sequence)?;
            let bytes = frame.data.context("DATA frame bytes are required")?;
            stream.send(&bytes).await?;
            Ok(InputOutcome::Continue)
        }
        StreamKind::Credit => {
            stream
                .grant_credit(frame.credit_bytes.context("credit_bytes is required")?)
                .await?;
            Ok(InputOutcome::Continue)
        }
        StreamKind::HalfClose => {
            if frame.sequence.is_some() || frame.payload.is_some() {
                bail!("HALF_CLOSE frames cannot carry sequence or payload");
            }
            stream.half_close().await?;
            Ok(InputOutcome::Continue)
        }
        StreamKind::Cancel => {
            let reason = frame
                .payload
                .as_ref()
                .and_then(|payload| payload.get("reason"))
                .and_then(Value::as_str)
                .context("CANCEL payload.reason is required")?
                .to_owned();
            stream.cancel(reason.clone()).await?;
            // Core cancellation is host-owned and terminal. The adapter emits
            // exactly one connection-ID terminal acknowledgement here, then
            // drops the already-terminal core stream without forwarding its
            // internal-ID terminal frame a second time.
            let mut cancelled = StreamEnvelope::new(frame.stream_id, StreamKind::Cancel);
            cancelled.payload = Some(json!({"reason": reason}));
            Ok(InputOutcome::Terminal(Box::new(cancelled)))
        }
        StreamKind::Open => bail!("connection stream is already open"),
        StreamKind::End => bail!("END is server-to-client only"),
    }
}

fn require_sequence(frame: &StreamEnvelope, next_sequence: &mut u64) -> Result<()> {
    let received = frame.sequence.context("stream sequence is required")?;
    if received != *next_sequence {
        bail!(
            "stream sequence out of order: expected {}, received {received}",
            *next_sequence
        );
    }
    *next_sequence = next_sequence
        .checked_add(1)
        .context("stream sequence exhausted u64")?;
    Ok(())
}

pub fn cancel_envelope(stream_id: &StreamId, code: &str, message: &str) -> StreamEnvelope {
    let mut envelope = StreamEnvelope::new(stream_id.clone(), StreamKind::Cancel);
    envelope.payload = Some(json!({"reason": code, "message": message}));
    envelope
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fmt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use anyhow::Result;
    use async_trait::async_trait;

    use super::*;
    use crate::host::{HostApi, HostEventStream, HostServiceStream};
    use crate::protocol::{
        CommandEnvelope, CommandOutcomeEnvelope, EventEnvelope, GraphRevision, InstanceId,
    };

    fn sid(value: impl Into<String>) -> StreamId {
        StreamId::new(value).expect("valid fixture stream id")
    }

    struct ProbeStream {
        dropped: Arc<AtomicBool>,
        provider: InstanceId,
    }

    impl fmt::Debug for ProbeStream {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("ProbeStream")
        }
    }

    impl Drop for ProbeStream {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    #[async_trait]
    impl HostServiceStream for ProbeStream {
        fn provider(&self) -> &InstanceId {
            &self.provider
        }

        async fn send(&mut self, _payload: &[u8]) -> Result<()> {
            Ok(())
        }

        async fn grant_credit(&mut self, _bytes: u64) -> Result<()> {
            Ok(())
        }

        async fn recv(&mut self) -> Option<Result<StreamEnvelope>> {
            std::future::pending().await
        }

        async fn half_close(&mut self) -> Result<()> {
            Ok(())
        }

        async fn cancel(&mut self, _reason: String) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct ProbeHost {
        dropped: Arc<AtomicBool>,
    }

    #[async_trait]
    impl HostApi for ProbeHost {
        async fn submit(&self, _command: CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
            anyhow::bail!("control is unused in stream tests")
        }

        async fn subscribe(&self, _after_cursor: u64) -> Result<HostEventStream> {
            Ok(Box::pin(futures_util::stream::pending::<
                Result<EventEnvelope>,
            >()))
        }

        fn graph_revision(&self) -> GraphRevision {
            GraphRevision(0)
        }

        fn token_generation(&self) -> u64 {
            0
        }

        fn open_service(&self, _request: ServiceOpenRequest) -> Result<BoxHostServiceStream> {
            Ok(Box::new(ProbeStream {
                dropped: self.dropped.clone(),
                provider: InstanceId::new("provider"),
            }))
        }

        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
    }

    struct BurstStream {
        provider: InstanceId,
        output: VecDeque<StreamEnvelope>,
    }

    impl fmt::Debug for BurstStream {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("BurstStream")
                .field("provider", &self.provider)
                .field("remaining", &self.output.len())
                .finish()
        }
    }

    #[async_trait]
    impl HostServiceStream for BurstStream {
        fn provider(&self) -> &InstanceId {
            &self.provider
        }

        async fn send(&mut self, _payload: &[u8]) -> Result<()> {
            Ok(())
        }

        async fn grant_credit(&mut self, _bytes: u64) -> Result<()> {
            Ok(())
        }

        async fn recv(&mut self) -> Option<Result<StreamEnvelope>> {
            match self.output.pop_front() {
                Some(frame) => Some(Ok(frame)),
                None => std::future::pending().await,
            }
        }

        async fn half_close(&mut self) -> Result<()> {
            Ok(())
        }

        async fn cancel(&mut self, _reason: String) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct BurstHost;

    #[async_trait]
    impl HostApi for BurstHost {
        async fn submit(&self, _command: CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
            anyhow::bail!("control is unused in stream tests")
        }

        async fn subscribe(&self, _after_cursor: u64) -> Result<HostEventStream> {
            Ok(Box::pin(futures_util::stream::pending::<
                Result<EventEnvelope>,
            >()))
        }

        fn graph_revision(&self) -> GraphRevision {
            GraphRevision(0)
        }

        fn token_generation(&self) -> u64 {
            0
        }

        fn open_service(&self, _request: ServiceOpenRequest) -> Result<BoxHostServiceStream> {
            let mut output = (1..=STREAM_OUTPUT_CAPACITY)
                .map(|sequence| {
                    let mut frame = StreamEnvelope::new(sid("internal"), StreamKind::Data);
                    frame.sequence = Some(sequence as u64);
                    frame.data = Some(vec![u8::try_from(sequence % 256).unwrap()]);
                    frame
                })
                .collect::<VecDeque<_>>();
            output.push_back(StreamEnvelope::new(sid("internal"), StreamKind::End));
            Ok(Box::new(BurstStream {
                provider: InstanceId::new("provider"),
                output,
            }))
        }

        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
    }

    fn open(stream_id: &str) -> StreamEnvelope {
        let mut frame = StreamEnvelope::new(sid(stream_id), StreamKind::Open);
        frame.payload = Some(json!({
            "consumer": "consumer",
            "service": "fixture.echo",
        }));
        frame
    }

    #[tokio::test]
    async fn disconnect_drops_every_connection_bound_core_stream() {
        let dropped = Arc::new(AtomicBool::new(false));
        let host = Arc::new(ProbeHost {
            dropped: dropped.clone(),
        });
        let mut router = StreamRouter::new(host);
        router.route(open("local-id")).unwrap();
        let opened = router.recv().await.unwrap();
        assert_eq!(opened.kind, StreamKind::Open);
        assert_eq!(opened.payload, Some(json!({"provider": "provider"})));

        router.disconnect().await;
        assert!(dropped.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn duplicate_open_terminates_the_original_connection_stream() {
        let dropped = Arc::new(AtomicBool::new(false));
        let host = Arc::new(ProbeHost {
            dropped: dropped.clone(),
        });
        let mut router = StreamRouter::new(host);
        router.route(open("duplicate")).unwrap();
        assert_eq!(router.recv().await.unwrap().kind, StreamKind::Open);

        assert!(router.route(open("duplicate")).is_err());
        tokio::task::yield_now().await;
        assert!(!router.streams.contains_key("duplicate"));
        assert!(dropped.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn out_of_order_data_cancels_only_that_stream() {
        let host = Arc::new(ProbeHost {
            dropped: Arc::new(AtomicBool::new(false)),
        });
        let mut router = StreamRouter::new(host);
        router.route(open("ordered")).unwrap();
        assert_eq!(router.recv().await.unwrap().kind, StreamKind::Open);

        let mut data = StreamEnvelope::new(sid("ordered"), StreamKind::Data);
        data.sequence = Some(2);
        data.data = Some(vec![1, 2, 3]);
        router.route(data).unwrap();
        let cancelled = router.recv().await.unwrap();
        assert_eq!(cancelled.kind, StreamKind::Cancel);
        assert!(
            cancelled.payload.unwrap()["message"]
                .as_str()
                .unwrap()
                .contains("expected 1, received 2")
        );

        router.disconnect().await;
    }

    #[tokio::test]
    async fn invalid_frame_terminates_the_existing_stream_actor() {
        let dropped = Arc::new(AtomicBool::new(false));
        let host = Arc::new(ProbeHost {
            dropped: dropped.clone(),
        });
        let mut router = StreamRouter::new(host);
        router.route(open("invalid")).unwrap();
        assert_eq!(router.recv().await.unwrap().kind, StreamKind::Open);

        let invalid = StreamEnvelope::new(sid("invalid"), StreamKind::Credit);
        assert!(router.route(invalid).is_err());
        tokio::task::yield_now().await;

        assert!(
            dropped.load(Ordering::Acquire),
            "the terminal invalid_stream_frame response must also drop the core stream"
        );
        router.disconnect().await;
    }

    #[tokio::test]
    async fn saturated_connection_output_preserves_data_and_the_terminal_frame() {
        let mut router = StreamRouter::new(Arc::new(BurstHost));
        router.route(open("burst")).unwrap();

        assert_eq!(router.recv().await.unwrap().kind, StreamKind::Open);
        for expected_sequence in 1..=STREAM_OUTPUT_CAPACITY {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(1), router.recv())
                .await
                .expect("stream output stalled")
                .expect("stream output ended");
            assert_eq!(frame.kind, StreamKind::Data);
            assert_eq!(frame.sequence, Some(expected_sequence as u64));
        }
        let terminal = tokio::time::timeout(std::time::Duration::from_secs(1), router.recv())
            .await
            .expect("terminal frame was lost when the shared output queue filled")
            .expect("stream output ended before its terminal frame");
        assert_eq!(terminal.kind, StreamKind::End);

        router.disconnect().await;
    }

    #[tokio::test]
    async fn connection_stream_state_has_a_hard_upper_bound() {
        let host = Arc::new(ProbeHost {
            dropped: Arc::new(AtomicBool::new(false)),
        });
        let mut router = StreamRouter::new(host);
        for index in 0..MAX_STREAMS_PER_CONNECTION {
            router.route(open(&format!("stream-{index}"))).unwrap();
        }
        let error = router.route(open("one-too-many")).unwrap_err();
        assert!(error.to_string().contains("stream limit"));
        router.disconnect().await;
    }

    #[tokio::test]
    async fn failed_open_ack_does_not_consume_a_stream_slot() {
        let dropped = Arc::new(AtomicBool::new(false));
        let host = Arc::new(ProbeHost {
            dropped: dropped.clone(),
        });
        let mut router = StreamRouter::new(host);
        for index in 0..STREAM_OUTPUT_CAPACITY {
            router
                .output_sender
                .try_send(StreamEnvelope::new(
                    sid(format!("queued-{index}")),
                    StreamKind::Credit,
                ))
                .unwrap();
        }

        let error = router.route(open("unacknowledged")).unwrap_err();
        assert!(error.to_string().contains("queue OPEN acknowledgement"));
        assert!(
            !router.streams.contains_key("unacknowledged"),
            "an OPEN that was never acknowledged consumed a connection stream slot"
        );
        assert!(dropped.load(Ordering::Acquire));

        router.disconnect().await;
    }

    #[tokio::test]
    async fn terminal_streams_are_reaped_and_their_ids_can_be_reused() {
        let host = Arc::new(ProbeHost {
            dropped: Arc::new(AtomicBool::new(false)),
        });
        let mut router = StreamRouter::new(host);

        for index in 0..=MAX_STREAMS_PER_CONNECTION {
            router.route(open("reused")).unwrap();
            assert_eq!(router.recv().await.unwrap().kind, StreamKind::Open);
            let mut cancel = StreamEnvelope::new(sid("reused"), StreamKind::Cancel);
            cancel.payload = Some(json!({"reason": format!("iteration-{index}")}));
            router.route(cancel).unwrap();
            assert_eq!(router.recv().await.unwrap().kind, StreamKind::Cancel);
        }

        router.disconnect().await;
    }

    #[test]
    fn binary_data_round_trip_is_raw_and_exact() {
        let payload: Vec<_> = (0..=u8::MAX).collect();
        let mut frame = StreamEnvelope::new(sid("raw"), StreamKind::Data);
        frame.sequence = Some(42);
        frame.data = Some(payload.clone());

        let encoded = encode_stream_data(&frame).expect("encode DATA");
        assert_eq!(encoded.len(), STREAM_DATA_HEADER_BYTES + 3 + payload.len());
        assert_eq!(decode_stream_data(&encoded).expect("decode DATA"), frame);
        assert_eq!(serde_json::to_value(&frame).unwrap().get("data"), None);
    }

    #[test]
    fn binary_data_encoder_enforces_the_wire_limit_during_encoding() {
        let mut frame = StreamEnvelope::new(sid("raw"), StreamKind::Data);
        frame.sequence = Some(42);
        frame.data = Some(vec![1, 2, 3]);
        let exact_length = STREAM_DATA_HEADER_BYTES + frame.stream_id.len() + 3;

        assert_eq!(
            encode_stream_data_bounded(&frame, exact_length)
                .expect("encode at exact limit")
                .len(),
            exact_length
        );
        let error = encode_stream_data_bounded(&frame, exact_length - 1)
            .expect_err("reject record above limit");
        assert!(error.downcast_ref::<StreamDataLimitExceeded>().is_some());
    }

    #[test]
    fn binary_data_decoder_rejects_adversarial_lengths_and_identifiers() {
        fn wire(stream_len: u16, data_len: u32, body: &[u8]) -> Vec<u8> {
            let mut encoded = Vec::from(STREAM_DATA_MAGIC.as_slice());
            encoded.extend_from_slice(&stream_len.to_be_bytes());
            encoded.extend_from_slice(&1_u64.to_be_bytes());
            encoded.extend_from_slice(&data_len.to_be_bytes());
            encoded.extend_from_slice(body);
            encoded
        }

        let invalid = [
            STREAM_DATA_MAGIC.to_vec(),
            wire(1, 2, b"a"),
            wire(256, 0, &[b'a'; 256]),
            wire(1, 0, &[0xff]),
            wire(1, 0, b"ax"),
        ];
        for encoded in invalid {
            assert!(
                decode_stream_data(&encoded).is_err(),
                "accepted adversarial DATA frame with {} bytes",
                encoded.len()
            );
        }
    }
}
