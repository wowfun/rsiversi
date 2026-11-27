# rsi-workspace-review-api

Review scopes use `WorkspaceKey` for complete execution coordinates, independently
of the current Workspace registration identity. The owner resolves and authorizes
the current registration before reviewing its source.

Authenticated clients list interval summaries for one exact native or observed
conversation in a registered workspace, then read one file diff from that interval.
The owner reauthorizes workspace and source for the actual API origin on every
request, retaining current location admission through the finite worker. Offline
source metadata may be read with Use; a live connection is unnecessary for Local
scratch or live summaries. Lists return at most
32 summaries and 1 MiB; file pages at most 64 entries and 256 KiB. Diff pages carry
at most 64 KiB of text and explicit byte cursors. IDs and cursors are canonical
strings, not JavaScript numbers. A requested comparison whose scratch has been released returns
`Expired`. A running tool can expose a pending review. A completed capture
appears in the list while its captured changes are available, with any known omissions.
Reads confer no execution, rollback, staging or restore authority.
