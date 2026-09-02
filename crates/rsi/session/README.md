# rsi-session

This package owns the standard product's transport-independent deep Session
module. `SessionApplication` creates, attaches, and lists sessions;
`SessionHandle` hides Agent draft tokens, Kernel admission, Store cursors,
workspace registration, routing checks, and approval-control plumbing behind
durable submit, history, observation, cancellation, and approval operations.

Attachment is a durable-data operation: it reads the Store Header and builds a
handle without resolving the current preset generation, Language/Image route,
filesystem state, or Workspace registry. Those execution dependencies are
prepared only by the selected submit operation. Draft creation canonicalizes
the candidate path, rejects an identity already present in the durable Store,
and pins its preset, but does not register the Workspace or require the default
Language route. Text submission validates its effective
Language route and registers the canonical Workspace after resume preparation;
direct Image submission validates only its Image route.

`LocalSessionApplication` is the process-local adapter. The sibling
`rsi-session-host` package provides the Unix-domain adapter; both pass the same
transport-independent operation and durable-result conformance suite. The Host
transport additionally owns cross-connection unpublished-draft admission,
identity uniqueness, and idle reclamation. Those transport resource policies
are not promises made by the process-local adapter. A successful submit is a
durable receipt. The caller supplies `TurnId`; retrying the same canonical
Header and request is idempotent, while reusing the identity for a different
request is a conflict.

History is bounded. A missing backward cursor selects one page ending at the
durable tail, and returned Facts are always ascending. Durable Facts are the
historical authority; subscriptions and approvals are live and detachable.
