//! The document forwards bytes and acknowledges drawn pages; Rust owns live protocol state.
use super::{Arc, Attachment, GuiApplication, Mutex, Result, error};
use futures_util::future::BoxFuture;
use rsi_session_protocol::terminal::{Operation, Reply, Request, Size, Terminal};
use std::sync::atomic::Ordering;
#[path = "terminal_controller.rs"]
mod controller;
#[derive(Debug)]
pub(super) struct Follower {
    status: Mutex<rsi_session_protocol::terminal::Attachment>,
    input: tokio::sync::Mutex<controller::Input>,
    output: tokio::sync::Mutex<controller::Output>,
}
#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    List,
    Create {
        size: Size,
    },
    Attach {
        terminal: String,
    },
    Read {
        attachment: String,
        ack: Option<String>,
    },
    Write {
        attachment: String,
        bytes: Vec<u8>,
    },
    Takeover {
        attachment: String,
    },
    Resize {
        attachment: String,
        size: Size,
    },
    Detach {
        attachment: String,
    },
    Close {
        terminal: String,
    },
    CloseAll,
}
impl GuiApplication {
    /// Dispatches one bounded terminal document intent for the current pane.
    ///
    /// # Panics
    /// Panics if a previous application panic poisoned pane state.
    pub fn terminal(self: &Arc<Self>, source: &str) -> BoxFuture<'static, Result<String>> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            pane: crate::SurfaceId,
            generation: String,
            request: Command,
        }
        let admitted = (|| {
            if source.len() > 512 * 1024 {
                return Err("Terminal request exceeds its limit".into());
            }
            let input: Input = serde_json::from_str(source).map_err(error)?;
            let attached = self.pane(input.pane)?.attachment(&input.generation)?;
            Ok((input, attached))
        })();
        let (input, attached) = match admitted {
            Ok(value) => value,
            Err(error) => return Box::pin(async { Err(error) }),
        };
        let slots = if matches!(input.request, Command::Read { .. }) {
            &self.terminal_reads
        } else if matches!(input.request, Command::Write { .. }) {
            &self.terminal_writes
        } else {
            &self.slots
        };
        match self.try_admit_inner(false, None, false, slots, move |app| async move {
            attached
                .dispatch_terminal(input.request, &app.execution)
                .await
        }) {
            Ok(waiter) => waiter,
            Err(error) => Box::pin(async { Err(error) }),
        }
    }
}
impl Follower {
    fn update(&self, status: &Terminal) {
        let mut value = self.status.lock().expect("terminal status poisoned");
        if status.controller_epoch >= value.terminal.controller_epoch {
            value.terminal = status.clone();
        }
    }
}
fn detach(value: &rsi_session_protocol::terminal::Attachment) -> Request {
    Request::Operate {
        operation: Operation::Detach {
            terminal: value.terminal.id.clone(),
            attachment: value.id.clone(),
            epoch: value.terminal.controller_epoch,
        },
    }
}
impl Attachment {
    pub(super) async fn detach_terminals(&self) {
        self.terminal_closed.store(true, Ordering::Release);
        let followers = std::mem::take(
            &mut *self
                .terminal_followers
                .lock()
                .expect("terminal followers poisoned"),
        );
        for follower in followers.values() {
            let value = follower
                .status
                .lock()
                .expect("terminal status poisoned")
                .clone();
            let _ = self.handle.terminal(detach(&value)).await;
        }
    }
}

