---
name: Bounded ACP interoperability with native and external session ownership
---

## Problem

ACP endpoints exchange untrusted process input and cancellation evidence.
The schema's permissive MCP collection decoding can silently discard malformed
declarations. A native durable Turn terminal can also precede retained Tool
settlement, so using it as cancellation completion would overstate cleanup.

## Decision

The protocol pins stable schema DTOs and validates bounded raw JSON before
conversion. An independent driver owns correlation, byte admission and write drain over
Process-owned pipes. Native server sessions use the existing Kernel and Session
owner; external client sessions retain a separate observed journal. Host owns
peers across UI detach. Executor publishes an exact-claim controlled-work
observation through Kernel, separately from durable outcome; its proof is required
before successful cancellation completion.

Host endpoints may also select ordered stable ACP configuration values before a
client becomes Ready. The latest advertised choices and every acknowledgement
must agree, including earlier selections after a later option changes. This keeps
model/effort startup policy with the operator without adding arbitrary remote
configuration or command authority to model Tools and detached UI controllers.

## Alternatives considered

The full SDK transport would introduce independent queues and runtime ownership.
Reusing TurnTerminal as settlement would ignore already admitted Tool cleanup.
Persisting raw ACP events as native Facts would give external observations native
execution guarantees that the local Kernel cannot establish.

Passing only an OpenCode default model through its environment does not select
the requested effort: an actual installed peer starts the selected Muse model
at `minimal`. Stable ACP configuration, confirmed before a prompt, avoids vendor
variant flags and silent default substitution. Automatic retry after a failed
business prompt cannot prove that the peer performed no effects and is excluded.

## Consequences

A borrowed async lock would release on cancellation while Journal's blocking
commit continues, so the [client](../../../../crates/rsi-acp/client/README.md)
retains ownership through publication. Separating completion from connection
availability prevents a cleanup failure from erasing a validated remote result.
Separating admission from flush observation distinguishes local rejection from
unknown remote effects without adding a second journal revision.

The independent TypeScript SDK 1.4.0 exercises both ACP roles over stdio.
The [interop fixture](../../../../fixtures/rsi/acp/README.md) owns its pinned
dependency and commands. It covers permissions, load replay, cancellation and
process cleanup. The SDK client also passes an opt-in DeepSeek run against the
native RSI server. A separate opt-in RSI client run against pinned DSH exercises
an actually called private MCP and resume without replay; DSH does not advertise
load, so that run provides no load evidence.

Owner tests reject malformed MCP setup before publication and check that supplied
secrets do not enter history or diagnostics. Controlled-work tests demonstrate
the interval between durable terminal and retained Tool settlement. Driver tests
exercise payload, frame and pending bounds and unknown-send behavior.

Peer disconnect or process loss can destroy cleanup evidence. Such outcomes are
explicitly unknown/unsettled and must never trigger automatic prompt retry.
Protocol conformance tests alone do not prove native Session or UI integration.
