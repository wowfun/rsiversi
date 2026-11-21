# rsi-workbench-ui

Discovery is advisory: a failed catalog refresh preserves previous capability
hints and does not prevent authoritative reads. Selecting a plugin page reuses
those hints; explicit Refresh updates them. Call admission remains authoritative.

Plugin observations remain available in read-only catalogs. Their `actions` list
advertises each exact optional command separately; renderers disable missing
actions without hiding the corresponding observations. Refresh reads current
registrations, including replacements in the same Host generation.

Plugin status guidance includes the last completed Host update separately from current health; its observation revision fences pagination. Polling and Busy admission do not manufacture completed attempts.
Plugin snapshot capture pairs both owner revisions with the view under the state-then-capabilities lock order. An unchanged pair skips view cloning and GUI serialization.
Transient discovery failures preserve the last successful capability catalog.
An initial discovery failure publishes an unavailable feature with a diagnostic;
an explicit refresh can recover it without reactivating the application.

An automatic full navigation refresh already supplies the newest cursor and skips
the redundant head comparison query in that round.

Navigation subscribes to the authenticated Host's attention page. The first
frame establishes the visible baseline. Later frames replace it when the
caller's visible activity changes. Explicit reading actions use their displayed
Host and source cut once. A successful mark changes the projected page on its
subscription frame. A disconnected observation retains the last page
with a stale-state notice until reconnection. A missing or retired operation,
incompatible protocol, or loss of authorization clears the page.

Saving a model requires a string deployment identity in its provider definition
that matches the selected model. Missing, null or mismatched identities are
rejected before provider writes; incomplete entries cannot match one another by
their absent JSON fields.

Setup persists confirmed composer selections in order when the Host grants
Settings replacement. A selection updates the model and effort together and
preserves other devices' unrelated agent settings. The composer checks apparent
no-ops against the Host. A rejected stale revision can be rebased on the same
registration; an uncertain write must be reconciled explicitly. Setup forms
remain bound to the revision the operator reviewed. The confirmed document and the
selected model's description supply the new default to starting composers.
A failed default write leaves the accepted Session selection usable.

Setup exposes typed snapshots to native consumers and the same redacted JSON
projection to GUI consumers. Discovery candidates are ephemeral and never imply
provider application or default selection. Each retained write keeps its own
receipt; cancelled presentation does not undo an admitted credential write.
Setup refresh also reads the exact configured default model's committed
description through the negotiated read-only Models API. The description is
published with the settings snapshot before a Session exists. Missing capability
or model clears it; a failed description read clears stale metadata while
retaining the freshly read settings. Description metadata is advisory: its failure
does not fail Setup refresh or prevent repairing a missing default model.
Credential status reads update only the Setup projection. The provider editor
coalesces slot edits before checking status; these reads do not refresh workspace,
conversation or model catalogs.
The editor retains at most one status read, cancels a pending check when saving,
and awaits an already admitted read before starting credential/provider writes.

Ordinary Application plugins own configuration and navigation presentation state
over the connected Host's typed APIs. They contribute application-scoped status
surfaces through the existing UI registry. Their closed command handles support
the first-party workbench forms without adding Host policy to JavaScript or the
generic GUI transport. The credential form uses a separate bounded secret command;
the shared UI view contract deliberately contains no secret input primitive.

Configuration retains no secret in a view, receipt, draft or Profile. The client
keeps credential writes, provider application and default selection independent.
Setup retains each settings document once with its namespace version. A selection
updates fields in the displayed document and submits that version for comparison.
Stale form tickets fail before mutation. Configuration read failures do not discard a
previous successful write receipt. Lost/unknown mutation replies require an
explicit refresh; no write is replayed.
Both setup commands and model saves publish their final failure in the typed view.
A provider write that did not apply its routes retains the Host's convergence
diagnostic, including restart requirements, separately from its confirmed write
receipt and from default selection.

Navigation owns query tickets and continuation cursors in Rust. One read round
scans at most 4,096 rows through the Host's bounded pages, stopping at a nonempty
page or exhaustion. The document receives exact SessionIds, grouped WorkspaceIds,
titles and continuation availability, never an editable Store cursor. Changing
query or selection tickets rejects stale controls.

A confirmed first durable message invalidates navigation. One coalesced read
worker refreshes the current filter after any admitted navigation command settles;
it does not replay a write, change the selected Session or manufacture a row from
document state. Retirement cancels this read worker before draining commands.

Each plugin admits one non-queued command and retains its execution independently
of the caller's waiter. Retirement closes admission, drains work and withdraws
its UI contributions. Service configuration authorization remains at the Host's
trusted-origin boundary. Local UI availability is not an authorization proof.

The capacity snapshot in `model_metadata.rs` records official sources and the
verification date. It matches exact provider, official API root (including the
OpenAI `/v1` base alias) and model ID;
custom endpoints receive no official fallback. Existing configured limits win,
then valid online metadata, then missing snapshot fields. Unknown capacities
remain explicit user inputs. Output reserve starts at `min(4096, maximum)` and
all saved limits pass `LanguageModelLimits`. Rounded DeepSeek capacities use
conservative decimal token counts.

