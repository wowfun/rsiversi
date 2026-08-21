# rsi-agent testing and conformance

Tests observe public behavior through `AgentHost`, typed protocol messages, JSON schemas, `rsi-meta` service streams, and process-owned files. Private coordination, session tasks, the SQLite writer, cold-read jobs, and adapter types are not compatibility surfaces. Persistent behavior tests use real temporary SQLite workspaces rather than a mock persistence trait.

## Evidence

| Suite | Owned evidence |
|---|---|
| Loop behavior | Direct final output, single and multiple serial calls, tool application failures, strict step/call limits, malformed model output, and terminal closure |
| Admission and concurrency | Unrelated-session overlap, active and admitted bounds, exact model-and-prompt join/conflict, probing-time request grouping, caller-future drop without cancellation, and per-session completion waits |
| Transcript invariants | Gap-free sequence numbers, strict turn/step nesting, one terminal result per call, logged-before-visible content, and byte-exact request reconstruction |
| Persistence and replay | Schema-v7/no-migration rejection, compare-and-set counters, one-query bounded reads, 128-row recovery paging and range-query plans, canonical workspace pinning across intermediate-link changes, actual-handle/path replacement rejection, reopen stability, same-request idempotence, exclusive workspace admission, and lease lifetime across cold reads |
| Direct AI operations | Caller-owned identity reservation before provider work, durable Reserved/Prepared/Started/terminal barriers, bounded tombstone eviction and reuse, NotStarted/OutcomeUnknown recovery, supervisor and handshake deadlines, five service bindings, CAS deduplication and corruption detection, and live-only Realtime frames |
| Recovery | Failpoints after request commit, call preparation, dispatch-before-result, and final-assistant-before-terminal; interrupted repair without model or tool re-execution |
| Failure isolation | Selected-session corruption and read-only busy/locked errors leave unrelated sessions healthy; store identity, schema, commit uncertainty, and worker loss poison the host, stop other active work at its next barrier, and recover without effect re-execution |
| Transaction and projection budgets | Five direct-final commits, nine one-tool commits (both including session creation), receipt-before-state installation, and exactly two model-request derivations |
| Protocol | Closed schema agreement, duplicate-key rejection, lossless envelope-number admission, dependency-feature-stable JSON semantics, canonical tool-argument machine numbers, exact wire-version classification, request correlation, identifier validation, and envelope/catalog/result bounds |
| Assembled conformance | Real native five-capability AI and tool providers routed through `CompositionHost` on Linux x86_64 and macOS arm64 |

Concurrency uses explicit gates and channels rather than sleep-based ordering. Current-thread runtime tests gate SQLite work to prove timers keep advancing, and cold-read fan-out tests prove one slow session does not serialize another. Provider failure adapters remain crate-private and only replace nondeterminism; durable tests retain the production SQLite implementation.

## Keyless assembled scenario

`cargo xtask rsi-agent conformance` checks one standalone fixture workspace through its six-stage gate and runs the credential-free native scenario. The fixture [README](../../../fixtures/rsi-agent/README.md) owns the exact commands, deterministic concurrency barrier, observations, and replay assertions; this product document intentionally does not duplicate that script.

Only Linux x86_64 and macOS arm64 CI together support the two-platform native conformance claim. A local run reports evidence only for its actual host.

## Choosing verification

| Changed surface | Required verification |
|---|---|
| Core API, loop, transcript, direct AI operation, artifact, or replay | Focused `rsi-agent` tests plus `cargo xtask rsi-agent conformance` for public behavior |
| Persistence or recovery | Core tests with the failpoint feature plus the assembled conformance scenario |
| Protocol DTO, canonical encoding, or schema | Protocol tests, schema-contract tests, and assembled conformance |
| One native fixture | The fixture workspace's locked format, Clippy, and tests; use full conformance for a shared contract change |
| Product docs, rustdoc, or instructions | `cargo xtask verify-docs`, the workspace library rustdoc build with warnings denied, and workspace doctests |
| CI, toolchain, product-wide validation, or a release claim | Root workspace checks and all product conformance commands |

Root `--workspace` commands do not cover the standalone fixture workspace.
