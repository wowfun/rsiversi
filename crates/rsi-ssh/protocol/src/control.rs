//! Closed operations allowed to consume reserved helper transport capacity.

/// A bounded lifecycle operation; identities still require helper-owner validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    /// Terminate and settle the exact process, including its owned descendants.
    Terminate {
        /// Connection-owned process handle.
        process: u64,
    },
    /// Resize an existing terminal; acknowledgement precedes size publication.
    Resize {
        /// Connection-owned terminal process handle.
        process: u64,
        /// Columns bounded by the Process owner's [`rsi_process::PtySize`] contract.
        columns: u16,
        /// Rows bounded by the Process owner's [`rsi_process::PtySize`] contract.
        rows: u16,
    },
    /// Release an already opened Files capability.
    FilesRelease {
        /// Connection-owned Files handle.
        handle: u64,
    },
}

impl Control {
    /// Checks scalar bounds before admission at either byte boundary.
    pub fn validate(self) -> crate::frame::Result<()> {
        let valid = match self {
            Self::Terminate { process } => process != 0,
            Self::Resize {
                process,
                columns,
                rows,
            } => process != 0 && rsi_process::PtySize { columns, rows }.validate().is_ok(),
            Self::FilesRelease { handle } => handle != 0,
        };
        if valid {
            Ok(())
        } else {
            Err(crate::frame::FrameError::Invalid)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reserved_controls_reject_unimplemented_stream_close() {
        assert!(
            serde_json::from_str::<Control>(r#"{"operation":"stream_close","stream":65}"#).is_err()
        );
    }
    #[test]
    fn reserved_resize_admission_matches_native_terminal_dimensions() {
        assert!(
            Control::Resize {
                process: 1,
                columns: 500,
                rows: 200
            }
            .validate()
            .is_ok()
        );
        for (columns, rows) in [(501, 200), (500, 201), (0, 1), (1, 0)] {
            assert_eq!(
                Control::Resize {
                    process: 1,
                    columns,
                    rows
                }
                .validate(),
                Err(crate::frame::FrameError::Invalid)
            );
        }
    }
}
