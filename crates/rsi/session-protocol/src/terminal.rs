//! Authenticated Session operations over generation-owned terminal scopes.
pub use rsi_pty_protocol::{
    Attachment, InputReceipt, InputState, MAXIMUM_INPUT_BYTES, Operation, OutputPage, Phase,
    PtyError, Reply, Size, Terminal,
};
use serde::{Deserialize, Serialize};
/// A Session creates only its frozen-policy Bash; callers cannot supply a program or environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Creates a restricted Bash and its initial writer attachment.
    Create {
        /// Initial character dimensions.
        size: Size,
    },
    /// Operates an already issued live identity.
    Operate {
        /// Finite scope operation.
        operation: Operation,
    },
}

impl Request {
    /// Validates domain framing before dispatch.
    pub fn validate(&self) -> rsi_pty_protocol::Result<()> {
        match self {
            Self::Create { size } => size.validate(),
            Self::Operate { operation } => operation.validate(),
        }
    }
    /// Byte-array input has its own bounded data admission.
    #[must_use]
    pub const fn is_input(&self) -> bool {
        matches!(
            self,
            Self::Operate {
                operation: Operation::Input { .. }
            }
        )
    }
    /// Output polling has independent admission from terminal mutations.
    #[must_use]
    pub const fn is_output(&self) -> bool {
        matches!(
            self,
            Self::Operate {
                operation: Operation::Read { .. }
            }
        )
    }
    /// Validates the exact reply coordinates at an untrusted receiving boundary.
    #[allow(clippy::too_many_lines)] // Keep each closed request variant beside its exact response-coordinate checks.
    pub fn validate_reply(&self, reply: &Reply) -> rsi_pty_protocol::Result<()> {
        reply.validate()?;
        let valid = match (self, reply) {
            (Self::Create { size }, Reply::Attached(value)) => {
                value.terminal.size == *size
                    && value.terminal.controller.as_deref() == Some(&value.id)
                    && matches!(value.terminal.phase, Phase::Running)
            }
            (
                Self::Operate {
                    operation: Operation::Attach { terminal },
                },
                Reply::Attached(value),
            ) => {
                value.terminal.id == *terminal
                    && value.terminal.controller.as_deref() != Some(&value.id)
            }
            (
                Self::Operate {
                    operation:
                        Operation::Read {
                            terminal,
                            attachment,
                            stream_epoch,
                            cursor,
                        },
                },
                Reply::Output(value),
            ) => {
                value.terminal.id == *terminal
                    && value.attachment == *attachment
                    && if value.reset {
                        value.stream_epoch > *stream_epoch && value.cursor == 0
                    } else {
                        value.stream_epoch == *stream_epoch && value.cursor == *cursor
                    }
            }
            (
                Self::Operate {
                    operation:
                        Operation::Input {
                            epoch,
                            sequence,
                            bytes,
                            ..
                        },
                },
                Reply::Input(value),
            ) => {
                value.epoch == *epoch
                    && value.sequence == *sequence
                    && !matches!(value.result,InputState::Accepted{bytes:accepted} if accepted>bytes.len())
            }
            (
                Self::Operate {
                    operation:
                        Operation::Receipt {
                            epoch, sequence, ..
                        },
                },
                Reply::Input(value),
            ) => value.epoch == *epoch && value.sequence == *sequence,
            (
                Self::Operate {
                    operation:
                        Operation::Takeover {
                            terminal,
                            attachment,
                        },
                },
                Reply::Terminal(value),
            ) => value.id == *terminal && value.controller.as_deref() == Some(attachment),
            (
                Self::Operate {
                    operation:
                        Operation::Resize {
                            terminal,
                            attachment,
                            epoch,
                            size,
                        },
                },
                Reply::Terminal(value),
            ) => {
                value.id == *terminal
                    && value.controller.as_deref() == Some(attachment)
                    && value.controller_epoch == *epoch
                    && value.size == *size
            }
            (
                Self::Operate {
                    operation:
                        Operation::Detach { .. } | Operation::Close { .. } | Operation::CloseAll,
                },
                Reply::Done,
            )
            | (
                Self::Operate {
                    operation: Operation::List,
                },
                Reply::List(_),
            ) => true,
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(PtyError::Invalid(
                "terminal reply does not match the request".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal() -> Terminal {
        Terminal {
            id: "pty".into(),
            size: Size {
                rows: 24,
                columns: 80,
            },
            phase: Phase::Running,
            controller: Some("writer".into()),
            controller_epoch: 3,
        }
    }

    #[test]
    fn receiving_boundary_rejects_wrong_output_identity_and_cursor() {
        let request = Request::Operate {
            operation: Operation::Read {
                terminal: "pty".into(),
                attachment: "writer".into(),
                stream_epoch: 2,
                cursor: 7,
            },
        };
        let page = OutputPage {
            terminal: terminal(),
            attachment: "writer".into(),
            stream_epoch: 2,
            reset: false,
            cursor: 7,
            next_cursor: 10,
            text: "界".into(),
        };
        assert!(request.validate_reply(&Reply::Output(page.clone())).is_ok());
        for changed in [
            OutputPage {
                attachment: "other".into(),
                ..page.clone()
            },
            OutputPage {
                cursor: 6,
                next_cursor: 9,
                ..page.clone()
            },
            OutputPage {
                stream_epoch: 3,
                ..page.clone()
            },
            OutputPage {
                next_cursor: 11,
                ..page.clone()
            },
        ] {
            assert!(request.validate_reply(&Reply::Output(changed)).is_err());
        }
        let reset = OutputPage {
            stream_epoch: 3,
            reset: true,
            cursor: 0,
            next_cursor: 3,
            ..page
        };
        assert!(
            request
                .validate_reply(&Reply::Output(reset.clone()))
                .is_ok()
        );
        assert!(
            request
                .validate_reply(&Reply::Output(OutputPage {
                    stream_epoch: 2,
                    ..reset
                }))
                .is_err()
        );
    }

    #[test]
    fn receiving_boundary_rejects_writer_attach_and_mismatched_input_receipt() {
        let attach = Request::Operate {
            operation: Operation::Attach {
                terminal: "pty".into(),
            },
        };
        assert!(
            attach
                .validate_reply(&Reply::Attached(Attachment {
                    terminal: terminal(),
                    id: "writer".into(),
                    stream_epoch: 1
                }))
                .is_err()
        );
        let request = Request::Operate {
            operation: Operation::Input {
                terminal: "pty".into(),
                attachment: "writer".into(),
                epoch: 3,
                sequence: 4,
                bytes: vec![0xe7, 0x95, 0x8c],
            },
        };
        let receipt = InputReceipt {
            epoch: 3,
            sequence: 4,
            result: InputState::Accepted { bytes: 2 },
        };
        assert!(
            request
                .validate_reply(&Reply::Input(receipt.clone()))
                .is_ok()
        );
        for changed in [
            InputReceipt {
                epoch: 2,
                ..receipt.clone()
            },
            InputReceipt {
                sequence: 5,
                ..receipt.clone()
            },
            InputReceipt {
                result: InputState::Accepted { bytes: 4 },
                ..receipt
            },
        ] {
            assert!(request.validate_reply(&Reply::Input(changed)).is_err());
        }
    }
}
