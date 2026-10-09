---
name: Cross-workspace history reuse and Session browsers
---

## Problem

Conversation-scoped text search requires users to know the source identity.
Browser exploration lacks a shared interactive page and durable Media output.

## Decision

History discovers finite locally saved sources under current caller authority.
Models retain their workspace boundary; humans explicitly transfer selected
original data through independently admitted source and target scopes.
Search pagination expires when content or visibility changes. Browser resources
belong to a Service owner keyed by immutable Session binding, with bounded
operations, precise local-origin grants and real process settlement.

## Alternatives considered

Full corpus snapshots require retaining per-source horizons and versions without
solving authorization revocation. Conservative cursor invalidation is simpler.
Generic MCP forwarding does not supply the product's shared Session authority,
image contract or node freshness checks. Browser pooling would share page state.

## Consequences

History pagination expires on any index revision or admitted-source-set change.
An empty indexing batch with unchanged coverage publishes no new revision. Polling
an already indexed source therefore preserves a page without retaining a snapshot.
The discovery pass is finite and live rather than an atomic Host snapshot;
partial metadata failure is explicit and a fresh pass retries it. This avoids
retaining corpus snapshots while preserving current authorization.

The browser is opt-in and Linux Local only. Its temporary profile, observations
and precise local grant end with the instance; durable Tool/Media evidence follows
the existing retain-all policy. Import admission bounds each BrowserId, rather
than inventing a separate garbage collector. Cancellation and deadlines stop new
work while retained Process and Media operations drain through actual settlement.
An uncertain publication can outlast the caller budget and retains its reserved
image charge.

The Service runtime pool pins its owner before asynchronous readiness work.
Failed or cancelled preparation keeps that owner: replacing it could leave an
earlier probe alive beside a second capacity owner. Readiness serialization belongs
to the runtime, while the pool lock only selects the pinned owner.

Both clients use the same standard model and authorized image sources. The Service
UI bridge requires asynchronous presentation because a synchronous UiView loses
model data and source bindings. The TUI cell renderer has no native image viewer;
a grayscale preview provides an inspectable image without an external application.
The current DeepSeek adapter accepts text only. Structured page evidence remains
available to that model, while durable image results and human rendering retain
the screenshot; this does not establish model pixel inspection.

The bridge publishes the committed model separately from its action owner. This
allows rendering and image reads during a slow action while keeping actions
serialized and one-use tickets owned. Source completion checks the captured
presentation and revision again, preventing a replaced or closed view from
receiving a late result.

The model Tool projects closed operation-specific schemas and turns malformed
arguments into a bounded not-dispatched error result before Browser admission.
This keeps a model's correctable field mistake from terminating its turn, while
preserving strict decoding and the no-replay rule for dispatched effects. Missing
trusted Agent authority remains an execution failure.

Page observations cross Rust twice: private CDP replies precede the public helper
result. CDP JSON therefore crosses Rust as an opaque string, preserving private
UTF-16 semantics while public observations use scalar values. Chromium can
report raw DOM attributes before page evaluation finishes, so repairing only
evaluation results does not protect that earlier boundary. An escaped private
signature preserves exact node semantics for later actions; comparing repaired
names alone would merge distinct action targets.

Pool pressure is distinguished before native launch and becomes a retryable
business refusal. An independent native cleanup owner retains capacity through
both Process and bridge settlement and supplies one receipt to all close waiters.
A failed or timed-out settlement fences the generation before
its Session binding is removed; the Process owner retains unproven native cleanup.
This lets presentation recover without treating a timeout as reusable capacity.
Media publication ownership is independent, so an operation deadline can stop
waiting without releasing an uncertain image charge or delaying native retirement.

History range work retains a permit per execution location, rather than per
conversation. Keeping every source's separate admission alive through a query
exhausted the standard operation pool at 64 saved conversations, including the
extra permission check required before publication. Sharing the retained pin
preserves actual-settlement ownership without treating catalog entries as
simultaneous backend operations; fresh source and final checks remain separate.

Scope, limits and lifecycle contracts live with
[History](../../../../crates/rsi/history-api/README.md),
[References](../../../../crates/rsi-agent/references/README.md), and
[Browser](../../../../crates/rsi/browser/README.md). Default validation stays
keyless and isolated; confined native, provider and visual probes remain opt-in.
