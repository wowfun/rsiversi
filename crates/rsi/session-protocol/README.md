# rsi-session-protocol

This library owns the standard product's transport-independent Session
contracts and bounded retained observation values. `SessionService` creates, attaches, and lists sessions;
`SessionHandle` hides Agent draft tokens, Kernel admission, Store cursors,
routing checks and approval-control
plumbing behind durable message submission, history, observation,
cancellation, direct image generation, and approval operations.

Creation, input, receipt and page values have closed Serde representations.
They describe domain API values, not a new durable Session or Store format.

API-backed capabilities retain common transport failures in `SessionError::Api`.
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
that exact registration into the existing canonical-cwd Header, freezes the
explicit workspace-trust decision, rejects an identity already present in the
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
