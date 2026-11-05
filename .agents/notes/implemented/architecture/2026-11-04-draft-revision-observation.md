---
name: Observe draft revisions and explicitly own active drafts
---

## Problem

The application bridge refreshed draft inbox and history every 250 milliseconds
because durable observation starts only at publication. Domain projection streams
already observe draft changes, but running callbacks and retaining full snapshots
solely to learn that a revision changed adds unrelated work. DSH's Agent lifecycle
uses notifications and activity promises instead of repeated state capture.

## Decision

The standard Session owns a coalesced draft-revision subscription. It uses the
existing draft change sender and registers before reading state. Publication has
an explicit terminal notice. The subscription borrows a non-renewing inspection
only during capture; waiting does not keep a draft active or reset its deadline.
The existing guarded stream retains access and retirement semantics.

Session API transports the same contract. Serve's application inbox wait uses
that subscription, and the DSH bridge refreshes only after revision change or
publication. No DSH lifecycle policy enters Session or Kernel. Contracts live in
the Session protocol and API package READMEs and the Serve application contract.

A Local-only draft owner keeps an unpublished draft eligible beyond its idle
deadline. It is distinct from an executing command or read, carries no execution
authority and cannot prevent explicit discard, publication or service shutdown.
Its weak reference identifies an exact draft generation. Releasing the last
owner restores the ordinary idle grace period. The bridge acquires it before
Agent setup and releases it on Cordis disposal, including failed or cancelled
acquisition. Serve retains one owner table per application callback so Agent
ownership does not consume one request snapshot slot per draft. There is no
renewal timer, task or additional durable representation.

## Alternatives considered

Faster polling still performs idle work. Reusing complete projections pays for
domain callbacks, snapshot allocation and retention. Watching only application
events misses changes from other native callers. A JavaScript-only notification
cannot establish native publication or expiry.

Using an observation as an owner makes reads secretly extend draft lifetime.
Using a command activity as an owner prevents explicit discard and pins the
draft state until an idle observer closes. Heartbeat renewal adds recurring IPC
and can still lose a draft during scheduling delays. DSH instead binds a live
Agent to the creating Cordis Fiber and drains it on disposal; the bridge uses
that lifetime to own a native draft explicitly.

## Consequences

An idle attached draft holds one observation instead of repeatedly capturing
inbox and history. Detach cancels the wait. The existing guarded stream still
owns its demand worker; this change does not claim to eliminate all observation
tasks or durable observation overhead. Agent cancellation, wake parking and idle
convergence remain separate native lifecycle work.
