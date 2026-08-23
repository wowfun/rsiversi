---
name: Runtime-owned plugin foundation
comment: Minimal Context and Fiber core with native loading as an ordinary plugin
---

## Problem

The former `rsi-meta` surface combined composition files, a daemon protocol,
persistence, recovery, native framing, and product integrations into one
platform. Those layers duplicated lifecycle authority and made ordinary plugin
behavior depend on orchestration machinery. The repository also retained Agent
and AI integration claims after their host contracts had been removed.

The foundation needs one ownership model for setup, dependency convergence,
calls, events, retirement, and shutdown. Cancellation or a hostile trusted
native callback must not let a caller-owned future strand shared lifecycle
state or permit overlapping access to foreign mutable state.

## Decision

The active model is `Runtime -> Context -> Fiber`. Core owns bounded registries,
exact generation and contract fencing, dependency convergence, application
reconciliation, callback admission, listener authority, effects, and joinable
shutdown. Long-lived lifecycle work belongs to Runtime-owned tasks, so dropping
the initiating future does not abandon published state. Cleanup waits for
admitted safe-Rust work instead of freeing resources after a deadline. A
provider gate stores closure and the admitted-callback count in one atomic
state; separate atomics would not give retirement and ordinary concurrent
admission a shared linearization point. Cleanup-time calls from retiring
dependents remain ordered by the convergence transaction that provider
retirement joins before draining.

Application ownership transfers only after the initiating future acknowledges
the returned handle; cancellation before that acknowledgement makes the
Runtime-owned task dispose the Fiber. Terminal state is also a publication
fence: an activation that finishes after terminalization is rolled back rather
than becoming Active.

Everything outside that kernel is a plugin. Native discovery and execution are
implemented by the ordinary Loader plugin over `PluginFactory`; core has no
native or product-specific vocabulary. The v1 C ABI is a small synchronous,
byte-oriented call boundary with explicit ownership and panic containment.
Minor-version extension appends fields: compatibility is decided from the
minimum prefix for the table's declared minor rather than the newest host table
size. The loader zero-initializes plugin output storage before entry, pins the
verified artifact identity, serializes each foreign instance, and uses one
atomic deadline adjudication for each callback. Completion published while the
callback gate is held and timeout are mutually exclusive; a timeout
terminalizes admission because in-process native code cannot be forcibly
stopped safely. The independently owned watchdog survives cancellation of its
adapter future but retires as soon as the callback publishes completion.

Terminalization covers the complete Runtime. A trusted in-process callback
shares memory and may have damaged global invariants, so fencing only its module
would falsely imply a fault-isolation boundary. Dedicated foreign threads keep
hung callbacks off Tokio's shared blocking pool, while Runtime and Loader
admission limits bound their normal in-flight populations; direct catalog users
must supply their own concurrency admission.

Initial native configuration crosses core's opaque prepared-application seam,
so transformation happens exactly once before any child is published. The
active `rsi-ai` product remains standalone, and the active `rsi-agent` product
surface is its protocol. The superseded runtime decisions are retained only in
the archived `2026-08-18-replayable-agent-turn-runtime.md` and
`2026-08-21-live-agent-and-coding-tools.md` notes. This decision also supersedes
the native composition and durable Agent integration portions of the archived
`2026-08-19-five-capability-ai-boundary.md` note while retaining its
provider-neutral standalone SDK decision.

## Alternatives considered

Keeping the composition daemon and adapting the new core beneath it was
rejected because compatibility would preserve two lifecycle authorities.
Embedding native loading, AI routing, or Agent durability in core was rejected
because each adds policy and failure modes that an ordinary plugin can own.

Emulating the removed stream/lifecycle ABI for the old coding-tools provider
was rejected. The synchronous v1 call boundary does not define durable session
identity, provider-to-host notifications, or fallible asynchronous teardown.
A compatibility shim would conceal those missing contracts rather than provide
the foundation needed to prove them.

Timing out cleanup and freeing a native generation anyway was rejected because
it can unload code or destroy data still used by a foreign thread. Forceful
preemption requires a future process or Wasm adapter, not unsafe in-process
teardown.

## Consequences

Pre-release callers receive no compatibility promise for the removed daemon,
manifests, schemas, wrappers, or frame protocol. Future AI and Agent milestones
must define their bounded plugin wires over the current public foundation and
re-establish end-to-end evidence. Obsolete Agent runtime and coding-tools
implementations are absent from the working tree so they cannot masquerade as
current architecture; archived decisions and Git history retain their
rationale.

The in-process native adapter can bound waiting and fence new work but cannot
reclaim a permanently hung foreign thread. It deliberately retains the live
instance and mapping until that callback returns. Native loader evidence is
host-specific; portable ABI checks do not imply Windows or macOS execution.
