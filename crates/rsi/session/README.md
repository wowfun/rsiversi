# rsi-session

Request-evidence reads bind an exact ModelIntent sequence in the attached Session.
The Session owner resolves one direct original-inline reference per selected
section, checks its digest/length and returns at most 256 KiB aligned UTF-8 bytes.
Reads neither reconstruct context nor acquire execution authority. The API client
validates the echoed identity, page bounds and unavailable state.

Goal control revision and identity conflicts remain typed command conflicts,
bound to the original request ID, across Session API. Live-owner contention
uses the existing retryable `SessionError::Capacity` admission error so clients can retry without classifying it as
a backend failure. A rejected pause/cancel
still revokes live scheduling before its command gate; it does not claim a
durable phase change or cancellation of the current Turn. The caller can issue
a new explicit control from a fresh revision after a known rejection. Unknown
outcomes retain their original identity and require receipt reconciliation.

Current-Turn Jobs reads delegate to the Kernel's read-only claim relay through
this handle's exact Header. Capture and returned-page retention have a separate
bounded owner. No Jobs scope is acquired by Session, and a missing, finalized or
replaced claim returns unavailable rather than a fabricated empty historical list.
Page semantics and bounds belong to the [shared Jobs contract](../session-protocol/README.md).

Goal controls delegate to the separately owned Host Goal controller. The Session
adapter supplies a narrow `GoalSession` bridge: current typed domain plus command
revision from one Store snapshot, staged draft freeze under its mutation lock,
ordinary command receipt reconciliation, Workspace preparation and exact Kernel
continuation submission. It retains the existing Header and selected composition;
the controller cannot choose another preset or execution policy.
Outcome waiting subscribes before reading canonical message/Turn state and uses
coalesced changes, bounded reads and cancellation. Live Goal status is separate
from the durable Goal projection. A Session without this optional Host capability
returns an explicit unavailable result; observation never arms execution.

This package implements the native Session domain service over the Agent Kernel,
Store, Agent composition, Workspace and Media. Its transport-independent
[contract](../session-protocol/README.md) owns requests, handles, receipts,
observation retention and errors. Application roots consume Session services.

Extension capture uses the Agent's independent `SessionProjections` service for
durable state and the actual retained draft for unpublished state. Draft mutation,
publication and expiry publish coalesced notifications under their state admission;
callbacks run after releasing it. A live stream subscribes to both draft changes
and Kernel commit hints before its first capture and retains only the immutable
Header fingerprint while waiting. Service retirement cancels all live streams.

`SessionFactory` publishes `SessionContract` and trusted `SessionIngressContract`
from one service generation, injecting its exact
Kernel, Store, composition, Workspace, Settings and live capabilities through
Meta, including the shared `ContextBudgetContract`. All clients of that generation receive the same service. The standard
Host's approval broker is also an ordinary plugin; the launcher neither
constructs Session adapters nor registers a second broker per client.

Trusted ingress binds a service view to the actual authenticated caller for all
operations, including streams and Header reads. Returned handles retain that
caller and consult Execution admission for every new operation. Reconciliation
of an existing draft shares its state, not the original caller's authority.
Recent enumeration applies a retained visibility grant inside the Store's
creation-ordered page query. Activity visibility uses batched scalar summaries,
without decoding full Headers. Storage and authority failures remain explicit.
Stream publication checks current authorization for each item and releases that
short-lived admission before yielding ownership to a consumer. A suspended
consumer cannot hold the shared operation pool; the next item rechecks revocation.
Remote metadata reads require Use but remain available while disconnected.
Execution coordinates never authorize an operation or select a Local fallback.
Resource list/body reads resolve one current-caller execution lease and pass it
through the draft or resident composition to the reader. The immutable Sources
catalog remains a metadata read and does not require an online target.
Draft creation accepts a registered SSH workspace after current-caller Use
admission. It freezes coordinates without resolving a connection or touching the
Service filesystem. First input acquires the live target lease; a disconnected
draft cannot start work.
Input preparation acquires one current-caller execution lease before checking the
workspace and carries that same lease into the prepared submission. It never
resolves a newer provider between path validation and Turn admission. The selected
provider canonicalizes the durable coordinates; Session submission does not
re-register a removed Workspace or consult a different filesystem.
The standard factory requires the product Execution resolver; an explicitly
constructed native-only service without that capability rejects SSH coordinates.

`LocalSessionService` is constructed from already-owned capabilities. Draft
creation resolves a registered WorkspaceId, freezes the canonical workspace and settings,
rejects a durable identity collision and retains the actual Agent draft payload,
including typed initial states and its preset generation. Attach and
history read only the durable Store. Submission defers execution dependencies
until the selected operation requires them; fresh publication serializes the
one transition from draft to durable attachment. Each fresh publication freezes
that retained payload, including its baseline digest, under the same admission.
The current Header belongs to the Fresh or Attached state. A handle retains only
its Session identity separately, so preset changes cannot leave a second stale
Header behind for binding or publication reconciliation.
Already admitted publication
waiters recheck the draft state after acquiring that transition lock; a draft
expired by a conflicting publication returns NotFound.
Activity admission rechecks successful publication when its former draft lease
has retired between the initial publication check and lease acquisition. Only
confirmed publication permits the durable path; actual expiry, capacity and
shutdown remain errors.
Fresh handle Header/history reads also reconcile against the Store. A matching
publication releases the draft pin and exposes durable history; a different
Header expires the old handle. A new attach then returns the durable Header.

