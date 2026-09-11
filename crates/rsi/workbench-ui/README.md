# rsi-workbench-ui

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
