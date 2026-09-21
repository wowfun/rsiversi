# rsi-workbench-ui

Navigation observes the bounded attention API with at most one poll or explicit
navigation operation in flight. An unchanged page backs off from one to eight
seconds; a local invalidation or changed page resets that delay. A failed refresh
retains the last page with an explicit stale-state notice. Explicit reading
acknowledgments are retained operations and never retried after uncertain results.

Saving a model requires a string deployment identity in its provider definition
that matches the selected model. Missing, null or mismatched identities are
rejected before provider writes; incomplete entries cannot match one another by
their absent JSON fields.

Setup exposes typed snapshots to native consumers and the same redacted JSON
projection to GUI consumers. Discovery candidates are ephemeral and never imply
provider application or default selection. Each retained write keeps its own
receipt; cancelled presentation does not undo an admitted credential write.

Ordinary Application plugins own configuration and navigation presentation state
over the connected Host's typed APIs. They contribute application-scoped status
surfaces through the existing UI registry. Their closed command handles support
the first-party workbench forms without adding Host policy to JavaScript or the
generic GUI transport. The credential form uses a separate bounded secret command;
the shared UI view contract deliberately contains no secret input primitive.

Configuration retains no secret in a view, receipt, draft or Profile. The client
keeps credential writes, provider application and default selection independent.
Settings selections use the exact snapshot retained under a fresh view ticket;
stale forms fail before mutation. Configuration read failures do not discard a
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

Host leaf management is a distinct panel in this feature, using the reviewed
`profile-leaves` API. It retains one bounded source page, four redacted proposals,
256 receipt identities and one selected receipt. Local grant editing is explicit;
Device and Agent scopes remain separate. Configuration text is parsed as exact
bounded JSON in Rust and never enters a retained view. A commit accepts the
displayed ticket/digest, records unknown replies without replay, and offers a
query of that original ticket. Reconnection may recover owned proposals and
receipt identities. Source save, directory durability and runtime application
are displayed separately; old resident Sessions are not edited.

Navigation only acknowledges coalesced invalidations when it will query the
current filter; an attention-only timer tick cannot consume a later publication.
