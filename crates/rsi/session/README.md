# rsi-session

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
Meta. All clients of that generation receive the same service. The standard
Host's approval broker is also an ordinary plugin; the launcher neither
constructs Session adapters nor registers a second broker per client.

`LocalSessionService` is constructed from already-owned capabilities. Draft
creation resolves a registered WorkspaceId, freezes workspace trust and settings,
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

The same service generation also publishes `SessionReadContract`. A finite read
acquires the existing draft activity owner, reconciles against the Store and
compares the current Header under that activity. Durable reads need no Agent pin;
expired drafts are rejected and service retirement cancels leases. The owning
API establishes authentication separately, then retains this lease through the
read. File tokens hold only correlation data and filesystem resources.
