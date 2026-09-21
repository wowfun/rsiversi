---
name: Bounded current-owner attention navigation
---

## Problem

Recent durable sessions cannot establish current execution or pending human
interaction. Polling only residents loses fast completions when Kernel evicts an
idle generation; holding full controllers pins generations and transcript data.

## Decision

Session retains only 64 recently opened identities and combines these with a
bounded instantaneous Kernel roster. It reads durable watermarks without hydration
and keeps live broker targets separate. A cheap Kernel commit revision invalidates
the bounded metadata cache without consuming Session observer slots. Navigation joins these native observations
with the ACP owner's eight peers and persists only explicit reading positions.
Unknown ownership remains visible instead of inferring execution from old Facts.

## Alternatives considered

A global durable history scan defeats bounded live navigation. Retaining a
controller for every recent session adds transcript memory and generation pins.
Persisting running flags resurrects obsolete process authority after restart.
Per-Session watches would consume the observer budget merely to display activity;
an optional revision allows providers without this optimization to keep fresh reads.

## Consequences

Owner tests cover a completion between polls, active and unknown ownership, exact
approval/question targets, bounded rosters, reading-position persistence and
retirement. Real Linux PTY, Chromium, Firefox and Linux desktop bridge fixtures
exercise opening the selected target; these do not establish native Windows or
macOS behavior.

The view intentionally covers a bounded subset and reports truncation.
Reading a row and acting on its request can race settlement, so the owning
approval or ACP endpoint must revalidate the exact interaction identity.

Reading positions are bounded recency metadata, so full capacity evicts older
acknowledgments instead of permanently rejecting all principals. Eviction can make
old observations appear unread again; it never grants access or suppresses another
principal’s unread state. Poll backoff trades up to eight seconds of idle discovery
latency for lower IPC cost, while explicit invalidation resets the interval.
