# rsi-session-protocol

`SessionService::activity` reads only the current generation's first 64 resident
identities and its 64 most recently opened/created identities, deduplicated. It
never discovers historical sessions, prepares a generation or retains a Session
handle. Results carry exact pending approval/question identities, a durable Fact
watermark and current-owner running evidence. A durable open Turn without that
evidence is `Unknown`; a closed durable cut is `Idle`, not proof that all external
effects settled. Missing unpublished drafts are omitted. Truncation is explicit.
Collection admits two callers, has a 30-second deadline and reserves the existing
bounded interaction payload budget before reading brokers. At most 32 interaction
identities per Session survive into the returned metadata.
The union therefore contains at most 128 identities. The current adapter performs
one sequential, single-row open-Turn Store read per selected identity under that
shared deadline; this is a bounded activity view, not historical enumeration.

`SessionDraftControl` is a Local-only lifetime capability. It explicitly expires
one completed, inactive, unpublished draft and releases its generation pin.
Missing or published drafts return NotDraft; construction or active operations
return Busy. It neither deletes durable history nor cancels execution, and is
not exported as a remote Session operation.

Reference capture freezes a durable source Session's bounded conversation text
for this target's actual Header before send. SessionInput::Reference carries the
returned Agent-owned FrozenReference unchanged. Submit verifies the complete CAS
envelope and original-target binding; an exact retry never recaptures the source.

`SessionHandle::peek_job` is a required read-only current-claim operation. A
Session API client generation admits at most two in-flight peeks and four starts
per second. The server independently bounds concurrent peeks to two; it does not
enforce a per-connection polling interval. Replies are bounded to 64 KiB.
Reads are cancelled with their Session lease. Consumers
retain at most one focused job and 256 KiB of preview data, cancel on close or
session switch, and use durable results when the process-local source is gone.

`SessionError::SetupRequired` identifies missing configuration when creating a
new Session. It is distinct from invalid settings, Store failures and API errors.
Its diagnostic names the configuration field; each application supplies its own
setup entry points rather than embedding terminal commands in the shared error.
Attaching durable Sessions keeps their frozen settings and needs no global default.


The optional current-Turn Jobs status read binds a caller-selected active Turn
to this handle's exact Session/Header. It returns at most 32 summaries / 64 KiB
encoded per page and retains that page's bytes through its final clone. One
adapter or decoder uses a separate 64 MiB pool; local capture reserves 8 MiB
before sampling the bounded Jobs list, and wire decode reserves 64 KiB before
materializing a page. The view cannot acquire scopes or read/report/kill jobs.
The exclusive job-id cursor is lexicographic and scoped to that live Turn.
Independent pages may observe different status instants and tombstone eviction;
callers discard pages after the active Turn changes. Unavailable claims are not
read from durable history.

The optional Goal capability exposes explicit `control_goal`, current
`goal_status`, and coalesced `observe_goal` on the attached Session. Controls bind
an immutable command request identity and expected revision. Live state is a
bounded process-local observation, separate from the pure durable Goal projection.
History, status, projection and receipt reads never arm a Goal. An absent Host
controller is reported as unavailable instead of inferring execution from state.

This library owns the standard product's transport-independent Session
contracts and bounded retained observation values. `SessionService` creates, attaches, and lists sessions;
`SessionHandle` hides Agent draft tokens, Kernel admission, Store cursors,
routing checks and approval-control
plumbing behind durable message submission, history, observation,
cancellation, direct image generation, and approval operations.

Session handles expose command discovery, invocation and receipt lookup through
the Agent-owned bounded command DTOs. Fresh handles use the actual draft payload;
durable handles obtain Kernel-issued command authority only for that operation.
Draft commands and first publication share mutation admission. Concurrent exact
draft requests join one service-owned callback, with a 30-second deadline and
64 distinct in-flight mutations, shared with preset preparation, per Session
service. Retirement cancels draft
callbacks, and a published or expired draft cannot accept their late results.
Durable commands retain the Kernel's canonical receipt semantics. Unknown
outcomes preserve the typed request identity for explicit query; no adapter
automatically repeats a callback.

Draft snapshots atomically expose the current Header and mutation revision. Preset
selection prepares a complete replacement outside mutation admission, then applies
it only to the same still-unpublished draft at the expected revision. Preparation
has a 30-second deadline and stops with the service. Success resets domain defaults
and advances the revision; failure preserves the existing Header, pin and values.
Publication and expiry reject late selection results. A transport failure leaves
the caller to reattach and inspect the current draft; selection is not replayed
automatically. Creation input remains the original lease identity after a switch.

`observe_projections` returns a complete initial extension snapshot, then
coalesced replacements derived at one draft revision or durable Fact/control
cut. Each snapshot binds its Session, Header fingerprint and captured generation.
The subscription retains that Header binding: a preset change ends it, requiring
reattach and a new subscription. Same-preset draft revisions and publication may
continue; neither durable watermark may regress. Individual producer failures
remain entries and do not affect core history.

