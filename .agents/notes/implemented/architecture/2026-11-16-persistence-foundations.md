---
name: Persistence foundations and cache ownership
---

## Problem

The former three-root Host layout mixed durable data and service state. Untyped
business Domains copied whole maps, and navigation rewrote one aggregate
document. Process capture and generic Tool output needed the same best-effort
retention mechanism. Durable encoding changes could invalidate Fact and control
prefix digests without changing a format version.

## Decision

The product freezes config, data, state, cache and scratch paths at launch.
Home-based XDG defaults use the `rsiversi` suffix on every platform. The
[product path contract](../../../../crates/rsi/core/docs/launch.md) owns
resolution and placement; [Service Host](../../../../crates/rsi/service-host/README.md)
owns durable writer leases and endpoint placement. Separating persistent data
from service state requires reset to journal all three durable authorities before
moving their owned entries. Scratch does not own durable authority or endpoints.

[Durable format verification](../testing/2026-11-16-durable-format-verification.md)
owns fixture retention, codec checks and executable writer evidence. It adds no
runtime migration chain or source hash inventory.

[Typed Domain](../../../../crates/rsi-storage/domain/README.md) adapts DSH's table,
global, update and subscription model to Rust ownership. Codecs admit durable
input once; snapshots share immutable values. One queue owns accepted writes
through cancellation and publishes value-free events after commit. Bounded
admission permits one write ahead of the executing job, preserving throughput
without an unbounded retained payload backlog. Waiting callers retain ownership
until admission; single-value encoding happens after capacity is acquired. Close waits
for accepted work and authority teardown. Backend uncertainty remains a fence.
Final-state limits alone cannot bound a batch of missing-key deletions. Mutation
count admission therefore allows both a full old set and its replacement, while
validation uses a shared immutable snapshot outside the publication lock. The
sole writer prevents interleaving; readers need not wait for validation.
[Navigation](../../../../crates/rsi/navigation/README.md) separates metadata,
order and revision, and submits affected records in one atomic batch. Backend
capabilities make the distinction between atomic document/SQLite writes and
per-record JSON explicit.

[Spill](../../../../crates/rsi-spill/README.md) adapts DSH's storage/policy split.
Generic text retention and Process capture share one bounded backend; Agent owns
Session association and policy integration. Typed locators survive pruning and
source inspection without placing expiring cache files in the durable Media
reference graph. Each Host cache authority has its own lease. Session purge
fences capture, then records independent canonical and cache cleanup receipts so
cache failure remains retryable after canonical deletion. Cache failures preserve
valid Tool results and command progress.
Retention scans keep directory cursors and yield between bounded slices on the
same worker. This avoids making cache size a startup or capture latency bound,
without a second filesystem owner or a scan cap that strands old objects.

## Alternatives considered

Windows Known Folders and placing all business data together would be reasonable
defaults, but the approved product contract explicitly chooses home-based XDG
roots and separates media from state. Cross-volume reset therefore requires a
recoverable journal rather than a false atomicity promise. Omitting optional
Domain capabilities would reduce scope; the approved plan retains them with
backend capability checks and conformance tests. Durable image spill would need
a supported image route and media retention design, neither supplied here.

## Consequences

Home-based Windows roots may roam. Free-space admission cannot reserve space
against unrelated writers. Cache expiry or original-owner purge can invalidate
fork locators without invalidating durable fork history. JSON remains Unix-only.
Cross-volume reset is recoverable, not atomic across filesystems.

Per-record JSON cannot provide navigation's cross-record crash atomicity, so
navigation requires a backend with atomic batches. Only explicitly disposable
tables may preserve and skip invalid records. Compatible reads do not rewrite
records. Full SSH capture remains deferred because upstream tails may already
contain gaps; text spill cannot restore bytes discarded before admission.

Existing TurnExecution flush barriers remain the durability boundary; this
decision adds no service-level flush abstraction.

Targeted Windows and Linux/WSL tests cover ordering, cancellation, corruption,
path authority, capture and purge. Browser and Windows Desktop checks exercise
restart persistence, and an isolated real DeepSeek conversation exercises spill
and retrieval. Default verification stays keyless. Native macOS validation is
outside the implementation environment.
