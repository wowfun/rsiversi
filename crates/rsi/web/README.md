# rsi-web

Each pane displays the latest complete extension-state snapshot, including fresh
drafts and idle control changes without a model request. Producer values and
failures are rendered as text. Snapshot replacement releases the prior retention;
attachment withdrawal releases the latest value. Projection-stream failure leaves
core history observation independent and marks extension state unavailable.

Each conversation exposes its discovered Session commands and saved command
receipt. Registered slash names execute through the shared controller; unknown
names remain Human input for workspace skills. One unresolved invocation per
saved Session retains its full arguments, original request ID and predecessor
across pane replacement. Refresh only queries that identity. Sending a new
message waits until the pending command is resolved; edited input remains saved.
Command state uses the existing 64 saved-Session bound (at most 16 KiB of arguments
per Session) independently of message-text retention.

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

Tool argument and result details use closed exact Fact sources, with decimal
string sequences preserved opaquely in JavaScript. Rust reads one exact Fact
through the shared controller and retains a 64 KiB UTF-8 field window. Paging
requires the current detail ticket; stale buttons cannot replace a newer view.
Closing or replacing details and replacing their pane cancel the old read.
Both success and failure are fenced by pane and detail generation; unavailable
sources are reported within their own detail. No full Fact or observation lease
is retained by a detail, and detail cancellation does not cancel the Turn.

Each transcript retains at most 128 blocks, 128 KiB per block, 1 MiB of owned
text capacity and 512 KiB of metadata capacity;
omission is visible. A history page has the same projection bounds. The view
uses shared [conversation semantics](../conversation/README.md) for Tool outcome
classification and bounded JSON previews; it does not serialize complete large
arguments or result values before truncating them. The view keeps a Tool's name and argument preview when its result arrives, with distinct
intent and result Fact sequences. Arguments and results each occupy at most half the block; this also reserves
room for an intent loaded after its result. Clipping never removes its exact source.
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
