use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic diagnostic counters for one Session Host server generation.
#[derive(Clone, Debug, Default)]
pub struct SessionHostDiagnostics {
    inner: Arc<SessionHostDiagnosticsInner>,
}

#[derive(Debug, Default)]
struct SessionHostDiagnosticsInner {
    accepted_connections: AtomicU64,
    accept_errors: AtomicU64,
    peer_credential_errors: AtomicU64,
    foreign_uid_rejections: AtomicU64,
    capacity_rejections: AtomicU64,
    handshake_rejections: AtomicU64,
    handshake_failures: AtomicU64,
    request_failures: AtomicU64,
    response_failures: AtomicU64,
    connection_task_panics: AtomicU64,
    drain_aborted_connections: AtomicU64,
}

/// One point-in-time copy of the Session Host diagnostic counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionHostDiagnosticsSnapshot {
    /// Connections accepted by the Unix listener.
    pub accepted_connections: u64,
    /// Transient listener accept failures.
    pub accept_errors: u64,
    /// Connections whose peer credentials could not be read.
    pub peer_credential_errors: u64,
    /// Connections rejected because their effective user differed from the Host.
    pub foreign_uid_rejections: u64,
    /// Connections rejected by the bounded connection admission.
    pub capacity_rejections: u64,
    /// Well-formed handshakes rejected for incompatible identity.
    pub handshake_rejections: u64,
    /// Malformed, incomplete, or unwritable handshakes.
    pub handshake_failures: u64,
    /// Malformed, incomplete, or timed-out request frames.
    pub request_failures: u64,
    /// Response framing or write failures after a valid request.
    pub response_failures: u64,
    /// Connection tasks that panicked.
    pub connection_task_panics: u64,
    /// Admitted connections aborted when the graceful drain deadline elapsed.
    pub drain_aborted_connections: u64,
}

impl SessionHostDiagnostics {
    /// Captures a consistent-enough monotonic snapshot for operational reporting.
    pub fn snapshot(&self) -> SessionHostDiagnosticsSnapshot {
        SessionHostDiagnosticsSnapshot {
            accepted_connections: load(&self.inner.accepted_connections),
            accept_errors: load(&self.inner.accept_errors),
            peer_credential_errors: load(&self.inner.peer_credential_errors),
            foreign_uid_rejections: load(&self.inner.foreign_uid_rejections),
            capacity_rejections: load(&self.inner.capacity_rejections),
            handshake_rejections: load(&self.inner.handshake_rejections),
            handshake_failures: load(&self.inner.handshake_failures),
            request_failures: load(&self.inner.request_failures),
            response_failures: load(&self.inner.response_failures),
            connection_task_panics: load(&self.inner.connection_task_panics),
            drain_aborted_connections: load(&self.inner.drain_aborted_connections),
        }
    }
}

#[cfg(any(unix, test))]
impl SessionHostDiagnostics {
    pub(crate) fn accepted_connection(&self) {
        increment(&self.inner.accepted_connections, 1);
    }

    pub(crate) fn accept_error(&self) {
        increment(&self.inner.accept_errors, 1);
    }

    pub(crate) fn peer_credential_error(&self) {
        increment(&self.inner.peer_credential_errors, 1);
    }

    pub(crate) fn foreign_uid_rejection(&self) {
        increment(&self.inner.foreign_uid_rejections, 1);
    }

    pub(crate) fn capacity_rejection(&self) {
        increment(&self.inner.capacity_rejections, 1);
    }

    pub(crate) fn handshake_rejection(&self) {
        increment(&self.inner.handshake_rejections, 1);
    }

    pub(crate) fn handshake_failure(&self) {
        increment(&self.inner.handshake_failures, 1);
    }

    pub(crate) fn request_failure(&self) {
        increment(&self.inner.request_failures, 1);
    }

    pub(crate) fn response_failure(&self) {
        increment(&self.inner.response_failures, 1);
    }

    pub(crate) fn connection_task_panic(&self) {
        increment(&self.inner.connection_task_panics, 1);
    }

    pub(crate) fn drain_aborted_connections(&self, count: usize) {
        increment(
            &self.inner.drain_aborted_connections,
            u64::try_from(count).unwrap_or(u64::MAX),
        );
    }
}

