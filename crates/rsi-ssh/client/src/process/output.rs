use super::{Arc, ProcessConnection, ProcessError, ReceiveStream, Result, invalid};
use rsi_process::{DuplexRead, ProcessOutput, ProcessRead};
use std::{collections::VecDeque, sync::Mutex};
use tokio::sync::watch;
fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
#[derive(Debug)]
struct Capture {
    bytes: VecDeque<u8>,
    next: u64,
    error: Option<ProcessError>,
}
#[derive(Debug)]
pub(super) struct Tail {
    capture: Mutex<Capture>,
    maximum: usize,
    done: watch::Sender<Option<Result<()>>>,
}
impl Tail {
    pub fn spawn(mut input: ReceiveStream, maximum: usize, client: ProcessConnection) -> Arc<Self> {
        let (done, _) = watch::channel(None);
        let tail = Arc::new(Self {
            capture: Mutex::new(Capture {
                bytes: VecDeque::new(),
                next: 0,
                error: None,
            }),
            maximum,
            done,
        });
        let retained = tail.clone();
        tokio::spawn(async move {
            let result = async {
                loop {
                    let chunk = input
                        .next()
                        .await
                        .map_err(|_| ProcessError::OutcomeUnknown)?
                        .ok_or(ProcessError::OutcomeUnknown)?;
                    if chunk.bytes == [1] && chunk.eof {
                        return Err(ProcessError::Io("target captured stream failed".into()));
                    }
                    let (offset, bytes) = rsi_ssh_protocol::rpc::decode_tail(&chunk.bytes)
                        .map_err(|_| client.malformed())?;
                    {
                        let mut capture = lock(&retained.capture);
                        if offset < capture.next {
                            return Err(client.malformed());
                        }
                        if offset > capture.next {
                            capture.bytes.clear();
                        }
                        if bytes.len() >= retained.maximum {
                            capture.bytes.clear();
                            capture
                                .bytes
                                .extend(&bytes[bytes.len() - retained.maximum..]);
                        } else {
                            let excess = capture
                                .bytes
                                .len()
                                .saturating_add(bytes.len())
                                .saturating_sub(retained.maximum);
                            capture.bytes.drain(..excess);
                            capture.bytes.extend(bytes);
                        }
                        capture.next = offset + bytes.len() as u64;
                    }
                    if chunk.eof {
                        return Ok(());
                    }
                }
            }
            .await;
            if let Err(error) = &result {
                lock(&retained.capture).error = Some(error.clone());
            }
            retained.done.send_replace(Some(result));
        });
        tail
    }
    pub async fn finished(&self) -> Result<()> {
        let mut done = self.done.subscribe();
        loop {
            if let Some(result) = done.borrow().clone() {
                return result;
            }
            done.changed()
                .await
                .map_err(|_| ProcessError::OutcomeUnknown)?;
        }
    }
}
impl ProcessOutput for Tail {
    fn read_from(&self, offset: u64) -> Result<ProcessRead> {
        let capture = lock(&self.capture);
        if let Some(error) = &capture.error {
            return Err(error.clone());
        }
        let oldest = capture.next - capture.bytes.len() as u64;
        if offset > capture.next {
            return Err(invalid());
        }
        let skip = usize::try_from(offset.saturating_sub(oldest)).map_err(|_| invalid())?;
        Ok(ProcessRead {
            bytes: capture.bytes.iter().skip(skip).copied().collect(),
            oldest_offset: oldest,
            next_offset: capture.next,
            lossy: offset < oldest,
            full_output: None,
        })
    }
    fn peek_tail(&self, maximum: usize) -> Result<ProcessRead> {
        if !(1..=32768).contains(&maximum) {
            return Err(invalid());
        }
        let capture = lock(&self.capture);
        if let Some(error) = &capture.error {
            return Err(error.clone());
        }
        let length = capture.bytes.len().min(maximum);
        let oldest = capture.next - length as u64;
        Ok(ProcessRead {
            bytes: capture
                .bytes
                .iter()
                .skip(capture.bytes.len().saturating_sub(maximum))
                .copied()
                .collect(),
            oldest_offset: oldest,
            next_offset: capture.next,
            lossy: oldest != 0,
            full_output: None,
        })
    }
}
#[derive(Debug)]
pub(super) struct Lossless {
    inner: tokio::sync::Mutex<LosslessState>,
    client: ProcessConnection,
}
#[derive(Debug)]
struct LosslessState {
    input: ReceiveStream,
    pending: VecDeque<u8>,
    eof: bool,
    error: Option<ProcessError>,
}
impl Lossless {
    pub fn new(input: ReceiveStream, client: ProcessConnection) -> Arc<Self> {
        Arc::new(Self {
            inner: tokio::sync::Mutex::new(LosslessState {
                input,
                pending: VecDeque::new(),
                eof: false,
                error: None,
            }),
            client,
        })
    }
    pub async fn read(&self, maximum: usize) -> Result<DuplexRead> {
        if !(1..=rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES).contains(&maximum) {
            return Err(invalid());
        }
        let mut inner = self.inner.try_lock().map_err(|_| ProcessError::Capacity)?;
        if let Some(error) = &inner.error {
            return Err(error.clone());
        }
        if inner.pending.is_empty() && !inner.eof {
            let Ok(Some(chunk)) = inner.input.next().await else {
                inner.error = Some(ProcessError::OutcomeUnknown);
                return Err(ProcessError::OutcomeUnknown);
            };
            match chunk.bytes.split_first() {
                Some((&0, bytes)) if !bytes.is_empty() || chunk.eof => {
                    inner.pending.extend(bytes);
                    inner.eof = chunk.eof;
                }
                Some((&1, bytes)) if bytes.is_empty() && chunk.eof => {
                    let error = ProcessError::Io("target byte stream failed".into());
                    inner.error = Some(error.clone());
                    return Err(error);
                }
                _ => {
                    let error = self.client.malformed();
                    inner.error = Some(error.clone());
                    return Err(error);
                }
            }
        }
        let length = inner.pending.len().min(maximum);
        let bytes = inner.pending.drain(..length).collect();
        Ok(DuplexRead {
            bytes,
            eof: inner.eof && inner.pending.is_empty(),
        })
    }
}
