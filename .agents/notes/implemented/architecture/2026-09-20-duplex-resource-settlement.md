---
name: Duplex resource settlement is separate from lossless output completion
---

## Problem

An ACP owner must prove its subprocess and pipe tasks have ended before releasing
resident capacity. Intentional transport shutdown cancels unread output, so the
existing lossless `wait()` reports an I/O failure even when reaping succeeded.
Treating every such error as an unreaped process permanently reserves capacity;
ignoring all errors would conceal a live process group.

## Decision

[Process Duplex](../../../../crates/rsi-process/core/README.md) exposes a distinct
`wait_settlement()` observation. The local owner publishes it only after waiting
for the direct child, the original process group and all pipe tasks. Lossless
`wait()` retains its output-integrity error. ACP propagates resource-settlement
failure independently of the remote prompt or local journal result.

## Alternatives considered

Checking only the exit status misses surviving descendants and pipe owners.
Accepting all `wait()` errors conflates intentional output cancellation with a
failed reap. Repeating cleanup in ACP would duplicate Process ownership and
cannot recover the original process-group identity safely.

## Consequences

Duplex provider implementations must implement both observations. Callers that
need complete output continue using `wait()`. Callers retiring a transport still
request termination before waiting for settlement. Linux tests verify a real PID
is gone despite a retained output cancellation error; a simulated process group
that never disappears still yields `SettlementTimeout`.