impl Attachment {
    #[allow(clippy::too_many_lines)] // One closed document dispatch binds each intent to this pane's retained follower.
    async fn dispatch_terminal(
        &self,
        command: Command,
        execution: &rsi_meta::Execution,
    ) -> Result<String> {
        if self.terminal_closed.load(Ordering::Acquire) {
            return Err("Terminal pane is closed".into());
        }
        let handle = self.handle.clone();
        let call = move |request| -> BoxFuture<'static, rsi_session_protocol::Result<Reply>> {
            let handle = handle.clone();
            Box::pin(async move { handle.terminal(request).await })
        };
        let reply = match command {
            Command::List => call(Request::Operate {
                operation: Operation::List,
            })
            .await
            .map_err(error)?,
            Command::Create { size } => call(Request::Create { size }).await.map_err(error)?,
            Command::Attach { terminal } => call(Request::Operate {
                operation: Operation::Attach { terminal },
            })
            .await
            .map_err(error)?,
            Command::Close { terminal } => {
                let reply = call(Request::Operate {
                    operation: Operation::Close {
                        terminal: terminal.clone(),
                    },
                })
                .await
                .map_err(error)?;
                self.terminal_followers
                    .lock()
                    .expect("terminal followers poisoned")
                    .retain(|_, f| {
                        f.status
                            .lock()
                            .expect("terminal status poisoned")
                            .terminal
                            .id
                            != terminal
                    });
                reply
            }
            Command::CloseAll => {
                let reply = call(Request::Operate {
                    operation: Operation::CloseAll,
                })
                .await
                .map_err(error)?;
                self.terminal_followers
                    .lock()
                    .expect("terminal followers poisoned")
                    .clear();
                reply
            }
            command => {
                let (Command::Read { attachment: id, .. }
                | Command::Write { attachment: id, .. }
                | Command::Takeover { attachment: id }
                | Command::Resize { attachment: id, .. }
                | Command::Detach { attachment: id }) = &command
                else {
                    unreachable!()
                };
                let follower = self
                    .terminal_followers
                    .lock()
                    .expect("terminal followers poisoned")
                    .get(id)
                    .cloned()
                    .ok_or("Terminal attachment does not belong to this pane")?;
                let value = follower
                    .status
                    .lock()
                    .expect("terminal status poisoned")
                    .clone();
                let terminal = value.terminal.id.clone();
                match command {
                    Command::Read { ack, .. } => {
                        let page = follower
                            .output
                            .try_lock()
                            .map_err(|_| "Terminal output read is busy")?
                            .read(&terminal, &value.id, ack, &call, execution)
                            .await
                            .map_err(error)?;
                        follower.update(&page.value.terminal);
                        return serde_json::to_string(
                            &serde_json::json!({"type":"output","value":page.value,"ack":page.ack}),
                        )
                        .map_err(error);
                    }
                    Command::Write { bytes, .. } => {
                        follower
                            .input
                            .try_lock()
                            .map_err(|_| "Terminal input is busy")?
                            .write(&terminal, &value.id, bytes, &call, execution)
                            .await
                            .map_err(error)?;
                        Reply::Done
                    }
                    Command::Takeover { .. } => {
                        let mut writer = follower
                            .input
                            .try_lock()
                            .map_err(|_| "Terminal input is busy")?;
                        let reply = call(Request::Operate {
                            operation: Operation::Takeover {
                                terminal,
                                attachment: value.id,
                            },
                        })
                        .await
                        .map_err(error)?;
                        if let Reply::Terminal(status) = &reply {
                            writer.epoch = Some(status.controller_epoch);
                            writer.next = 1;
                            writer.blocked = false;
                            follower.update(status);
                        }
                        reply
                    }
                    Command::Resize { size, .. } => {
                        let reply = call(Request::Operate {
                            operation: Operation::Resize {
                                terminal,
                                attachment: value.id,
                                epoch: value.terminal.controller_epoch,
                                size,
                            },
                        })
                        .await
                        .map_err(error)?;
                        if let Reply::Terminal(status) = &reply {
                            follower.update(status);
                        }
                        reply
                    }
                    Command::Detach { .. } => {
                        let reply = call(detach(&value)).await;
                        self.terminal_followers
                            .lock()
                            .expect("terminal followers poisoned")
                            .remove(&value.id);
                        reply.map_err(error)?
                    }
                    _ => unreachable!(),
                }
            }
        };
        if let Reply::Attached(value) = &reply {
            let closed = {
                let mut followers = self
                    .terminal_followers
                    .lock()
                    .expect("terminal followers poisoned");
                if self.terminal_closed.load(Ordering::Acquire) {
                    true
                } else {
                    followers.insert(
                        value.id.clone(),
                        Arc::new(Follower {
                            status: Mutex::new(value.clone()),
                            input: tokio::sync::Mutex::new(controller::Input {
                                epoch: (value.terminal.controller.as_deref() == Some(&value.id))
                                    .then_some(value.terminal.controller_epoch),
                                next: 1,
                                blocked: false,
                            }),
                            output: tokio::sync::Mutex::new(controller::Output::new(
                                value.stream_epoch,
                            )),
                        }),
                    );
                    false
                }
            };
            if closed {
                let _ = call(detach(value)).await;
                return Err("Terminal pane closed while attaching".into());
            }
        }
        serde_json::to_string(&reply).map_err(error)
    }
}