Plugin status is an independent ordinary feature over the grant-gated Configuration
read. It retains one 32-row page, explicit refresh and exact-ticket adjacent-page
commands. Desired and observed revision changes invalidate pagination; failed reads
clear the old page. Both TUI `/plugins` and GUI Settings → Plugins consume this
owner, whose admitted reads drain before feature retirement.

Host leaf management is a distinct panel using the reviewed
[`profile-leaves` API](../configuration-api/README.md). Local grant editing is
explicit and permits Local or Device principals; Agent tools use their own
per-call authority. Configuration text is parsed as exact bounded JSON in Rust.
Host Profile review retains one private client-side change until it is committed,
dismissed or refreshed; the document receives only redacted metadata. Refresh
makes one catalog call, without recovering server previews or receipts. Commit
consumes the review before sending and displays the direct result. An unknown
reply is not retried automatically; current state is observation, not proof of
that original write. The configuration API owns server admission and publication.
Source save, directory durability and runtime at completion are displayed
separately. Reconnection reads current source choices without recovering old
results.

Navigation only acknowledges coalesced invalidations when it will query the
current filter; an idle navigation timer tick cannot consume a later publication.

Navigation keeps at most 16 independently expanded group pages, each with at most
64 entries and its own opaque ticket/cursor. Group continuation replaces that
bounded page; the view identifies continued pages so the document does not imply
that earlier results are still present. Search/filter replacement releases group
pages. Pinned rows are a separate complete bounded query.

Unchanged metadata refreshes preserve continued workspace pages and their tickets.
Newest pages may refresh to include newly durable Sessions; identical results
keep their tickets. A metadata revision change retires old cursors and reloads
the first page of each open, uncontinued group. A continued or previously failed
group keeps its explicit refresh control so a change cannot silently reset its
reading position. Its invalidated rows and mutation controls remain unavailable.

Navigation replacements carry the metadata revision displayed when the edit
began. Both the view owner and Store reject stale revisions; a rename cannot
overwrite a pin or archive change that arrived while its form was open.
For a visible row in complete, unsearched activity pages, a validated title or
archive receipt updates the row in place or removes it from pages whose archive
filter excludes it. These edits preserve activity order. The owner advances the
metadata revision and renews page tickets. Changes in pin membership, searches,
incomplete pages and manual order use a fresh read.

A confirmed navigation replacement remains successful if its follow-up reads fail
or race another revision. The view explicitly reports that the change was saved
but refresh is unavailable, clears stale results and invalidates old cursors.
Group refreshes run with at most four reads in flight. With unchanged metadata,
the last validated page remains usable during a read; identical results preserve
its ticket. A failed read invalidates only that group, while others can finish.
No mutation is retried to repair a failed refresh.

Automatic navigation catalogue reads have a five-second deadline and yield
to an admitted user action: cancellation releases their serialized read owner
before that action runs. An unchanged attention page or repeated failure leaves
the presentation revision intact. Background reads do not occupy the user-operation slot.
Cancelled refresh notifications remain pending. User mutations retain normal
owned-work semantics and are never cancelled or replayed by this preemption.

Manual navigation retains one complete coordinate-scoped order seed under a fresh
view ticket. A later summary command may request only 64 distinct members of that
seed and retains their requested order. Document-side persistence owns the device
order; Rust owns seed validity, metadata revision, exact coordinates and summary
publication. A metadata change discards the presentation seed, never the device's
saved order. Activity changes leave the seed valid. A background head observation
updates the first activity page and its open groups when the newest activity or
metadata revision changes. Readers of a continued page keep their controls
and can explicitly refresh.

The SSH panel presents helper-unavailable rebuild guidance through its normal
command error state, including the optional Linux TUI helper and restart step.
The SSH panel retains only the bounded caller-visible target catalog, one resolved
target directory and a safe outcome. Candidate edits, trust and connection controls
use the exact displayed Host, target revision and connection epoch. Trust is offered
only when the Local-only operation was negotiated. Authentication paths are ephemeral
commands and never retained in views. An uncertain mutation blocks further writes
until an explicit catalog read; no reconnect or trust mutation is retried.

SSH MCP administration selects one exact target/server and reads its current
configuration revision before any change. The workbench retains one authorized
configuration observation, with explicit save, remove and connect actions. It
never resolves or displays credential values. Saved configuration and connection
application have separate outcomes; a committed save with apply failure remains
committed. Unknown outcomes clear the observation and fence mutations until an
explicit read. Selecting another server clears its editor binding. Exact stdio
management and Use remain independently enforced by the Service API.

PresentationState publishes a private content revision synchronously under its
state lock when mutable access begins. Read-only snapshots preserve the revision;
conservative invalidation is allowed. Feature content revisions include capability
state and are independent of asynchronous work notifications. Composite revision
and snapshot reads hold state and capability locks together, in that order, to
observe one coherent pair. They identify cached
GUI projections, never user action authority or durable metadata versions.

Setup and Navigation cached snapshots capture their revision and projection under
one owner lock, and skip projection when that revision is unchanged.
