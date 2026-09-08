# rsi-web

The Web coding application runs the same Rust Meta, ordered Profile compiler,
domain clients and Session controllers in a Dedicated Worker. The document
thread renders views and forwards input. JavaScript does not own session
submission, reconciliation, observation cursors, credentials or Profile state.

One authenticated connection supplies independent domain capabilities. The Web
application owns two independently selectable panes; a Session-free Shell owns
their ordinary renderer/controller child Profiles. Each pane has its own draft,
model, transcript, history page and interactions. Replacing one attachment does
not discard its saved input: each pane retains at most 64 Session drafts with
2 MiB of aggregate UTF-8 text, rejecting excess changes before replacing input.
Replacing one attachment does not dispose the other. The Shell permits four surfaces only to allow both old
and replacement generations during simultaneous switches; each pane admits one
switch at a time. The current draft remains registered even when empty, until
attachment replacement succeeds; a failed navigation cannot invalidate its input.
History is a bounded separate page, preserving live observation.

Inputs enter a closed Rust command grammar with non-queued admission. The application
admits at most eight commands globally and one submission awaiting a receipt per
saved Session draft; input documents are limited to 1 MiB plus their small command envelope.
Encoded views use a separate 32 MiB reservation. Submissions use caller identities
and the shared reconciliation owner. Each saved draft retains its immutable
unresolved request across attachment replacement, under a separate 2 MiB
aggregate request-text bound per pane. Send retries resolve that request first;
only an authoritative NotFound permits replay with the same identity and bytes.
Edited draft text remains saved until a subsequent new submission. Unknown
outcomes never allocate replacement identities. Cancellation targets the active Turn and this
pane's accepted pending messages. Question and approval actions preserve exact
request/owner identities. Settings writes preserve the read scope and revision.
The single detail view has its own generation: late settings reads and settled
answers cannot replace or close a newer view. History reads admit one operation
per pane; returning to live view fences a history result still in flight and resets
backward paging to the earliest Fact represented by the current live projection.
Workspace paths describe the selected server, not the browser filesystem.

Each transcript retains at most 128 blocks, 128 KiB per block and 1 MiB of text;
omission is visible. A history page has the same projection bounds. The view
keeps a Tool's name and argument preview when its result arrives, with distinct
intent and result Fact sequences. Arguments occupy at most half the block so a
result still has preview capacity; clipping never removes its exact source.
History beginning after intent displays an explicitly missing intent rather than
inventing a Tool name or arguments. It also marks a page whose durable prefix
was not loaded, including the initial
attachment tail; a page can begin in the middle of streamed model output.
Rendering acknowledges an observation after its bounded Rust projection is retained.
Coalesced view notifications cannot advance or replace domain cursors. The DOM
bridge permits one view delivery at a time, acknowledged after rendering. Whole
valid API Facts and history pages remain possible transient allocations under
their independent domain/transport budgets; UI text limits are not RSS limits.

Login consumes a device registration receipt; the token is exchanged through
the existing same-origin HttpOnly cookie owner and is not persisted in browser
storage. Closing an application drains Worker-owned requests and surfaces and
does not stop the remote service. A WASM trap is a failed Worker, not proof of
clean Rust shutdown. Production uses TLS/H2; loopback HTTP is explicitly for
development transport debugging.
