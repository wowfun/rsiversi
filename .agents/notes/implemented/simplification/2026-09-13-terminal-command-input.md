---
name: Fixed terminal input and recoverable application setup
---

## Problem

Modal completion swallowed editing. Setup lacked back navigation and credential
repair. Configurable Enter conflicted with immediate application commands, and
closing write waiters could hide independent save results.

## Decision

Use fixed composer keys and a separate live completion popup. Application grammar
requires single-line input and cursor-at-end, with literal multiline reserved
names and `//`. Session descriptors are refreshed display snapshots; invocation
retains the existing revision-aware controller. Continuation-only commands remain
excluded. Use a short reversible wizard over existing setup operations, querying
credential availability before empty-key reuse. Retain an admitted mutation waiter
in the application after closing: completion reports its receipt without restarting
the closed workflow. The [startup decision](../feature/2026-09-13-terminal-setup.md)
still owns Session absence, discovery and persistence boundaries.

Use terminal-native foreground/background and restrained selection accents,
following the reference clients without importing their theme or window systems.
Keep application forms content-sized so state and recovery actions remain next to
the field. Credential availability controls whether an input is presented; an
unavailable store exposes a read-only retry, preserving credential ownership.

## Alternatives considered

Configurable Enter creates ambiguity when composing multiline commands. A complete
connection form exposes unnecessary fields for official providers. Blocking close
during writes couples navigation to backend latency. Durable exit drafts would
introduce a separate data product contract.

## Consequences

Exit intentionally discards process-local drafts without confirmation; switching
Sessions preserves them. Reopening observes retained writes, while restart reads
configuration rather than replaying operations. Scene v3 requires rebuilding the
native renderer. Secret state never crosses that boundary. Linux deterministic,
visual and opt-in live tests provide separate evidence; native Windows/macOS
coverage is not implied.
