---
name: Standing authority for bounded preview acceptance and exploration
---

## Problem

An external deployment is not human Session input. Existing restart-disarmed
Goal continuation, process-local Jobs and generic Storage records do not supply
durable webhook admission, protected browser execution or standing authority.

## Decision

An operator rule authorizes one frozen deployment attempt. Only independently
committed assertion failure can issue a live, restricted Goal. The product owns
this issuer, queue and dedicated SQLite ledger; Agent keeps canonical Session
and Goal durability. A bounded opaque protection scope in the immutable Header
lets product adapters narrow all public reads independently of artifact TTL or
ledger health. Public protected handles cannot mutate execution. Ordinary Agent
history and reference callers cannot convert Local execution into view authority.

Browser owns a separate resource scope with Chromium sandbox, isolated PID/net
and mounts, cgroup limits, CDP pipes and broker-only public egress. Private MCP
clients attach without launching Chromium or joining the global manifest.

## Alternatives considered

Global MCP and Jobs would couple unrelated Session readiness and confuse
resource ownership. Generic Storage upserts cannot atomically claim multiple
admission identities. A temporary browser data directory is state separation,
not confinement. Header protection avoids a mutable ledger-only ACL that could
disappear during retention or force unrelated Session reads to fail closed.

## Consequences

The namespace/CDP/broker combination requires native validation and fails closed
if unsupported. HTTPS CONNECT controls destination, not encrypted HTTP effects.
Page JavaScript can mutate a permitted preview backend. GitHub does not retry
failed deliveries automatically. This decision preserves the restart-disarmed
[Goal authority](2026-09-12-goal-continuation.md)
and [independent work](2026-09-24-independent-automatic-work.md)
contracts; it adds explicit fresh-event delegation rather than arming history.

Protected navigation needs caller-bound opaque scan tokens: the underlying Store
cut may name a hidden Session. Wire v4 carries only the last visible ordering key;
the owner retains bounded private cuts and rejects forged or expired tokens.
This changes negotiation together with clients instead of encoding random tokens
as fictitious Store timestamps.

Linux native checks observe actual Chromium sandbox flags, namespace separation,
effective cgroup memory/process limits, private MCP and termination after Host
SIGKILL. The renderer proof checks both PID and network namespace separation. Web, Desktop
and PTY checks use real product APIs; seeded presentation data and real-provider
execution remain distinct evidence. Native Windows/macOS and a public GitHub
account deployment are outside the exercised coverage.

Verification freezes one operator-installed runtime generation. The installation
must remain immutable until retirement; replacing it creates a new generation
and requires prepare. Hashing the same tree before each session would still leave
a verify-to-spawn gap and repeatedly occupy async workers. Prepare instead hashes
on a blocking worker and proves startup/settlement once; failed settlement fences
that generation.

Opaque cursor cuts are principal-bound, expire after five minutes, and have both
global and per-principal caps. A continuation replaces its predecessor, so a
long scan consumes one retained cut and cannot evict another principal's scans.

Admission uses one durable commit with a candidate savepoint. Rejection within
that admission transaction still advances the acceptance floor: losing that floor on rejection would admit old
events after clock rollback. Screenshot reads use finite responses outside GUI
frames. The intake journal is diagnostic and coalesces writes; a crash can lose
entries since its last flush without weakening receipt durability.

Recent Session listing uses the same bounded private-cut principle as Navigation.
A visible-only cursor cannot progress through a long unauthorized span without
either leaking identities or unbounded scanning. Empty pages therefore carry
continuation work explicitly; clients retain visible ordering separately.

Browser exchanges retire their scope on timeout or abandonment. Pump tasks retain
only private process ports, so dropping the last public owner starts retirement
without an Arc cycle. The cleanup guardian, rather than a request waiter, retains
capacity until settlement. Per-scope egress budgets remain aggregate: dividing
them by socket would allow reconnects to bypass an attempt's resource bound.

