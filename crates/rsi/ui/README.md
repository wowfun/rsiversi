# rsi-ui

`rsi-ui` is an ordinary Meta plugin shared by native and Worker applications.
It owns bounded surface, action and block-renderer registrations; it owns no
Session, Shell, layout, DOM, terminal or second Runtime. Applications compose
one registry and explicitly register each actual application or Shell surface
Context as a target. A target plugin publishes only `UiTargetContract`; the
Shell continues to expose Local capabilities without exporting Context control.

A registration belongs to the exact contributing Meta generation. Membership
uses Meta registration admission and declaration-order snapshots, including
live reorder. A contribution bundle declares surfaces, named actions and block
renderers. Each surface and renderer chooses application or surface targets.
Callbacks run outside registry locks. Renderers borrow bounded presentation
input; neither the registry nor its views retain Facts or observation leases.

Action references bind the fresh application nonce, contribution registration,
target generation and exact action name. They are opaque strings on the document
wire. Rust verifies all four and bounds payloads before invoking plugin code;
the handler validates its business payload and acquires the Local capabilities
it needs before I/O. Retiring Meta Contexts reject new lookups; an admitted
operation retains its already acquired typed values. The target is the real
Meta Context with its existing Local mappings, not an application-supplied
Session name. An action cannot retarget a stale reference to a replacement pane.
These staleness identities are not authentication credentials.

Admission is nonqueued and occurs before returning a waiter. Dropping a waiter
never drops admitted work. Both the contribution and target own tracking for
that work. Withdrawal closes new admission and signals cooperative cancellation;
cleanup drains admitted actions before releasing their owner. A separately supplied
presentation cancellation token lets read actions cancel on detail close; it never
automatically drops an admitted mutation. Handlers retain
responsibility for mutations already dispatched and must cooperate with owner
retirement. Meta's existing cleanup deadline and failure reporting remain
applicable. Explicit lease disposal joins the same work; dropping a lease
withdraws immediately and leaves final draining with its Meta effect owner.

Views contain closed text, code, field, input and button elements. Buttons name
only actions in their own bundle, and Rust supplies their bound references.
Before publication, form defaults together with each button payload must fit
the action input envelope, including JSON escaping.
Applications render cards, menus, details and forms from this data; they own
focus, editing, sanitization and layout. UI fields are non-secret. Linked
callbacks are trusted application code; no dynamic frontend script admission
or DOM isolation is implied. Text never becomes HTML or an execution entry.

This registry owns limits of 128 live bundles, 16 targets, 32 entries of each
contribution type per bundle and 8 concurrent actions per application.
[rsi-ui-protocol](../ui-protocol/README.md) owns view and action-input wire limits.
Retiring entries keep their admitted action capacity until completion. A
registry generation cannot admit replacement work by forgetting old tasks.

`PresentationLease` owns asynchronous model refresh for one exact surface. Its
background reads capture the displayed revision and action admission epoch.
An admitted action fences older reads before its handler starts; a background
result or diagnostic cannot publish during an action or after that ticket changes.
Concurrent actions retain their displayed predecessor: a reply conflict after
execution is an unknown outcome, never permission to replay the mutation.
Invalidations coalesce while work runs. Failed actions request a trailing refresh;
successful actions keep their returned model unless data was explicitly invalidated.
Candidates are validated and encoded outside the publication lock; that lock only
checks freshness and installs a complete immutable snapshot.

`Ui::membership_changes` reports registration changes to menu/catalog consumers.
Data refresh belongs to the exact contribution/target registration lease or
presentation lease: their `invalidate` methods reject retired owners. Each
presentation watches only its contribution, target and own coalesced invalidation.

Each source runs after reserving one of 32 snapshot slots and up to 128 KiB from the
application's shared 4 MiB encoded snapshot pool. At most 16 presentations run.
Current, candidate and escaped reader pins share these limits. A synchronous
snapshot lookup reads already materialized data and never calls domain code.
Failed refresh preserves the current snapshot and publishes one bounded diagnostic.
Before the first snapshot, `ready()` preserves Invalid, Retired and Capacity
classification; contribution and infrastructure failures remain diagnostic handler
failures. A materialization rejection is not evidence that an action was invoked.
Capacity-blocked refresh resumes when retained snapshot or source bytes release
admission, without requiring a domain invalidation. Once an action handler has
started, any failure to produce or publish its reply has an unknown outcome;
only failures before dispatch can report that the action was not admitted.
Closing a lease or retiring its target/contribution cancels reads and drains its
owned work before releasing the presentation slot.

A source may bind one ordinary presentation child scope before its first model.
The source owns cancellation-safe startup; the presentation worker requests
cancellation and joins startup instead of releasing its slot while a candidate
is still activating. Models, actions and source reads use that scope's Context.
Closing drains admitted actions, then joins the binding owner and reports cleanup
failure. The binding cannot turn a presentation ID into domain authority; products
must explicitly supply already narrowed Local facets to the child scope.
`UiBusinessApiContract` is that explicit target facet: a semantic scope and a
domain-owned restricted ApiClient. Adapters never substitute an ambient global
ApiClient when the target facet is absent.

The [neutral protocol](../ui-protocol/README.md) gives a model an explicit schema,
renderer, displayed actions and sources. Presentation invocation checks the exact
epoch, current revision and displayed membership before admitting the existing
owned action task. Neither possession of a bundle name nor construction of a
`UiReference` grants a presentation action. The source handler interprets only
names published in the same snapshot. These local capabilities are distinct from
remote authentication and one-time input admission.

Verification exercises ordinary plugin activation, source reorder, rollback,
stale and foreign references, duplicate/capacity failures, target isolation,
dropped waiters and retirement with blocked mutations. Native and Worker
adapters must use the same public interfaces, with separate visual evidence.
