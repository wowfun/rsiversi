use rsi_meta::CleanupReport;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BoundedDiagnostic {
    pub(super) message: String,
    pub(super) truncated: bool,
}

impl BoundedDiagnostic {
    pub(super) fn from_string(mut message: String, maximum: usize) -> Self {
        let retained = utf8_prefix(&message, maximum);
        let truncated = retained != message.len();
        message.truncate(retained);
        Self { message, truncated }
    }

    pub(super) fn from_display(value: impl fmt::Display, maximum: usize) -> Self {
        struct Writer {
            message: String,
            maximum: usize,
            truncated: bool,
        }

        impl fmt::Write for Writer {
            fn write_str(&mut self, value: &str) -> fmt::Result {
                if self.truncated {
                    return Ok(());
                }
                let remaining = self.maximum.saturating_sub(self.message.len());
                let retained = utf8_prefix(value, remaining);
                self.message.push_str(&value[..retained]);
                self.truncated = retained != value.len();
                Ok(())
            }
        }

        let mut writer = Writer {
            message: String::new(),
            maximum,
            truncated: false,
        };
        fmt::write(&mut writer, format_args!("{value}"))
            .expect("bounded diagnostic formatting cannot fail");
        Self {
            message: writer.message,
            truncated: writer.truncated,
        }
    }
}

fn utf8_prefix(value: &str, maximum: usize) -> usize {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Bounded failure from one scoped mutation.
///
/// The first failure remains authoritative. When initial notification fails,
/// `cleanup` is the joined exact undo and `compensation` separately retains a
/// failed compensating notification, if any.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationError {
    primary: BoundedDiagnostic,
    cleanup: CleanupReport,
    compensation: Option<BoundedDiagnostic>,
}

impl MutationError {
    pub(super) fn new(
        primary: BoundedDiagnostic,
        cleanup: CleanupReport,
        compensation: Option<BoundedDiagnostic>,
    ) -> Self {
        Self {
            primary,
            cleanup,
            compensation,
        }
    }

    pub(super) fn from_primary(value: impl fmt::Display, maximum: usize) -> Self {
        Self::new(
            BoundedDiagnostic::from_display(value, maximum),
            CleanupReport::default(),
            None,
        )
    }

    /// Returns the authoritative bounded error message.
    pub fn primary(&self) -> &str {
        &self.primary.message
    }

    /// Returns whether the authoritative error was truncated.
    pub fn primary_truncated(&self) -> bool {
        self.primary.truncated
    }

    /// Returns the exact joined undo report.
    pub fn cleanup(&self) -> &CleanupReport {
        &self.cleanup
    }

    /// Returns a failed compensating-notification diagnostic, when present.
    pub fn compensation(&self) -> Option<&str> {
        self.compensation
            .as_ref()
            .map(|diagnostic| diagnostic.message.as_str())
    }

    /// Returns whether the compensating error was truncated.
    pub fn compensation_truncated(&self) -> bool {
        self.compensation
            .as_ref()
            .is_some_and(|diagnostic| diagnostic.truncated)
    }
}

impl fmt::Display for MutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.primary.message)
    }
}

impl std::error::Error for MutationError {}