Ledger dispatch waits asynchronously before creating a blocking worker. Queuing
workers behind a connection mutex would retain a thread and durable owner for
each abandoned HTTP waiter; releasing dispatch admission with that waiter would
recreate the same problem. The worker therefore owns admission until settlement.
This preserves committed-effect ownership without making cleanup compete for a
new rejectable capacity reservation.

Isolated process service names belong to their issuer. Sandbox validates the
shared resource namespace and builds enforcement arguments; moving that wrapper
into Browser would make the consumer implement and claim its own confinement.

A model report can be committed before its provider stream fails. Its presence
therefore cannot settle exploration successfully. Goal owns proof that the exact
report source Turn canonically completed; Automation consumes that proof and
retains failed claims as diagnostics. All verified report dispositions can finish
the investigation without changing the independently committed check verdict.

Webhook delivery and event headers are not covered by GitHub's body signature.
Persisting every delivery name would let one valid captured body exhaust receipt
accounting. Signed-body deduplication therefore returns the first receipt without
allocating an alias. Conflicting header reuse still rejects unrelated bodies.
A matched-rule set shares one transaction, because returning an admission error
after committing earlier rules would leave an ambiguous partial delivery.

A failure to save an execution's terminal state cannot justify freeing its durable
slot: the execution may already have effects. Fencing the owner makes readiness
and further admission honest; restart disarms interrupted attempts. A timed live
reaper would conceal this persistence failure rather than prove settlement.

Automation mutation identities include the authenticated Local/Device principal.
The generic Ledger sees an issuer-prepared key; permission checks still precede
replay. Private frozen Session ingress similarly belongs to a trusted composition
issuer, which validates its product catalog and standing policy. The generic
Session owner cannot validate another product's catalog policy by hardcoding it.
Known Unauthorized and NotFound refusal categories remain distinct, so a caller
with an opaque Session ID can distinguish existence; uniform errors would remove
that established lifecycle information.

Browser native acceptance is opt-in locally and explicit in Linux CI. Its selected
test names are checked before execution: omitting a feature must never count a zero-test
binary as native proof. Terminal menu dismissal cancels reads but preserves an
admitted control waiter; native visual reports use observed frames and decoded
image dimensions rather than predetermined result labels.

Checker completion and process cleanup are independent evidence. Losing a complete
verdict when retirement fails would make a valid observation disappear and invite
rerunning effects; the owner preserves that result and refuses further exploration.
Cancelling its unfinished investigation changes exploration rather than rewriting
the failed checker outcome. Queue expiry belongs to maintenance because readiness
cannot supply a lifetime bound when Browser remains disabled.

The isolated mount issuer and private Automation directory operator are trusted
host owners. Their pathname resources must remain stable; namespace confinement
does not defend against another host process with equivalent account authority
replacing them. Pinning descriptors through the systemd service launcher would
require a different resource-transfer contract, while an extra pathname check
would leave the same race. Protected reference capture remains forbidden because
a durable reference would copy evidence into a separately authorized Session and
outlive its source's reading lease.

Client control and Workflow recovery ownership survive attachment reset and task
abort respectively. Replacing either with a cleared flag could admit a successor
while forgetting the original uncertain result. Domain validation of a mutation
response therefore preserves uncertainty too. Linux CI cleanup records preexisting
resources before setup instead of stopping an operator's existing user manager.

Header protection belongs to Session format 20. Its incompatible shape changes
Header fingerprints and invalidates derived context checkpoints; the allocation
golden intentionally binds the new fingerprint while retaining its existing
payload/envelope formats. No compatibility rewriting is introduced.

Redirect refusal withholds escaped page snapshots, assertions and screenshots.
Exploration policy coordinates come from private CDP target metadata, not MCP
text formatting. Its protocol reader and structured policy replies use separate
bounded channels so idle protocol reads cannot hold the policy response lock.
Operator cancellation remains the terminal exploration disposition even if a
late callback carries a report.