Projection snapshots use a separate 64 MiB canonical-byte retention pool per
Session service or client decoder. Native capture reserves 7 MiB before copying
the bounded Header/domain inputs and computing at most 5 MiB of output; completed
snapshots retain only their exact encoded charge until their last clone drops.
This bounds canonical payload ownership, not allocator RSS or arbitrary native
plugin allocation. Decoders reserve before decoding and transfer that lease
before releasing transport bytes. Capture is cancelled on drop or service
retirement and bounded by 30 seconds. Idle subscriptions hold no draft activity
lease or composition pin, so observing cannot prevent expiry. Changes subscribe
before capture; no periodic polling or Fact replay is needed.

Creation, input, receipt and page values have closed Serde representations.
They describe domain API values, not a new durable Session or Store format.

All three observation streams use `SessionError` for opening and item failures.
The Fact/control stream retains the Agent's typed payload without adopting its
service error type. API-backed capabilities retain common transport failures in `SessionError::Api`.
Authentication, capacity and uncertain outcomes are not converted into domain
rejections. A message's caller-owned identity remains the reconciliation key.

Model enumeration belongs to AI's independent `LanguageModelsContract`.
`SessionHandle::read_message` reads the
immutable acceptance body at a positive `accepted_control_seq` and requires
that exact control record to be `MessageAccepted` for the requested message.
It uses the existing Store control log, without a second body index or replay
subscription. A stale or mismatched identity is not a neighboring record.

Attachment is a durable-data operation: it reads the Store Header and builds a
handle without resolving the current preset generation, Language/Image route,
filesystem state, or Workspace registry. Those execution dependencies are
prepared only by the selected operation. Draft creation takes a registered `WorkspaceId`. Its owned preparation resolves
that exact registration into the canonical-cwd Header, rejects an identity already present in the
durable Store and pins its preset. It neither probes the filesystem nor registers
a Workspace or requires the default Language route. Clients register or select a
Workspace through its independent capability before creating a Session.

`AgentSettingsSource` reads a fallible current Settings snapshot for each new
draft. The returned Header freezes that value; later Settings changes affect
only new drafts. A stale or unavailable Settings registration fails creation
instead of silently reusing an old template. Attached Headers never read current
defaults. The caller allocates the Session identity before creation and retains
the complete request for retries. During one live draft lease, identical
input shares one creation and composition pin; changing that input
for the same identity returns `DraftConflict`. Published identities still
require attach rather than create.

The Session service owns at most 1,024 Creating/Ready drafts across every
adapter. Creation reserves its slot before Workspace lookup, Settings or preset preparation.
The same owner limits each authenticated device to 64 Creating/Ready drafts.
`SessionIngressContract` accepts the trusted call origin at the server boundary;
application clients receive only `SessionContract`. Origin is not creation input
and cannot be supplied through JSON. The first creator owns the device charge;
an identical retry from any client shares that lease before checking new-draft
capacity. Publication, failed preparation and expiry release the charge together
with the draft. Local callers share the global bound without a device charge.
An identical live retry does not repeat those reads, even if the registration
is subsequently removed; a new draft with that unknown WorkspaceId fails.
Dropping a create waiter does not cancel its service-owned preparation. Creating
drafts have a one-hour absolute preparation deadline; timeout drops preparation,
fails its waiters and releases capacity. Idle Ready drafts expire after one hour;
activity renews that lease, and active
operations retain their lease until completion. Targeted lookup expires only
that identity; a one-minute service sweep reclaims all idle expired pins.
Expired local handle clones cannot retain a pin or revive a draft. A successful
durable publication releases the slot. If the Store instead proves that the
identity belongs to a different Header, the old draft expires and cannot
silently resume that other Session. Retirement cancels preparation, stops
admission and releases unpublished pins. Restart and expiry do not recover a
previous generation pin from the idempotency identity.

Message submission validates its effective Language route, verifies each already
uploaded canonical `MediaRef` through Media, and registers the canonical
Workspace after resume preparation. Upload is an independent durable Media
operation. A later Message rejection, cancellation or uncertain outcome does
not delete an uploaded object: another Session may already reference it. Clients
retain the exact references with their retryable Message body. Cross-service
reference tracking and garbage collection are outside this contract. It durably returns a Message receipt, not a speculative
Turn identity: claim creates the Turn and Step later. Direct Image generation
is a separate operation and validates only its Image route. A fresh handle
serializes its first durable publication so exactly one generation pin wins;
once attached, independent submissions release the handle state lock before
resume preparation and backend I/O.

Native execution, filesystem access and draft preparation belong to the
[Session implementation](../session/README.md). Wire adapters consume these
contracts directly; protocol consumers do not link the native implementation.

