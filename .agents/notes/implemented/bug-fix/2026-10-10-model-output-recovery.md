---
name: Bounded recovery from incomplete model output
---

## Problem

A model's output reserve describes context capacity, but does not set a provider
request limit. Long tool arguments can therefore hit a provider default cap.
Treating that response as ordinary output either admits incomplete history or
reports task completion without executing the intended write.

## Decision

The [Session contract](../../../../crates/rsi-agent/session-protocol/README.md)
owns immutable exact-model request budgets and a bounded Turn recovery allowance.
The [AI contract](../../../../crates/rsi-ai/protocol/README.md) distinguishes an
incomplete response from a successful output. Projection and pressure rationale
belong to the [compaction decision](../architecture/2026-09-12-durable-context-compaction.md);
source authority and marker ownership belong to the
[Tool origin decision](../architecture/2026-09-14-tool-model-origin.md).

Recovery is a fresh logical model effect, authorized by a flushed native Fact.
This keeps recovery separate from retries of undispatched requests and from
summaries. A deterministic instruction asks the model to split writes rather
than continue an incomplete argument stream.

## Alternatives considered

Increasing a context reserve alone cannot change the HTTP request cap. Continuing
partial JSON or executing complete siblings makes an incomplete batch authoritative
and risks writes based on an unfinished plan. A harness-side continuation cannot
authenticate the native model effect or enforce the Turn's allowance. Unlimited
replanning can consume time and money without delivering an artifact.

## Consequences

An empty output budget table leaves provider defaults unspecified; operators must
choose explicit caps for their deployment. Recovery can improve delivery but is
not task-success proof: independent artifact checks and graders remain necessary.
Exhaustion is an ordinary budget terminal with ACP `max_tokens` projection.
Recovery does not guarantee that protected input and its instruction fit the
request; the compaction decision above owns the explicit context.limit trade-off.

The new Session shape rejects older authoritative Headers. Derived Context caches
use a new identity and payload version. No authoritative Store migration or
compatibility reader is added in this pre-release repository. Omitted policy
fields in the current format use the Session contract's defaults; version
admission precedes that decoding and still rejects every older Header.
Interrupted model
effects remain conservatively interrupted after a crash; recovery never replays
an uncertain dispatched effect.
Live executor replacement can continue after retained Tool settlement. It must
reconcile the recovery ordinal from the claimed Turn's Facts instead of resetting
the allowance; this does not authorize resuming an unfinished model attempt.