impl SessionHostDiagnosticsSnapshot {
    /// Returns the saturating per-counter difference from an earlier snapshot.
    #[must_use]
    pub fn saturating_delta_since(self, earlier: Self) -> Self {
        Self {
            accepted_connections: self
                .accepted_connections
                .saturating_sub(earlier.accepted_connections),
            accept_errors: self.accept_errors.saturating_sub(earlier.accept_errors),
            peer_credential_errors: self
                .peer_credential_errors
                .saturating_sub(earlier.peer_credential_errors),
            foreign_uid_rejections: self
                .foreign_uid_rejections
                .saturating_sub(earlier.foreign_uid_rejections),
            capacity_rejections: self
                .capacity_rejections
                .saturating_sub(earlier.capacity_rejections),
            handshake_rejections: self
                .handshake_rejections
                .saturating_sub(earlier.handshake_rejections),
            handshake_failures: self
                .handshake_failures
                .saturating_sub(earlier.handshake_failures),
            request_failures: self
                .request_failures
                .saturating_sub(earlier.request_failures),
            response_failures: self
                .response_failures
                .saturating_sub(earlier.response_failures),
            connection_task_panics: self
                .connection_task_panics
                .saturating_sub(earlier.connection_task_panics),
            drain_aborted_connections: self
                .drain_aborted_connections
                .saturating_sub(earlier.drain_aborted_connections),
        }
    }

    /// Reports whether this interval contains anything other than successful accepts.
    #[must_use]
    pub const fn has_anomaly(self) -> bool {
        self.accept_errors != 0
            || self.peer_credential_errors != 0
            || self.foreign_uid_rejections != 0
            || self.capacity_rejections != 0
            || self.handshake_rejections != 0
            || self.handshake_failures != 0
            || self.request_failures != 0
            || self.response_failures != 0
            || self.connection_task_panics != 0
            || self.drain_aborted_connections != 0
    }
}

fn load(counter: &AtomicU64) -> u64 {
    counter.load(Ordering::Relaxed)
}

#[cfg(any(unix, test))]
fn increment(counter: &AtomicU64, amount: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(amount))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_and_delta_preserve_named_monotonic_counters() {
        let diagnostics = SessionHostDiagnostics::default();
        diagnostics.accepted_connection();
        diagnostics.handshake_rejection();
        let before = diagnostics.snapshot();
        diagnostics.accepted_connection();
        diagnostics.request_failure();
        diagnostics.drain_aborted_connections(2);

        assert_eq!(
            diagnostics.snapshot().saturating_delta_since(before),
            SessionHostDiagnosticsSnapshot {
                accepted_connections: 1,
                request_failures: 1,
                drain_aborted_connections: 2,
                ..SessionHostDiagnosticsSnapshot::default()
            }
        );
        assert!(before.has_anomaly());
        let healthy = SessionHostDiagnostics::default();
        healthy.accepted_connection();
        assert!(!healthy.snapshot().has_anomaly());
    }

    #[test]
    fn every_failure_class_is_publicly_observable_and_saturating() {
        let diagnostics = SessionHostDiagnostics::default();
        diagnostics.accept_error();
        diagnostics.peer_credential_error();
        diagnostics.foreign_uid_rejection();
        diagnostics.capacity_rejection();
        diagnostics.handshake_failure();
        diagnostics.response_failure();
        diagnostics.connection_task_panic();
        diagnostics.drain_aborted_connections(3);
        diagnostics
            .inner
            .accepted_connections
            .store(u64::MAX, Ordering::Relaxed);
        diagnostics.accepted_connection();

        assert_eq!(
            diagnostics.snapshot(),
            SessionHostDiagnosticsSnapshot {
                accepted_connections: u64::MAX,
                accept_errors: 1,
                peer_credential_errors: 1,
                foreign_uid_rejections: 1,
                capacity_rejections: 1,
                handshake_failures: 1,
                response_failures: 1,
                connection_task_panics: 1,
                drain_aborted_connections: 3,
                ..SessionHostDiagnosticsSnapshot::default()
            }
        );
    }
}
