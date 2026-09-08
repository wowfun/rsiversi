use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// Shared monotonic counters for one HTTP listener generation, with no request data.
#[derive(Clone, Debug, Default)]
pub struct HttpDiagnostics(Arc<Counters>);

#[derive(Debug, Default)]
struct Counters {
    rejected_requests: AtomicU64,
    failed_requests: AtomicU64,
    connection_failures: AtomicU64,
    tls_failures: AtomicU64,
}

/// Point-in-time counts; fields can advance independently during capture.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HttpDiagnosticsSnapshot {
    /// Responses generated with a 4xx status, including typed domain rejection.
    pub rejected_requests: u64,
    /// Responses generated with a 5xx status.
    pub failed_requests: u64,
    /// TCP accept, connection/handshake capacity, HTTP codec, or transport I/O failures.
    pub connection_failures: u64,
    /// Failed or timed-out TLS handshakes, excluding retirement cancellation.
    pub tls_failures: u64,
}

impl HttpDiagnostics {
    /// Captures the current monotonic counters.
    pub fn snapshot(&self) -> HttpDiagnosticsSnapshot {
        HttpDiagnosticsSnapshot {
            rejected_requests: self.0.rejected_requests.load(Ordering::Relaxed),
            failed_requests: self.0.failed_requests.load(Ordering::Relaxed),
            connection_failures: self.0.connection_failures.load(Ordering::Relaxed),
            tls_failures: self.0.tls_failures.load(Ordering::Relaxed),
        }
    }
    pub(crate) fn response(&self, status: http::StatusCode) {
        if status.is_client_error() {
            increment(&self.0.rejected_requests);
        }
        if status.is_server_error() {
            increment(&self.0.failed_requests);
        }
    }
    pub(crate) fn connection_failure(&self) {
        increment(&self.0.connection_failures);
    }
    pub(crate) fn tls_failure(&self) {
        increment(&self.0.tls_failures);
    }
}

impl HttpDiagnosticsSnapshot {
    /// Computes a saturating interval delta without assuming atomic multi-field capture.
    #[must_use]
    pub fn saturating_delta_since(self, earlier: Self) -> Self {
        Self {
            rejected_requests: self
                .rejected_requests
                .saturating_sub(earlier.rejected_requests),
            failed_requests: self.failed_requests.saturating_sub(earlier.failed_requests),
            connection_failures: self
                .connection_failures
                .saturating_sub(earlier.connection_failures),
            tls_failures: self.tls_failures.saturating_sub(earlier.tls_failures),
        }
    }
    /// Whether any rejection or failure was observed.
    pub const fn has_anomaly(self) -> bool {
        self.rejected_requests != 0
            || self.failed_requests != 0
            || self.connection_failures != 0
            || self.tls_failures != 0
    }
}

fn increment(counter: &AtomicU64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(1))
    });
}