A successful submit is a durable receipt containing the exact message identity,
its acceptance control cursor, and the durable Fact tail observed when the
receipt was produced. The caller supplies `MessageId`; retrying
the same canonical Header and request is idempotent, while reusing the identity
for different input is a typed message conflict. The indexed message-status
operation and the reconnectable control stream both expose the later claim,
including its created Turn identity and model-visible Fact cursor. Session
observation takes independent control and Fact cursors and never infers one
stream's position from the other.

History is bounded. A missing backward cursor selects one page ending at the
durable tail, and returned Facts are always ascending. Durable Facts and
Agent-control records are the historical authority; subscriptions are
reconnectable. Approval listing and answers cover the exact durable Agent tree
rooted at the attached root Session, so a child request is never hidden behind
a root-only client surface.

Human ingress carries immutable `NextTurn` or `Steer` intent; the Kernel alone
resolves its current route. Read-only inspection captures Header, durable Fact
and control cursors, bounded mailbox state, and the Agent tree in one Store
snapshot. An attaching client inspects, reads history at the captured Fact
boundary, then observes with both captured cursors across subsequent Turns.
`observe_interactions` produces one complete initial snapshot of tree approvals
and this root's questions, followed by coalesced scoped changes. Each reconnect
gets a fresh baseline. The immutable snapshot handle retains its share of one
64 MiB application budget until the last clone drops; admission failure is
immediate typed Capacity. Snapshot bounds are independent: 1,024 approvals and
256 questions. Collection reserves before copying broker payloads: at most
16 MiB of approvals, 256 questions of at most 64 KiB, and JSON collection framing.
The stream
subscribes to root/tree membership before validation, then broker changes before
collection. It holds identities and live services, never a Fresh composition pin;
a draft yields an empty baseline and follows its durable publication. No periodic
Header validation or per-Session polling is needed. Approval answers carry an
explicit owning Session identity. The adapter checks its membership in the
current durable tree once and dispatches the exact (Session, approval id) tuple.
Durable Headers and parent links are immutable, and tree membership only grows;
there is no removal or reparenting between validation and answer dispatch.
Identical ids in sibling Sessions remain independent. Question answers return live receipt acceptance, not a durable Tool
result acknowledgment.
Both question operations preserve broker shutdown and capacity as typed Session
errors; malformed or conflicting answers remain invalid operations.

Completed-output reads belong to the Process `ProcessOutputCacheContract`.
Clients inject that read-only capability independently of Session. An output
identity and raw byte cursor suffice; attaching a Session is not a prerequisite
for a read. The cache remains independent of durable Session history.

`SessionReadContract` is a trusted server-side Local capability for finite
workspace reads. It checks a `SessionTarget` against the actual current Header
and returns a non-cloneable `SessionReadLease`. That lease keeps an unpublished
draft active only for the admitted read and exposes the Session service's
retirement signal. Consumers retain it until their finite operation completes;
they never put it in idle file tokens or UI subscriptions. Every new read
reacquires it, so expired drafts and changed Header bindings are rejected before
filesystem work. The lease establishes
current Session correlation and lifetime, not API authentication, model Tool
policy or permission to promote file content into instructions.

`tree_metrics(refresh)` explicitly reads the current Session and up to 255 descendants
without execution. It returns membership completeness, the root membership control
cursor, each member's optional captured Fact watermark and reduced cursor, and
checked totals. Cuts are per member, not simultaneous. `refresh` starts a new
cycle only after completion; an active cycle keeps its frozen roster and cuts.
While `complete` is false, call `tree_metrics(false)` again to advance the same
cycle. `complete: true` covers only the included roster: if `membership_complete`
is false, totals remain partial even after reading finishes. Repeated calls do
not page in omitted descendants, and refresh does not increase the roster limit.
The API operation is version 1 with a 256 KiB reply bound.

`metrics` reads a bounded forward reduction at a fixed Fact watermark. Each
reply reports its cursor and whether that watermark is complete. Repeated reads
continue the current cut before advancing to a newer one. Session totals exclude
child and inherited attempts. This operation neither observes live executions
nor changes Session state. The pure reducer belongs to Conversation; acquisition
and bounded cache policy belong to Session.

## Live Session terminals

A persisted Session may create Linux Bash terminals only under its frozen
ReadOnly or WorkspaceWrite Bubblewrap policy and canonical workspace. Drafts,
unsupported platforms/backends and DangerFullAccess return TerminalUnavailable.
No execution policy is widened or chosen by the client. A service generation
retains at most 256 Session scopes, each with the bounds and controller/receipt
protocol owned by [PTY](../../rsi-pty/README.md). Pane detach and Kernel eviction
leave these scopes alive; explicit close, service retirement and Host stop reap
them. Live IDs do not survive restart.

Requests bind the ordinary Session target and Header fingerprint. Output uses
finite 16 KiB UTF-8 pages on a separate API data operation, independent of
transcript acknowledgement. Terminal control has its own bounded operation.
Input is at most 64 KiB of exact bytes: accepted-prefix receipts permit safe
continuation even if a native write splits a UTF-8 character. Unknown input
receipts require querying the original epoch/sequence; they never permit replay.
