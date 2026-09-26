# rsi-goal

Control DTOs and observation contracts are portable. The controller factory and
driver compile only for native Hosts; browser clients use Session API and do
not create a local scheduling owner.

The Host-generation Goal controller owns a bounded set of at most 64 live
Session drivers, with one lease per Session. GUI detach leaves that owner
running. Host withdrawal revokes leases, discards pending continuation input,
cancels and joins drivers. Startup is disarmed; only an explicit successful
create/resume action may arm after canonical command-receipt reconciliation.

Each round atomically reserves by an internal command/CAS and accepts the exact
frozen message through idle Kernel admission. Busy preserves the live owner and
waits for Session changes without charging an allocation. A draft first round
computes a private candidate baseline and publishes it only with acceptance. Claim uses the existing atomic
activation/Turn/Step/input path. Neither receipt reads nor uncertain outcomes
start a new round. A Store failure disarms and retains the original reservation
identity; the controller does not fabricate a persisted blocked state.

Pause revokes before pending-only discard and lets an already claimed Turn
finish. Cancel also targets the exact automatic message or claimed Turn.
Ordinary waking input has priority at acceptance and claim. Driver state is
separate from the pure durable Goal projection and is explicitly unavailable
after its owning Host generation ends.

`GoalSession` is the standard Session owner's narrow bridge: it serializes draft
freeze/publication with other draft mutations, prepares Workspace access through
the existing Session path, and supplies canonical domain/message/Turn reads.
The controller holds that bridge during execution. It never imports the standard
Session implementation, and the Agent Goal plugin never imports this controller.
Application controls carry an immutable request identity and expected revision.
After control admission, shutdown, panic or timeout can leave its command outcome
unknown; callers retain that original identity and reconcile its receipt.
Only a successful create/resume receipt matching the latest captured revision
can arm a new owner; replaying an old receipt after later state changes cannot.

Pause/cancel revoke scheduling before waiting for the per-Session control gate.
A pending-only discard or a claimed Turn's natural terminal outcome is settled
through the retained revoked lease. An allocated input that was never accepted
stays unresolved on Pause with its original identity; explicit Resume can
reconcile and submit that allocation. Explicit Cancel abandons it without
provider execution, permitting replacement of the stopped Goal. Outcome reads
do not fabricate a discard, abandonment or refund.
A draft published by human input still has zero automatic allocations. Its first
automatic round uses the ordinary durable reserve transaction once idle.
