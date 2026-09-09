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

Limits are declared once in this crate: 128 live bundles, 16 targets, 32 entries
of each contribution type per bundle, 8 concurrent actions per application,
256 elements and 128 KiB encoded bytes per view, and 64 KiB per action input.
Retiring entries keep their admitted action capacity until completion. A
registry generation cannot admit replacement work by forgetting old tasks.

Verification exercises ordinary plugin activation, source reorder, rollback,
stale and foreign references, duplicate/capacity failures, target isolation,
dropped waiters and retirement with blocked mutations. Native and Worker
adapters must use the same public interfaces, with separate visual evidence.
