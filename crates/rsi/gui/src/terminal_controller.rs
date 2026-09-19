//! Receipt and cursor ownership shared by native and Worker GUI bridges.
use crate::application::{Result, error};
use futures_util::future::BoxFuture;
use rsi_session_protocol::terminal::{
    InputState, MAXIMUM_INPUT_BYTES, Operation, OutputPage, PtyError, Reply, Request,
};
use rsi_session_protocol::{Result as SessionResult, SessionError};
pub(super) type Call<'a> =
    dyn Fn(Request) -> BoxFuture<'static, SessionResult<Reply>> + Send + Sync + 'a;
#[derive(Debug)]
pub(super) struct Input {
    pub epoch: Option<u64>,
    pub next: u64,
    pub blocked: bool,
}
impl Input {
    pub async fn write(
        &mut self,
        terminal: &str,
        attachment: &str,
        bytes: Vec<u8>,
        call: &Call<'_>,
        execution: &rsi_meta::Execution,
    ) -> Result<()> {
        if bytes.is_empty() || bytes.len() > MAXIMUM_INPUT_BYTES {
            return Err(format!(
                "Input must contain 1..={MAXIMUM_INPUT_BYTES} bytes"
            ));
        }
        if self.blocked {
            return Err(
                "Input was not confirmed; check the shell before taking control again".into(),
            );
        }
        let epoch = self
            .epoch
            .ok_or("This terminal is read only; take control before typing")?;
        // Cancellation or an uncertain transport result leaves this latched.
        self.blocked = true;
        let mut offset = 0;
        let mut batch = bytes.len();
        while offset < bytes.len() {
            let end = bytes.len().min(offset + batch);
            let sequence = self.next;
            self.next = self
                .next
                .checked_add(1)
                .ok_or("Terminal input sequence exhausted")?;
            let result = call(Request::Operate {
                operation: Operation::Input {
                    terminal: terminal.into(),
                    attachment: attachment.into(),
                    epoch,
                    sequence,
                    bytes: bytes[offset..end].to_vec(),
                },
            })
            .await;
            let mut receipt = match result {
                Ok(Reply::Input(receipt)) => Some(receipt),
                Err(SessionError::Terminal(PtyError::StaleController)) => {
                    self.epoch = None;
                    self.blocked = false;
                    return Err("Another pane took control; take control before typing".into());
                }
                _ => None,
            };
            for _ in 0..40 {
                if receipt
                    .as_ref()
                    .is_some_and(|r| !matches!(r.result, InputState::Pending))
                {
                    break;
                }
                if receipt.is_some() {
                    execution.sleep(std::time::Duration::from_millis(50)).await;
                }
                receipt = match call(Request::Operate {
                    operation: Operation::Receipt {
                        terminal: terminal.into(),
                        epoch,
                        sequence,
                    },
                })
                .await
                .map_err(error)?
                {
                    Reply::Input(value) => Some(value),
                    _ => return Err("Invalid terminal input receipt".into()),
                };
            }
            let receipt = receipt.ok_or("Input receipt is unavailable")?;
            if receipt.epoch != epoch || receipt.sequence != sequence {
                return Err("Terminal input receipt does not match".into());
            }
            let InputState::Accepted { bytes: accepted } = receipt.result else {
                return Err(
                    "Input was not confirmed; check the shell before taking control again".into(),
                );
            };
            if accepted == 0 || accepted > end - offset {
                return Err("Invalid terminal accepted-byte count".into());
            }
            batch = if accepted == end - offset {
                accepted.saturating_mul(2).min(MAXIMUM_INPUT_BYTES)
            } else {
                accepted
            };
            offset += accepted;
        }
        self.blocked = false;
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub(super) struct Page {
    pub ack: String,
    pub value: OutputPage,
}
#[derive(Debug)]
pub(super) struct Output {
    pub epoch: u64,
    pub cursor: u64,
    next: u64,
    last_ack: Option<String>,
    pending: Option<std::sync::Arc<Page>>,
}
impl Output {
    pub fn new(epoch: u64) -> Self {
        Self {
            epoch,
            cursor: 0,
            next: 1,
            last_ack: None,
            pending: None,
        }
    }
    pub async fn read(
        &mut self,
        terminal: &str,
        attachment: &str,
        ack: Option<String>,
        call: &Call<'_>,
        execution: &rsi_meta::Execution,
    ) -> Result<std::sync::Arc<Page>> {
        if let Some(page) = &self.pending
            && ack.as_deref() == Some(&page.ack)
        {
            self.epoch = page.value.stream_epoch;
            self.cursor = page.value.next_cursor;
            self.last_ack.clone_from(&ack);
            self.pending = None;
        } else if ack != self.last_ack {
            return Err("Terminal output acknowledgement is stale".into());
        }
        if let Some(page) = &self.pending {
            return Ok(page.clone());
        }
        let request = Request::Operate {
            operation: Operation::Read {
                terminal: terminal.into(),
                attachment: attachment.into(),
                stream_epoch: self.epoch,
                cursor: self.cursor,
            },
        };
        let reply = rsi_client::read_with_capacity_retry(execution, || call(request.clone()))
            .await
            .map_err(error)?;
        request.validate_reply(&reply).map_err(error)?;
        let Reply::Output(value) = reply else {
            unreachable!("validated output reply")
        };
        let page = std::sync::Arc::new(Page {
            ack: self.next.to_string(),
            value,
        });
        self.next = self
            .next
            .checked_add(1)
            .ok_or("Terminal output ticket exhausted")?;
        self.pending = Some(page.clone());
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_session_protocol::terminal::{InputReceipt, Phase, Size, Terminal};
    use std::sync::{Arc, Mutex};
    #[tokio::test]
    async fn transient_short_write_recovers_batch_throughput_without_replaying_bytes() {
        let observed = Arc::new(Mutex::new((Vec::new(), 0usize, 0usize)));
        let seen = observed.clone();
        let call = move |request| -> BoxFuture<'static, SessionResult<Reply>> {
            let seen = seen.clone();
            Box::pin(async move {
                let Request::Operate {
                    operation:
                        Operation::Input {
                            bytes,
                            epoch,
                            sequence,
                            ..
                        },
                } = request
                else {
                    panic!()
                };
                let mut state = seen.lock().unwrap();
                let accepted = if sequence == 1 { 1 } else { bytes.len() };
                state.0.extend_from_slice(&bytes[..accepted]);
                state.1 += bytes.len();
                state.2 += 1;
                Ok(Reply::Input(InputReceipt {
                    epoch,
                    sequence,
                    result: InputState::Accepted { bytes: accepted },
                }))
            })
        };
        let original: Vec<_> = (0u8..=255).cycle().take(MAXIMUM_INPUT_BYTES).collect();
        let mut input = Input {
            epoch: Some(1),
            next: 1,
            blocked: false,
        };
        input
            .write(
                "terminal",
                "attachment",
                original.clone(),
                &call,
                &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            )
            .await
            .unwrap();
        let (accepted, copied, calls) = &*observed.lock().unwrap();
        assert_eq!(accepted, &original);
        assert!(*copied <= 3 * original.len());
        assert!(
            *calls <= 18,
            "one transient short write needed {calls} calls"
        );
    }

    #[tokio::test]
    async fn repeated_short_writes_copy_at_most_three_times_the_original_input() {
        let copied = Arc::new(Mutex::new(0usize));
        let seen = copied.clone();
        let call = move |request| -> BoxFuture<'static, SessionResult<Reply>> {
            let seen = seen.clone();
            Box::pin(async move {
                let Request::Operate {
                    operation:
                        Operation::Input {
                            bytes,
                            epoch,
                            sequence,
                            ..
                        },
                } = request
                else {
                    panic!()
                };
                *seen.lock().unwrap() += bytes.len();
                Ok(Reply::Input(InputReceipt {
                    epoch,
                    sequence,
                    result: InputState::Accepted { bytes: 1 },
                }))
            })
        };
        let mut input = Input {
            epoch: Some(1),
            next: 1,
            blocked: false,
        };
        input
            .write(
                "terminal",
                "attachment",
                vec![b'x'; 4096],
                &call,
                &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            )
            .await
            .unwrap();
        assert!(*copied.lock().unwrap() <= 3 * 4096);
        assert_eq!(input.next, 4097);
    }
    #[tokio::test]
    async fn partial_utf8_writes_advance_only_confirmed_prefix_and_use_new_sequences() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let received = calls.clone();
        let call = move |request: Request| -> BoxFuture<'static, SessionResult<Reply>> {
            let received = received.clone();
            Box::pin(async move {
                let Request::Operate {
                    operation:
                        Operation::Input {
                            epoch,
                            sequence,
                            bytes,
                            ..
                        },
                } = request
                else {
                    panic!()
                };
                received.lock().unwrap().push((sequence, bytes));
                Ok(Reply::Input(InputReceipt {
                    epoch,
                    sequence,
                    result: InputState::Accepted { bytes: 1 },
                }))
            })
        };
        let mut input = Input {
            epoch: Some(3),
            next: 1,
            blocked: false,
        };
        input
            .write(
                "terminal",
                "attachment",
                "界".as_bytes().to_vec(),
                &call,
                &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            )
            .await
            .unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            vec![(1, vec![231, 149, 140]), (2, vec![149]), (3, vec![140])]
        );
        assert!(!input.blocked);
    }
    #[tokio::test]
    async fn lost_reply_queries_original_receipt_without_replaying_input() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let call = move |request: Request| -> BoxFuture<'static, SessionResult<Reply>> {
            let received = received.clone();
            Box::pin(async move {
                received.lock().unwrap().push(request.clone());
                match request {
                    Request::Operate {
                        operation: Operation::Input { .. },
                    } => Err(SessionError::Backend("lost reply".into())),
                    Request::Operate {
                        operation:
                            Operation::Receipt {
                                epoch, sequence, ..
                            },
                    } => Ok(Reply::Input(InputReceipt {
                        epoch,
                        sequence,
                        result: InputState::Accepted { bytes: 3 },
                    })),
                    _ => panic!(),
                }
            })
        };
        let mut input = Input {
            epoch: Some(2),
            next: 1,
            blocked: false,
        };
        input
            .write(
                "terminal",
                "attachment",
                vec![1, 2, 3],
                &call,
                &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            )
            .await
            .unwrap();
        assert_eq!(requests.lock().unwrap().len(), 2);
        assert!(!input.blocked);
    }
    #[tokio::test]
    async fn unknown_receipt_latches_input_until_explicit_new_controller() {
        let call = |request: Request| -> BoxFuture<'static, SessionResult<Reply>> {
            Box::pin(async move {
                let Request::Operate {
                    operation:
                        Operation::Input {
                            epoch, sequence, ..
                        },
                } = request
                else {
                    panic!()
                };
                Ok(Reply::Input(InputReceipt {
                    epoch,
                    sequence,
                    result: InputState::Unknown,
                }))
            })
        };
        let mut input = Input {
            epoch: Some(2),
            next: 1,
            blocked: false,
        };
        let execution = rsi_meta::Execution::native(tokio::runtime::Handle::current());
        assert!(
            input
                .write("terminal", "attachment", vec![1], &call, &execution)
                .await
                .is_err()
        );
        assert!(input.blocked);
        assert!(
            input
                .write("terminal", "attachment", vec![2], &call, &execution)
                .await
                .is_err()
        );
        assert_eq!(input.next, 2);
    }
    #[tokio::test]
    async fn capacity_retry_and_lost_output_reply_preserve_the_exact_page_and_cursor() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let received = calls.clone();
        let call = move |request: Request| -> BoxFuture<'static, SessionResult<Reply>> {
            let received = received.clone();
            Box::pin(async move {
                let Request::Operate {
                    operation:
                        Operation::Read {
                            terminal,
                            attachment,
                            stream_epoch,
                            cursor,
                        },
                } = request
                else {
                    panic!()
                };
                let first = {
                    let mut received = received.lock().unwrap();
                    received.push(cursor);
                    received.len() == 1
                };
                if first {
                    return Err(SessionError::Capacity);
                }
                Ok(Reply::Output(OutputPage {
                    terminal: Terminal {
                        id: terminal,
                        size: Size {
                            rows: 24,
                            columns: 80,
                        },
                        phase: Phase::Running,
                        controller: None,
                        controller_epoch: 1,
                    },
                    attachment,
                    stream_epoch,
                    reset: false,
                    cursor,
                    next_cursor: cursor + 3,
                    text: "界".into(),
                }))
            })
        };
        let execution = rsi_meta::Execution::native(tokio::runtime::Handle::current());
        let mut output = Output::new(1);
        let first = output
            .read("terminal", "attachment", None, &call, &execution)
            .await
            .unwrap();
        let lost = output
            .read("terminal", "attachment", None, &call, &execution)
            .await
            .unwrap();
        assert_eq!(first.ack, lost.ack);
        assert_eq!(calls.lock().unwrap().len(), 2);
        assert!(Arc::ptr_eq(&first, &lost));
        let second = output
            .read(
                "terminal",
                "attachment",
                Some(first.ack.clone()),
                &call,
                &execution,
            )
            .await
            .unwrap();
        let lost = output
            .read(
                "terminal",
                "attachment",
                Some(first.ack.clone()),
                &call,
                &execution,
            )
            .await
            .unwrap();
        assert_eq!(second.ack, lost.ack);
        assert_eq!(*calls.lock().unwrap(), vec![0, 0, 3]);
        assert!(
            output
                .read(
                    "terminal",
                    "attachment",
                    Some("wrong".into()),
                    &call,
                    &execution
                )
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn stale_controller_is_definitive_and_does_not_query_a_receipt() {
        let call = |request: Request| -> BoxFuture<'static, SessionResult<Reply>> {
            assert!(matches!(
                request,
                Request::Operate {
                    operation: Operation::Input { .. }
                }
            ));
            Box::pin(async { Err(SessionError::Terminal(PtyError::StaleController)) })
        };
        let mut input = Input {
            epoch: Some(3),
            next: 1,
            blocked: false,
        };
        let execution = rsi_meta::Execution::native(tokio::runtime::Handle::current());
        let message = input
            .write("terminal", "attachment", vec![b'x'], &call, &execution)
            .await
            .unwrap_err();
        assert!(message.contains("Another pane took control"));
        assert_eq!(input.epoch, None);
        assert!(!input.blocked);
    }
}