Live interaction collection reserves its bounded snapshot budget before broker
payloads are copied. The resulting immutable snapshot and all its clones retain
the same byte lease. Its observer holds live services and identities, never a
fresh composition pin. Native filesystem work remains outside shared protocol
consumers.

The same service generation also publishes `SessionReadContract`. It binds the
actual ingress origin before attachment and rechecks location admission. A finite read
acquires the existing draft activity owner, reconciles against the Store and
compares the current Header under that activity. Durable reads need no Agent pin;
expired drafts are rejected and service retirement cancels leases. The owning
API establishes authentication separately, then retains this lease through the
read. File tokens hold only correlation data and filesystem resources.

Metrics cache admission reserves at most 64 own-Session or tree slots, conservatively charged
as 256 KiB each (16 MiB total), and four concurrent readers. A Session has one
serialized forward scan; concurrent callers wait on its cursor before acquiring
one of four workers. Cache-slot, worker and retained-byte admission failures all
report the domain `SessionError::Capacity`. Inactive entries
evict in least-recent-use order. Each read yields a progress response after at
most eight 128-Fact pages or 16 MiB of processed Facts. One larger Fact is processed
alone to guarantee progress; a Store page remains indivisible during acquisition
under the Store's existing page bound. This cache retains
only reducers and tree counters, never Facts, headers, transcripts or effect-ID sets. Shutdown and
caller cancellation drop the read work and its permits.

An explicit tree metrics cycle freezes root plus at most 255 lexical descendants
from one Store inspection. It reports the membership control cursor and whether
the roster was truncated. Each member captures its own Fact watermark when its
scan begins; these cuts are explicit and are not a simultaneous tree snapshot.
One reducer advances members in order, retaining only their cursors and a checked
aggregate. Per-member completion and whole-cycle completion are distinct from
membership completeness. A completed cycle remains readable until an explicit
refresh starts another; reads never attach or resume descendants. This avoids
thrashing the own-Session cache when a tree contains more than 64 Sessions.
Each retained slot must fit its 256 KiB accounting reservation, including owned
string/vector capacities and inline structures; an over-budget state is discarded
before reporting capacity failure. No response stores a lifetime
set of effect identities. Tree aggregate currencies are capped at eight and
checked overflow is an explicit read error, never a saturated reported total.

Attached handles share their immutable Header internally. Metrics and model
availability polling do not copy the frozen pricing table; an owned Header is
materialized only for the public Header response.

Live terminals are retained by one registry owned by the Session service
generation. Creation checks the persisted Header and freezes its authorization;
subsequent operations use the typed live scope without Store reads or Header
fingerprints. Creation revalidates its canonical workspace and confines an explicit PTY-intent Bash
plan. Local shells receive fixed PATH, workspace HOME, SHELL, TERM, LANG and
disabled HISTFILE. SSH shells receive target account HOME and the target's fixed
program environment. Neither inherits Service credentials or shell startup files.
Retiring this service retires all scopes even while detached handles remain.
The registry neither pins an Agent composition nor writes terminal state to Store.
Closing the last terminal releases its empty scope after admitted operations finish;
failed first creation also releases that slot. Detach and Kernel eviction retain
nonempty scopes. Explicit operation leases determine the last admitted operation,
independently of incidental shared-owner clones. Its release checks the local
scope emptiness snapshot without issuing a List operation. Concurrent creation
and closure serialize scope mutations.

Service retirement attempts every terminal scope even if one cleanup fails. The
first cleanup error propagates through the Session plugin's finalizer; successful
draft cleanup cannot turn a failed native reap into a clean Host shutdown.
Retirement retains scopes until cleanup completes, so cancelling a waiter does
not lose cleanup ownership. Repeated completed retirements return the same error.

The standard `SessionFactory` requires both PTY and Sandbox local contracts and
publishes the complete product service. Direct `LocalSessionService::new` callers
may omit optional capabilities, as with other embedded/test service assemblies;
without `with_terminals`, terminal requests return unavailable. This constructor
is not an alternative plugin dependency declaration. Terminal authority is
Session-wide for authorized single-user clients, not a per-pane shell ACL.

Activity caches at most 128 durable watermark/open-Turn pairs under the Kernel
commit revision captured before the read. An unchanged revision avoids Store
reads without allocating Session observers or generation pins. Providers without
a revision fall back to fresh reads. Broker requests and resident running state
are always sampled separately.

`SessionService::read_header` reads one durable Store Header by SessionId without
attaching, creating a handle, touching activity or loading transcript. Unpublished
drafts are absent. The authenticated `session/read-header` v1 operation exposes
the same read and validates the echoed identity; narrowed Session contribution
clients do not gain this deployment-wide operation.

A new queue replacement transfers its owned content through normal input admission
and back into the unchanged request before Kernel submission. Validation does not
clone the full replacement payload; committed retries still resolve their receipt
before reading media or workspace state.

Terminal creation resolves one execution lease for the authenticated caller,
verifies the durable workspace through that lease, and freezes the terminal
program/environment and restricted plan. Remote shells use the target's
`terminal` selector; native-only embeddings retain their explicit Local path.
The retained scope is Session-owned. Each input and resize selects the current
caller's lease and the PTY owner rejects a different provider or connection epoch.
Other bounded terminal operations use the Session's metadata admission and remain
available without reconnecting; private output pumping never publishes to a caller
without a separately admitted request. Creator revocation does not lend that
creator's authority to another authorized attachment.
