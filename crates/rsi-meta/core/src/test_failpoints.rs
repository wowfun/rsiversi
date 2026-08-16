//! Process-crash gates compiled only into explicit test-failpoint builds.

use std::path::PathBuf;

use serde::Deserialize;

pub(crate) const CRASH_GATE_ENV: &str = "RSI_META_CORE_TEST_CRASH_GATE";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CrashPoint {
    LockPublishedBeforeTerminal,
    PreparedBeforeJournal,
    ManifestReplacedBeforeLock,
    TerminalCommittedBeforePublish,
}

impl CrashPoint {
    const fn as_str(self) -> &'static str {
        match self {
            Self::LockPublishedBeforeTerminal => "lock_published_before_terminal",
            Self::PreparedBeforeJournal => "prepared_before_journal",
            Self::ManifestReplacedBeforeLock => "manifest_replaced_before_lock",
            Self::TerminalCommittedBeforePublish => "terminal_committed_before_publish",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrashGate {
    command_id: String,
    point: String,
    gate_path: PathBuf,
}

#[cfg(unix)]
pub(crate) fn gate(command_id: &str, point: CrashPoint) {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let Some(encoded) = std::env::var_os(CRASH_GATE_ENV) else {
        return;
    };
    let encoded = encoded
        .into_string()
        .expect("RSI_META_CORE_TEST_CRASH_GATE must be UTF-8 JSON");
    let gate: CrashGate = serde_json::from_str(&encoded)
        .expect("RSI_META_CORE_TEST_CRASH_GATE must match the crash gate schema");
    if gate.command_id != command_id || gate.point != point.as_str() {
        return;
    }
    let mut socket =
        UnixStream::connect(&gate.gate_path).expect("connect core process-crash test gate");
    socket
        .write_all(&[1])
        .and_then(|()| socket.flush())
        .expect("notify core process-crash test gate");
    let mut release = [0_u8; 1];
    socket
        .read_exact(&mut release)
        .expect("wait for process-crash test release");
}

#[cfg(not(unix))]
pub(crate) fn gate(_command_id: &str, _point: CrashPoint) {}
