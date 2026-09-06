# rsi-session

This package owns the standard product's transport-independent deep Session
module. `SessionApplication` creates, attaches, and lists sessions;
`SessionHandle` hides Agent draft tokens, Kernel admission, Store cursors,
workspace registration, media import, routing checks, and approval-control
plumbing behind durable message submission, history, observation,
cancellation, direct image generation, and approval operations.

Attachment is a durable-data operation: it reads the Store Header and builds a
handle without resolving the current preset generation, Language/Image route,
filesystem state, or Workspace registry. Those execution dependencies are
prepared only by the selected operation. Draft creation canonicalizes the
candidate path, freezes the explicit workspace-trust decision, rejects an
identity already present in the durable Store, and pins its preset, but does
not register the Workspace or require the default Language route.

Message submission validates its effective Language route, imports each
bounded image through Media, and registers the canonical Workspace after
resume preparation. It durably returns a Message receipt, not a speculative
Turn identity: claim creates the Turn and Step later. Direct Image generation
is a separate operation and validates only its Image route. A fresh handle
serializes its first durable publication so exactly one generation pin wins;
once attached, independent submissions release the handle state lock before
resume preparation and backend I/O.

`LocalSessionApplication` is the process-local adapter. The sibling
`rsi-session-host` package provides the Unix-domain adapter; both pass the same
transport-independent operation and durable-result conformance suite. The Host
transport additionally owns cross-connection unpublished-draft admission,
identity uniqueness, idle reclamation, and bounded upload framing. Those
transport resource policies are not promises made by the process-local
adapter.

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
Pending questions and approvals are separate live snapshots refreshed on
reconnect. Question answers return live receipt acceptance, not a durable Tool
result acknowledgment. Output reads accept only completed-cache identities and
raw byte cursors through a separate read-only capability.
Both question operations preserve broker shutdown and capacity as typed Session
errors; malformed or conflicting answers remain invalid operations.

Completed-output identities name the Host's Process cache, independently of
Session history. A Session handle supplies read-only access to that same Host
capability; the cache is not partitioned by Session or presented as a durable
Session archive.
