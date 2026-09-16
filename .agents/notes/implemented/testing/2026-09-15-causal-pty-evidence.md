---
name: Causal PTY transitions and independent CI evidence
---

## Problem

A generic footer can already be present when a Session switch starts. Waiting
for that text does not establish attachment completion. Opening another view
before attachment completes can invalidate the pending presentation revision.
The raw-output cap also stopped the terminal parser, freezing later screens.
Independent browser checks were skipped after an earlier failure.

## Decision

The PTY switch test binds each submission to its unique provider response and
request count. It first observes a new empty composer with the old response and
menu absent, then opens Session details once to verify the changed identity.
Every PTY chunk reaches the parser. Raw retention keeps a bounded prefix and
ring tail, records the gap, and refuses full-transcript assertions after truncation.
Polling searches the retained slices in place, including contiguous slice
boundaries, without joining the capture or matching across a discarded gap.

Browser checks declare actual prerequisite outcomes and run independently when
those prerequisites succeeded. The guard discovers browser test steps from their
commands and requires an ID, including newly added steps. Logs and categorical
step outcomes survive failures; arbitrary step outputs are not copied. A
successful product check must produce a parseable report marking both browsers
passed; an empty output directory is insufficient; a skipped check remains a
skipped check. Every committed lockfile is audited even after an earlier audit
fails. The aggregate gate continues to reject any unsuccessful required job.

## Alternatives considered

Longer sleeps leave the switch race intact. A new production readiness signal
would add an interface for a test that can use existing observable identity.
Concatenating a truncated transcript could invent matches across missing bytes.
Ignoring missing evidence would hide failures; requiring product output from a
step that never started would obscure the original failure.

## Consequences

PTY artifacts distinguish a complete transcript from a retained prefix and tail.
Negative transcript assertions require complete capture. CI collects more
independent evidence without accepting skipped tests as passes. Linux execution
can validate these mechanisms, but cannot establish native macOS or Windows
behavior or a hosted GitHub Actions run.
