//! Shared inputs for published-package conformance checks.

use serde_json::Value;

/// One published fixture package and its stable, non-secret prepare input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishedPackage {
    pub relative_path: &'static str,
    representative_config_json: &'static str,
    pub expected_audit_sha256: &'static str,
}

impl PublishedPackage {
    /// Representative instance configuration covered by package conformance.
    ///
    /// # Panics
    ///
    /// Panics if a compile-time fixture literal is not valid JSON.
    pub fn representative_config(self) -> Value {
        serde_json::from_str(self.representative_config_json)
            .expect("published package config literals are valid JSON")
    }
}

/// All trusted cdylib packages shipped as runtime or conformance fixtures.
pub const PUBLISHED_PACKAGES: &[PublishedPackage] = &[
    PublishedPackage {
        relative_path: "plugins/rsi-meta/fs-watch-native",
        representative_config_json: "{}",
        expected_audit_sha256: "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
    },
    PublishedPackage {
        relative_path: "plugins/rsi-meta/fs-watch-polling",
        representative_config_json: "{}",
        expected_audit_sha256: "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
    },
    PublishedPackage {
        relative_path: "plugins/rsi-meta/hmr-consumer",
        representative_config_json: r#"{
            "manifest_path": "/workspace/rsi-meta.toml",
            "lock_path": "/workspace/rsi-meta.lock",
            "watch_request_id": "hmr-conformance"
        }"#,
        expected_audit_sha256: "2278a41f8ec00bc1a0a943e201bc450a22d35f4e9854a3c72d43fe670bb694c2",
    },
    PublishedPackage {
        relative_path: "fixtures/rsi-meta/echo-bidi",
        representative_config_json: "{}",
        expected_audit_sha256: "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
    },
    PublishedPackage {
        relative_path: "fixtures/rsi-meta/nested-scope-consumer",
        representative_config_json: r#"{
            "message": "nearest-provider",
            "request_id": "nested-conformance"
        }"#,
        expected_audit_sha256: "3ae44ce0d44fc3b8de7aec5511f8f4a3ecd28f26272b268f0b0f6f5ca6f9cbab",
    },
    PublishedPackage {
        relative_path: "fixtures/rsi-meta/cas-counter",
        representative_config_json: "{}",
        expected_audit_sha256: "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
    },
    PublishedPackage {
        relative_path: "fixtures/rsi-meta/lifecycle-probe",
        representative_config_json: r#"{
            "fail_prepare": false,
            "retire_mode": "ack",
            "tag": "conformance"
        }"#,
        expected_audit_sha256: "0b592c33a9f365e8750e9819bffd653d06ae2b1a71655a463907cdcd97a7a1e0",
    },
];
